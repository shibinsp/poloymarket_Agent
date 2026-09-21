//! Web dashboard — axum HTTP server serving REST API + embedded HTML.
//!
//! When `DASHBOARD_TOKEN` is configured, every `/api/*` route except
//! `/api/health` (kept open for uptime probes) requires
//! `Authorization: Bearer <token>`. Binding to a non-loopback address without
//! a token is refused in live mode and downgraded to loopback otherwise.

use std::net::IpAddr;
use std::sync::Arc;

use anyhow::{bail, Result};
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use rust_decimal::Decimal;
use subtle::ConstantTimeEq;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::agent::kill_switch::{Halt, HaltSource, KillSwitch};
use crate::config::AgentMode;
use crate::db::store::Store;
use crate::monitoring::health::HealthState;
use crate::monitoring::metrics::compute_metrics;
use crate::risk::circuit_breaker::HaltScope;

const LOOPBACK: &str = "127.0.0.1";

/// Shared state accessible by all dashboard route handlers.
#[derive(Clone)]
pub struct DashboardState {
    store: Arc<Store>,
    health: HealthState,
    /// The limits the breakers enforce, so the risk page can show headroom
    /// rather than only reporting a trip after the fact.
    risk: Arc<crate::config::RiskConfig>,
    mode: AgentMode,
    initial_bankroll: Decimal,
    api_token: Option<Arc<str>>,
    /// Shared with the agent loop. The dashboard only ever sets or clears
    /// this flag; the loop is what acts on it — cancelling resting orders,
    /// alerting, and writing the row that survives a restart. Doing that work
    /// from an HTTP handler would mean a halt whose side effects depend on
    /// which route raised it.
    kill_switch: Arc<KillSwitch>,
}

impl DashboardState {
    pub fn new(
        store: Store,
        health: HealthState,
        initial_bankroll: Decimal,
        api_token: Option<String>,
        kill_switch: Arc<KillSwitch>,
        risk: crate::config::RiskConfig,
        mode: AgentMode,
    ) -> Self {
        Self {
            store: Arc::new(store),
            health,
            initial_bankroll,
            kill_switch,
            risk: Arc::new(risk),
            mode,
            // Trim before storing: the page sends a trimmed token, so keeping
            // stray whitespace here would 401 every request with an
            // apparently-correct token.
            api_token: api_token
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .map(|t| Arc::from(t.as_str())),
        }
    }
}

/// Spawn the dashboard HTTP server. Returns a handle that can be aborted.
pub fn spawn_dashboard(
    state: DashboardState,
    bind: &str,
    port: u16,
    mode: AgentMode,
) -> Result<JoinHandle<()>> {
    let bind = resolve_bind(bind, state.api_token.is_some(), mode)?;
    let addr = format!("{bind}:{port}");
    let app = build_router(state);

    Ok(tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => {
                info!(addr = %addr, "Dashboard server listening");
                l
            }
            Err(e) => {
                warn!(error = %e, addr = %addr, "Failed to bind dashboard server");
                return;
            }
        };

        if let Err(e) = axum::serve(listener, app).await {
            warn!(error = %e, "Dashboard server error");
        }
    }))
}

/// A non-loopback bind with no token would expose every trade to the network:
/// refuse it in live mode, fall back to loopback otherwise.
fn resolve_bind(bind: &str, has_token: bool, mode: AgentMode) -> Result<String> {
    if is_loopback(bind) || has_token {
        return Ok(bind.to_string());
    }
    if mode == AgentMode::Live {
        bail!(
            "Refusing to bind the dashboard to {bind} without DASHBOARD_TOKEN in live mode — \
             every /api/* route would be world-readable"
        );
    }
    warn!(
        bind,
        "dashboard_bind is not loopback and DASHBOARD_TOKEN is unset — falling back to 127.0.0.1"
    );
    Ok(LOOPBACK.to_string())
}

fn is_loopback(bind: &str) -> bool {
    bind.eq_ignore_ascii_case("localhost")
        || bind
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

fn build_router(state: DashboardState) -> Router {
    let protected = Router::new()
        .route("/api/metrics", get(metrics_handler))
        .route("/api/trades", get(trades_handler))
        .route("/api/trades/all", get(trades_all_handler))
        .route("/api/cycles", get(cycles_latest_handler))
        .route("/api/cycles/all", get(cycles_all_handler))
        .route("/api/costs", get(costs_handler))
        .route("/api/orders", get(orders_handler))
        .route("/api/fills", get(fills_handler))
        .route("/api/equity", get(equity_handler))
        .route("/api/reconciliation", get(reconciliation_handler))
        .route("/api/risk", get(risk_handler))
        .route("/api/halt", post(halt_handler))
        .route("/api/halt", get(halt_status_handler))
        .route("/api/resume", post(resume_handler))
        // The plan's name for the same action. A reconciliation mismatch is
        // cleared by acknowledging it, which is a resume.
        .route("/api/reconcile/ack", post(resume_handler))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));

    Router::new()
        .route("/", get(index_handler))
        .route("/api/health", get(health_handler))
        .merge(protected)
        .with_state(state)
}

async fn require_token(State(state): State<DashboardState>, req: Request, next: Next) -> Response {
    let Some(expected) = state.api_token.as_deref() else {
        return next.run(req).await;
    };

    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let authorized = provided
        .map(|p| bool::from(p.as_bytes().ct_eq(expected.as_bytes())))
        .unwrap_or(false);

    if authorized {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "unauthorized",
        )
            .into_response()
    }
}

// -- Route Handlers --

async fn index_handler() -> impl IntoResponse {
    let html = include_str!("../../static/index.html");
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}

/// The execution record: what was asked for, and what came back.
async fn orders_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_orders(500).await {
        Ok(rows) => Json(serde_json::to_value(&rows).unwrap_or_default()),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

/// Individual executions — where slippage and time-to-fill live.
async fn fills_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_fills(500).await {
        Ok(rows) => Json(serde_json::to_value(&rows).unwrap_or_default()),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

/// Start-of-day equity and the running peak — the drawdown series.
async fn equity_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_daily_equity().await {
        Ok(rows) => Json(serde_json::to_value(&rows).unwrap_or_default()),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

async fn reconciliation_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_reconciliation_runs(200).await {
        Ok(rows) => Json(serde_json::to_value(&rows).unwrap_or_default()),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

/// How much headroom is left under each circuit breaker.
///
/// The limits live in a config file and the trips land in the logs; neither
/// answers the question an operator actually has, which is *how close am I*.
/// Reported as raw numbers rather than a single percentage so the page can
/// show both the limit and the distance to it.
async fn risk_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let today = chrono::Utc::now().date_naive();
    let cfg = &state.risk;

    // Read the marks; never write them. Recording equity from a GET would let
    // a page refresh move the day's opening figure, which is the number every
    // daily-loss calculation is measured against.
    let equity = state.store.get_daily_equity().await.unwrap_or_default();
    let today_row = equity.iter().find(|r| r.day == today.to_string());
    let peak = equity
        .iter()
        .filter_map(|r| Decimal::from_str_exact(&r.high_water_mark).ok())
        .max();

    let trades_today = state.store.count_trades_opened_on(today).await.unwrap_or(0);
    let losses = state.store.consecutive_losses().await.unwrap_or(0);

    Json(serde_json::json!({
        "mode": format!("{:?}", state.mode).to_lowercase(),
        "day": today.to_string(),
        "starting_equity": today_row.map(|r| r.starting_equity.clone()),
        "current_equity": today_row.and_then(|r| r.closing_equity.clone()),
        "high_water_mark": peak.map(|p| p.to_string()),
        "trades_today": trades_today,
        "consecutive_losses": losses,
        "limits": {
            "max_daily_loss_pct": cfg.max_daily_loss_pct.to_string(),
            "max_daily_loss_usd": cfg.max_daily_loss_usd.to_string(),
            "max_drawdown_pct": cfg.max_drawdown_pct.to_string(),
            "max_trades_per_day": cfg.max_trades_per_day,
            "max_consecutive_losses": cfg.max_consecutive_losses,
            "max_live_notional_per_position_usd":
                cfg.max_live_notional_per_position_usd.to_string(),
            "max_live_total_notional_usd": cfg.max_live_total_notional_usd.to_string(),
            // The absolute caps only bind with real money on the line.
            "live_caps_apply": state.mode == AgentMode::Live,
        },
    }))
}

/// Stop opening positions.
///
/// Sets the flag and returns. The agent loop notices on its next wake and
/// does the work — cancels resting orders, alerts, writes the row that
/// survives a restart. So a 200 here means "the halt is recorded", not "every
/// order is already cancelled"; the response says which, because the
/// difference matters to whoever is pressing the button.
async fn halt_handler(
    State(state): State<DashboardState>,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    let detail = body
        .and_then(|Json(v)| {
            v.get("reason")
                .and_then(|r| r.as_str())
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "halted from the dashboard".to_string());

    let now = chrono::Utc::now();
    // Raised UntilResume: an operator pressing stop does not mean "until
    // midnight".
    let newly = state.kill_switch.trip(Halt::new(
        HaltSource::Api,
        HaltScope::UntilResume,
        detail,
        now,
    ));

    warn!(newly, "Halt requested over the API");
    Json(serde_json::json!({
        "halted": true,
        "newly_halted": newly,
        "note": if newly {
            "Resting orders are cancelled by the agent on its next wake."
        } else {
            "Already halted; the original reason is kept."
        },
        "halt": state.kill_switch.current(),
    }))
}

/// What is currently stopping the agent, if anything.
async fn halt_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "halted": state.kill_switch.is_tripped(),
        "halt": state.kill_switch.current(),
    }))
}

/// Lift the halt and let the agent open positions again.
///
/// Also removes the `HALT` file, so a resume from here is not silently undone
/// by the next poll of it.
async fn resume_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let cleared = match state.kill_switch.clear() {
        Ok(c) => c,
        Err(e) => {
            // The flag is down but the file is still there, so the next wake
            // will halt again. Saying "resumed" would be a lie.
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "halted": true,
                    "error": format!(
                        "could not remove {}: {e} — the agent will halt again on its next wake",
                        state.kill_switch.halt_file().display()
                    ),
                })),
            )
                .into_response();
        }
    };

    if let Err(e) = state.store.clear_halts("api", chrono::Utc::now()).await {
        // Worth a 500: the flag is down, so the agent resumes now, but the
        // uncleared row would halt it again on the next restart. An operator
        // who thinks they have resumed and has not is the failure here.
        warn!(error = %e, "Could not clear the halt rows");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "halted": false,
                "error": format!(
                    "resumed, but the halt record could not be cleared ({e}) — a restart would halt again"
                ),
            })),
        )
            .into_response();
    }

    info!(?cleared, "Resumed over the API");
    Json(serde_json::json!({
        "halted": false,
        "cleared": cleared,
    }))
    .into_response()
}

async fn health_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let mut data = state.health.to_json().await;
    // Read the flag now rather than serving whatever the last completed cycle
    // published. A cycle can run for minutes, and a halt raised inside that
    // window is exactly the one an operator is refreshing this page to see.
    if let Some(obj) = data.as_object_mut() {
        let halt = state.kill_switch.current();
        obj.insert(
            "halted".to_string(),
            serde_json::json!(state.kill_switch.is_tripped()),
        );
        obj.insert(
            "halt".to_string(),
            halt.and_then(|h| serde_json::to_value(h).ok())
                .unwrap_or(serde_json::Value::Null),
        );
    }
    Json(data)
}

async fn metrics_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match compute_metrics(&state.store, state.initial_bankroll).await {
        Ok(metrics) => Json(serde_json::to_value(&metrics).unwrap_or_default()),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

async fn trades_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_recent_trades(50).await {
        Ok(trades) => Json(serde_json::to_value(&trades).unwrap_or_default()),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

async fn trades_all_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_all_trades().await {
        Ok(trades) => Json(serde_json::to_value(&trades).unwrap_or_default()),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

async fn cycles_latest_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_latest_cycle().await {
        Ok(Some(cycle)) => Json(serde_json::to_value(&cycle).unwrap_or_default()),
        Ok(None) => Json(serde_json::json!(null)),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

async fn cycles_all_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_all_cycles().await {
        Ok(cycles) => Json(serde_json::to_value(&cycles).unwrap_or_default()),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

async fn costs_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_all_api_costs().await {
        Ok(costs) => Json(serde_json::to_value(&costs).unwrap_or_default()),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use rust_decimal_macros::dec;
    use tower::ServiceExt;

    async fn app_with_token(token: Option<&str>) -> Router {
        build_router(state_with_token(token).await)
    }

    async fn state_with_token(token: Option<&str>) -> DashboardState {
        let store = Store::new(":memory:").await.unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let switch = Arc::new(KillSwitch::new(dir.path().join("HALT")));
        // The directory has to outlive the state, or the HALT path points at
        // something already deleted and `clear` starts failing for the wrong
        // reason.
        std::mem::forget(dir);
        DashboardState::new(
            store,
            HealthState::new(),
            dec!(100),
            token.map(str::to_string),
            switch,
            crate::config::RiskConfig::default(),
            AgentMode::Paper,
        )
    }

    async fn status(app: Router, uri: &str, auth: Option<&str>) -> StatusCode {
        let mut req = HttpRequest::builder().uri(uri);
        if let Some(a) = auth {
            req = req.header(header::AUTHORIZATION, a);
        }
        app.oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    async fn get_json(app: Router, uri: &str) -> serde_json::Value {
        let resp = app
            .oneshot(HttpRequest::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    async fn post(app: Router, uri: &str, auth: Option<&str>) -> (StatusCode, serde_json::Value) {
        let mut req = HttpRequest::builder().method("POST").uri(uri);
        if let Some(a) = auth {
            req = req.header(header::AUTHORIZATION, a);
        }
        let resp = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (code, json)
    }

    /// The halt endpoint is the one route that changes what the agent does.
    /// Leaving it open would make the dashboard a remote stop button.
    #[tokio::test]
    async fn the_control_routes_sit_behind_the_token() {
        let app = build_router(state_with_token(Some("s3cret")).await);
        for route in ["/api/halt", "/api/resume", "/api/reconcile/ack"] {
            let (code, _) = post(app.clone(), route, None).await;
            assert_eq!(
                code,
                StatusCode::UNAUTHORIZED,
                "{route} must require the bearer token"
            );
        }
        assert_eq!(
            status(app, "/api/halt", None).await,
            StatusCode::UNAUTHORIZED,
            "reading the halt state is behind the token too"
        );
    }

    #[tokio::test]
    async fn halting_sets_the_flag_and_resuming_clears_it() {
        let state = state_with_token(None).await;
        let switch = state.kill_switch.clone();
        let app = build_router(state);

        assert!(!switch.is_tripped(), "starts clear");

        let (code, body) = post(app.clone(), "/api/halt", None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["halted"], serde_json::json!(true));
        assert_eq!(body["newly_halted"], serde_json::json!(true));
        assert!(
            switch.is_tripped(),
            "the flag the agent loop reads must be set"
        );
        assert_eq!(switch.current().unwrap().source, HaltSource::Api);

        let (code, body) = post(app.clone(), "/api/resume", None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["halted"], serde_json::json!(false));
        assert!(!switch.is_tripped(), "resume must clear the flag");
    }

    /// Halting twice must not report the second one as new, or the agent
    /// would cancel orders and re-alert on every press.
    #[tokio::test]
    async fn a_second_halt_is_not_a_new_halt() {
        let app = build_router(state_with_token(None).await);
        let (_, first) = post(app.clone(), "/api/halt", None).await;
        assert_eq!(first["newly_halted"], serde_json::json!(true));
        let (_, second) = post(app, "/api/halt", None).await;
        assert_eq!(second["halted"], serde_json::json!(true));
        assert_eq!(second["newly_halted"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn acknowledging_a_reconciliation_mismatch_resumes() {
        // The plan's route name for a resume, pointed at the same handler —
        // so a mismatch halt raised by the reconciler is cleared by it.
        let state = state_with_token(None).await;
        let switch = state.kill_switch.clone();
        let app = build_router(state);
        switch.trip(Halt::new(
            HaltSource::Reconciliation,
            HaltScope::UntilResume,
            "alpaca: 1 position held at the venue but absent locally",
            chrono::Utc::now(),
        ));

        let (code, body) = post(app, "/api/reconcile/ack", None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["halted"], serde_json::json!(false));
        assert!(!switch.is_tripped());
    }

    /// The two endpoints must never disagree about whether trading is
    /// stopped. They did: `/api/health` served a snapshot written at the end
    /// of each cycle, so a halt raised during a long cycle showed as `false`
    /// there and `true` on `/api/halt` — and the dashboard reads health.
    /// Every new route must be behind the token and must actually exist.
    /// The pages that call them were written before the handlers were, and
    /// the only symptom of a missing route is an empty card.
    #[tokio::test]
    async fn the_new_read_routes_exist_and_are_authenticated() {
        let authed = build_router(state_with_token(Some("s3cret")).await);
        let open = build_router(state_with_token(None).await);

        for route in [
            "/api/orders",
            "/api/fills",
            "/api/equity",
            "/api/reconciliation",
            "/api/risk",
        ] {
            assert_eq!(
                status(authed.clone(), route, None).await,
                StatusCode::UNAUTHORIZED,
                "{route} must sit behind the token"
            );
            assert_eq!(
                status(open.clone(), route, None).await,
                StatusCode::OK,
                "{route} must be routed at all"
            );
        }
    }

    /// An empty database must produce empty arrays, not an error object. The
    /// UI narrows on shape, and a `{"error": ...}` body renders as a failed
    /// card — which on a fresh install would say something is broken when
    /// nothing has happened yet.
    #[tokio::test]
    async fn the_new_collections_are_empty_arrays_before_anything_happens() {
        let app = build_router(state_with_token(None).await);
        for route in [
            "/api/orders",
            "/api/fills",
            "/api/equity",
            "/api/reconciliation",
        ] {
            let body = get_json(app.clone(), route).await;
            assert!(
                body.is_array(),
                "{route} returned {body} rather than an array"
            );
            assert_eq!(body.as_array().unwrap().len(), 0);
        }
    }

    #[tokio::test]
    async fn the_risk_route_reports_the_configured_limits() {
        let app = build_router(state_with_token(None).await);
        let body = get_json(app, "/api/risk").await;

        // Defaults from RiskConfig::default(), which the test state uses.
        assert_eq!(body["limits"]["max_trades_per_day"], serde_json::json!(10));
        assert_eq!(
            body["limits"]["max_consecutive_losses"],
            serde_json::json!(4)
        );
        assert_eq!(
            body["limits"]["max_drawdown_pct"],
            serde_json::json!("0.15")
        );
        // Paper mode: the absolute cash caps are reported but do not bind.
        assert_eq!(
            body["limits"]["live_caps_apply"],
            serde_json::json!(false),
            "paper mode must not claim the live caps apply"
        );
        assert_eq!(body["mode"], serde_json::json!("paper"));
        // Nothing recorded yet, and that must read as absent rather than zero.
        assert_eq!(body["starting_equity"], serde_json::Value::Null);
        assert_eq!(body["trades_today"], serde_json::json!(0));
    }

    #[tokio::test]
    async fn health_reports_a_halt_raised_since_the_last_cycle_completed() {
        let state = state_with_token(None).await;
        let switch = state.kill_switch.clone();
        let app = build_router(state);

        // No cycle has completed, so nothing has been published.
        let before = get_json(app.clone(), "/api/health").await;
        assert_eq!(before["halted"], serde_json::json!(false));

        switch.trip(Halt::new(
            HaltSource::HaltFile,
            HaltScope::UntilResume,
            "margin call",
            chrono::Utc::now(),
        ));

        let after = get_json(app.clone(), "/api/health").await;
        assert_eq!(
            after["halted"],
            serde_json::json!(true),
            "health must not wait for a cycle to notice a halt"
        );
        assert_eq!(after["halt"]["source"], serde_json::json!("halt_file"));
        assert_eq!(after["halt"]["detail"], serde_json::json!("margin call"));

        // And the two routes agree.
        let halt_route = get_json(app, "/api/halt").await;
        assert_eq!(halt_route["halted"], after["halted"]);
        assert_eq!(halt_route["halt"], after["halt"]);
    }

    #[tokio::test]
    async fn health_reports_a_resume_immediately_too() {
        let state = state_with_token(None).await;
        let switch = state.kill_switch.clone();
        let app = build_router(state);
        switch.trip(Halt::new(
            HaltSource::Api,
            HaltScope::UntilResume,
            "x",
            chrono::Utc::now(),
        ));
        assert_eq!(
            get_json(app.clone(), "/api/health").await["halted"],
            serde_json::json!(true)
        );
        switch.clear().unwrap();
        let resumed = get_json(app, "/api/health").await;
        assert_eq!(resumed["halted"], serde_json::json!(false));
        assert_eq!(resumed["halt"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn the_halt_status_route_reports_the_reason() {
        let state = state_with_token(None).await;
        let switch = state.kill_switch.clone();
        let app = build_router(state);
        switch.trip(Halt::new(
            HaltSource::CircuitBreaker,
            HaltScope::RestOfDay,
            "down 6.20% on the day, limit 5.00%",
            chrono::Utc::now(),
        ));

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/halt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["halted"], serde_json::json!(true));
        assert_eq!(body["halt"]["source"], serde_json::json!("circuit_breaker"));
        assert_eq!(body["halt"]["scope"], serde_json::json!("rest_of_day"));
        assert!(body["halt"]["detail"].as_str().unwrap().contains("6.20%"));
    }

    #[tokio::test]
    async fn api_is_open_when_no_token_configured() {
        let app = app_with_token(None).await;
        assert_eq!(
            status(app.clone(), "/api/trades", None).await,
            StatusCode::OK
        );
        assert_eq!(status(app, "/api/health", None).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn blank_token_counts_as_no_token() {
        let app = app_with_token(Some("   ")).await;
        assert_eq!(status(app, "/api/trades", None).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn token_is_trimmed_before_comparison() {
        // A token with stray whitespace (easy to introduce in an .env or
        // systemd EnvironmentFile) must still match what the page sends.
        let app = app_with_token(Some("  s3cret\t")).await;
        assert_eq!(
            status(app.clone(), "/api/trades", Some("Bearer s3cret")).await,
            StatusCode::OK
        );
        // The comparison itself is still exact — only the configured value is
        // trimmed, not whatever the client sends.
        assert_eq!(
            status(app, "/api/trades", Some("Bearer  s3cret")).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn api_requires_bearer_token_when_configured() {
        let app = app_with_token(Some("s3cret")).await;
        assert_eq!(
            status(app.clone(), "/api/trades", None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(app.clone(), "/api/trades", Some("Bearer wrong")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(app.clone(), "/api/metrics", Some("s3cret")).await,
            StatusCode::UNAUTHORIZED,
            "token must be sent as a Bearer credential"
        );
        assert_eq!(
            status(app.clone(), "/api/trades", Some("Bearer s3cret")).await,
            StatusCode::OK
        );
        // Health stays open for uptime probes; the index page is public.
        assert_eq!(
            status(app.clone(), "/api/health", None).await,
            StatusCode::OK
        );
        assert_eq!(status(app, "/", None).await, StatusCode::OK);
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("::1"));
        assert!(is_loopback("localhost"));
        assert!(!is_loopback("0.0.0.0"));
        assert!(!is_loopback("192.168.1.10"));
        assert!(!is_loopback("not-an-address"));
    }

    #[test]
    fn non_loopback_bind_without_token_is_refused_in_live_mode() {
        assert!(resolve_bind("0.0.0.0", false, AgentMode::Live).is_err());
        assert_eq!(
            resolve_bind("0.0.0.0", false, AgentMode::Paper).unwrap(),
            LOOPBACK
        );
        assert_eq!(
            resolve_bind("0.0.0.0", true, AgentMode::Live).unwrap(),
            "0.0.0.0"
        );
        assert_eq!(
            resolve_bind("127.0.0.1", false, AgentMode::Live).unwrap(),
            "127.0.0.1"
        );
    }
}
