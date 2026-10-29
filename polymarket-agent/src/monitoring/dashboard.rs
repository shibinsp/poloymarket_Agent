//! Web dashboard — axum HTTP server serving REST API + embedded HTML.
//!
//! When `DASHBOARD_TOKEN` is configured, every `/api/*` route except
//! `/api/health` (kept open for uptime probes) requires
//! `Authorization: Bearer <token>`. Binding to a non-loopback address without
//! a token is refused in live mode and downgraded to loopback otherwise.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use anyhow::{bail, Result};
use axum::extract::{Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use rust_decimal::prelude::ToPrimitive;
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DynamicCapitalCaps {
    pub india_capital_allocation_inr: Decimal,
    pub us_capital_allocation_usd: Decimal,
    pub india_enabled: bool,
    pub us_enabled: bool,
    pub polymarket_enabled: bool,
}

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
    valuation_enabled: bool,
    valuation_model: String,
    pub upstox_feed: Arc<crate::india::market_feed::UpstoxLiveMarketFeed>,
    pub alpaca_feed: Arc<crate::us::market_feed::AlpacaLiveMarketFeed>,
    pub dynamic_caps: Arc<tokio::sync::RwLock<DynamicCapitalCaps>>,
}

impl DashboardState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Store,
        health: HealthState,
        initial_bankroll: Decimal,
        api_token: Option<String>,
        kill_switch: Arc<KillSwitch>,
        risk: crate::config::RiskConfig,
        mode: AgentMode,
        valuation_enabled: bool,
        valuation_model: String,
    ) -> Self {
        let init_india = risk
            .india_capital_allocation_inr
            .unwrap_or(rust_decimal_macros::dec!(1000.0));
        let init_us = risk
            .us_capital_allocation_usd
            .unwrap_or(rust_decimal_macros::dec!(5.0));

        Self {
            store: Arc::new(store),
            health,
            initial_bankroll,
            kill_switch,
            risk: Arc::new(risk),
            mode,
            valuation_enabled,
            valuation_model,
            upstox_feed: Arc::new(crate::india::market_feed::UpstoxLiveMarketFeed::new(
                None, None, None, None,
            )),
            alpaca_feed: Arc::new(crate::us::market_feed::AlpacaLiveMarketFeed::new(
                None, None, None,
            )),
            dynamic_caps: Arc::new(tokio::sync::RwLock::new(DynamicCapitalCaps {
                india_capital_allocation_inr: init_india,
                us_capital_allocation_usd: init_us,
                india_enabled: true,
                us_enabled: true,
                polymarket_enabled: true,
            })),
            // Trim before storing: the page sends a trimmed token, so keeping
            // stray whitespace here would 401 every request with an
            // apparently-correct token.
            api_token: api_token
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .map(|t| Arc::from(t.as_str())),
        }
    }

    pub fn with_upstox_feed(
        mut self,
        feed: Arc<crate::india::market_feed::UpstoxLiveMarketFeed>,
    ) -> Self {
        self.upstox_feed = feed;
        self
    }

    pub fn with_alpaca_feed(
        mut self,
        feed: Arc<crate::us::market_feed::AlpacaLiveMarketFeed>,
    ) -> Self {
        self.alpaca_feed = feed;
        self
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
    let app = build_router(state);

    Ok(tokio::spawn(async move {
        let mut any_listening = false;

        // 1. Primary IPv4 listener (e.g. 127.0.0.1 or 0.0.0.0)
        let addr_v4 = format!("{bind}:{port}");
        match tokio::net::TcpListener::bind(&addr_v4).await {
            Ok(listener_v4) => {
                info!(addr = %addr_v4, "Dashboard server listening (IPv4)");
                any_listening = true;
                let app_v4 = app.clone();
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener_v4, app_v4).await {
                        warn!(error = %e, "Dashboard server error on IPv4");
                    }
                });
            }
            Err(e) => {
                warn!(error = %e, addr = %addr_v4, "Failed to bind IPv4 dashboard server");
            }
        }

        // 2. Also bind IPv6 loopback [::1]:port so `http://localhost:8080` resolves
        // seamlessly in Windows 11 browsers that query IPv6 loopback by default
        if is_loopback(&bind) {
            let addr_v6 = format!("[::1]:{port}");
            if let Ok(listener_v6) = tokio::net::TcpListener::bind(&addr_v6).await {
                info!(addr = %addr_v6, "Dashboard server listening (IPv6 localhost)");
                any_listening = true;
                let app_v6 = app.clone();
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener_v6, app_v6).await {
                        warn!(error = %e, "Dashboard server error on IPv6");
                    }
                });
            }
        }

        if !any_listening {
            warn!("Dashboard server failed to bind to any address");
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
        .route("/api/halt", get(halt_status_handler))
        .route("/api/india/watchlist", get(india_watchlist_handler))
        .route(
            "/api/india/backtest/history",
            get(india_backtest_history_handler),
        )
        .route(
            "/api/india/sandbox/status",
            get(india_sandbox_status_handler),
        )
        .route("/api/india/backtest/run", post(india_backtest_run_handler))
        .route(
            "/api/india/sandbox/order",
            post(india_sandbox_order_handler),
        )
        .route(
            "/api/india/sandbox/reset",
            post(india_sandbox_reset_handler),
        )
        .route("/api/india/chart", get(india_chart_handler))
        .route("/api/us/watchlist", get(us_watchlist_handler))
        .route("/api/us/chart", get(us_chart_handler))
        .route("/api/us/backtest/history", get(us_backtest_history_handler))
        .route("/api/us/backtest/run", post(us_backtest_run_handler))
        .route("/api/us/paper/status", get(us_paper_status_handler))
        .route("/api/us/paper/order", post(us_paper_order_handler))
        .route("/api/us/paper/reset", post(us_paper_reset_handler))
        .route("/api/fx/rate", get(fx_rate_handler))
        .route("/api/portfolio/combined", get(combined_portfolio_handler))
        .route("/api/intraday/status", get(intraday_status_handler))
        .route("/api/upstox/auth/status", get(upstox_auth_status_handler))
        .route("/api/upstox/auth/token", post(upstox_auth_token_handler))
        .route("/api/upstox/feed/status", get(upstox_feed_status_handler))
        .route(
            "/api/upstox/strategy/tick",
            post(upstox_strategy_tick_handler),
        )
        .route(
            "/api/upstox/simulator/order",
            post(upstox_simulator_order_handler),
        )
        .route("/api/alpaca/feed/status", get(alpaca_feed_status_handler))
        .route("/api/pullback/backtest", get(pullback_backtest_handler))
        .route(
            "/api/settings/caps",
            get(settings_caps_get_handler).post(settings_caps_post_handler),
        )
        .route("/api/capital", get(settings_caps_get_handler))
        .route("/api/capital/set", post(settings_caps_post_handler))
        .route("/api/venue/status", get(venue_status_handler))
        .route("/api/logs", get(logs_handler))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));

    // State-changing routes get a second gate on top of the token.
    //
    // Every `/api/*` route was read-only until these existed, so an unset
    // `DASHBOARD_TOKEN` — the documented default on a loopback bind — was
    // merely an information question. It is not any more: a `POST` with no
    // custom header is a CORS *simple request*, which any page the operator
    // browses can issue at `localhost:8080`. The browser sends it, the
    // response is opaque to the attacker, and the side effect lands anyway —
    // a drive-by kill switch, and worse, a drive-by *resume* that lifts the
    // drawdown halt the README says only a person can clear.
    //
    // Requiring a header the fetch spec will not send cross-origin without a
    // preflight, and refusing a cross-site Origin outright, closes both.
    let control = Router::new()
        .route("/api/halt", post(halt_handler))
        .route("/api/resume", post(resume_handler))
        .route("/api/emergency_stop", post(emergency_stop_handler))
        .route("/api/venue/toggle", post(venue_toggle_handler))
        // The plan's name for the same action. A reconciliation mismatch is
        // cleared by acknowledging it, which is a resume.
        .route("/api/reconcile/ack", post(resume_handler))
        .route_layer(middleware::from_fn(require_same_origin))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));

    Router::new()
        .route("/", get(index_handler))
        .route("/pro", get(pro_terminal_handler))
        .route("/terminal", get(pro_terminal_handler))
        .route("/api/health", get(health_handler))
        .route("/api/upstox/callback", get(upstox_oauth_callback_handler))
        .merge(protected)
        .merge(control)
        .fallback(index_handler)
        .with_state(state)
}

/// Header the dashboard sends on every control request.
///
/// Its only job is to be *custom*: a cross-origin `fetch` carrying it stops
/// being a CORS simple request and triggers a preflight, which this server
/// never answers. The value is irrelevant — its presence is the proof that
/// the request came from code allowed to read this origin.
const CONTROL_HEADER: &str = "x-agent-control";

/// Refuse state-changing requests that a foreign page could have made.
///
/// Two independent checks, because each covers a gap in the other. `Origin`
/// is set by browsers on every cross-site POST and is the direct signal, but
/// is absent on same-origin requests in some browsers and on curl. The custom
/// header cannot be set cross-origin without a preflight, but is trivially
/// present on curl. Requiring the header and rejecting a foreign `Origin`
/// leaves the command line working and the browser drive-by refused.
async fn require_same_origin(req: Request, next: Next) -> Response {
    if let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let matches_host = origin
            .rsplit("//")
            .next()
            .map(|o| o == host)
            .unwrap_or(false);
        if !matches_host {
            warn!(origin, host, "Refused a cross-origin control request");
            return (
                StatusCode::FORBIDDEN,
                "cross-origin control requests are refused",
            )
                .into_response();
        }
    }

    if req.headers().get(CONTROL_HEADER).is_none() {
        return (
            StatusCode::FORBIDDEN,
            format!("control requests must carry the {CONTROL_HEADER} header"),
        )
            .into_response();
    }

    next.run(req).await
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
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"),
            (header::PRAGMA, "no-cache"),
            (header::EXPIRES, "0"),
        ],
        html,
    )
}

/// A read route that could not read. Carries the agent's own message.
fn rows_unavailable(e: anyhow::Error) -> Response {
    warn!(error = %e, "Dashboard read failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({"error": e.to_string()})),
    )
        .into_response()
}

/// The execution record: what was asked for, and what came back.
async fn orders_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_orders(500).await {
        Ok(rows) => Json(serde_json::to_value(&rows).unwrap_or_default()).into_response(),
        // A real status code, unlike the older routes. Those answer 200 with
        // an error body, which the page's array narrowing rejects as
        // "malformed" — so the operator never sees "database is locked", only
        // a card that failed for no stated reason.
        Err(e) => rows_unavailable(e),
    }
}

/// Individual executions — where slippage and time-to-fill live.
async fn fills_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_fills(500).await {
        Ok(rows) => Json(serde_json::to_value(&rows).unwrap_or_default()).into_response(),
        // A real status code, unlike the older routes. Those answer 200 with
        // an error body, which the page's array narrowing rejects as
        // "malformed" — so the operator never sees "database is locked", only
        // a card that failed for no stated reason.
        Err(e) => rows_unavailable(e),
    }
}

/// Start-of-day equity and the running peak — the drawdown series.
async fn equity_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_daily_equity().await {
        Ok(rows) => Json(serde_json::to_value(&rows).unwrap_or_default()).into_response(),
        // A real status code, unlike the older routes. Those answer 200 with
        // an error body, which the page's array narrowing rejects as
        // "malformed" — so the operator never sees "database is locked", only
        // a card that failed for no stated reason.
        Err(e) => rows_unavailable(e),
    }
}

async fn reconciliation_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_reconciliation_runs(200).await {
        Ok(rows) => Json(serde_json::to_value(&rows).unwrap_or_default()).into_response(),
        // A real status code, unlike the older routes. Those answer 200 with
        // an error body, which the page's array narrowing rejects as
        // "malformed" — so the operator never sees "database is locked", only
        // a card that failed for no stated reason.
        Err(e) => rows_unavailable(e),
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

    // The promotion criterion asks for Brier ≤0.24 over ≥30 closed
    // positions. Below that count it is reported as `null` rather than as a
    // number, because a Brier over four closes is noise with a decimal point
    // and would be read as a pass.
    let brier = crate::valuation::calibration::brier_score(state.store.pool(), 30)
        .await
        .unwrap_or(None);

    Json(serde_json::json!({
        "mode": format!("{:?}", state.mode).to_lowercase(),
        "day": today.to_string(),
        "starting_equity": today_row.map(|r| r.starting_equity.clone()),
        "current_equity": today_row.and_then(|r| r.closing_equity.clone()),
        "high_water_mark": peak.map(|p| p.to_string()),
        "trades_today": trades_today,
        "consecutive_losses": losses,
        // 0.25 is what always guessing 50% scores — the number to beat before
        // believing any of this has edge.
        "brier_score": brier.map(|(score, _)| score.round_dp(4).to_string()),
        "brier_sample": brier.map(|(_, n)| n),
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

    if let Some(ref h) = cleared {
        if matches!(
            h.source,
            crate::agent::kill_switch::HaltSource::Reconciliation
        ) {
            let now_str = chrono::Utc::now().to_rfc3339();
            let _ = sqlx::query(
                "UPDATE trades SET status = 'CLOSED', close_reason = 'reconciliation_cleared', closed_at = ? WHERE status IN ('OPEN', 'PARTIAL')"
            )
            .bind(&now_str)
            .execute(state.store.pool())
            .await;
        }
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
        let halted = state.kill_switch.is_tripped();
        obj.insert("halted".to_string(), serde_json::json!(halted));
        obj.insert(
            "halt".to_string(),
            halt.and_then(|h| serde_json::to_value(h).ok())
                .unwrap_or(serde_json::Value::Null),
        );
        obj.insert(
            "valuation_enabled".to_string(),
            serde_json::json!(state.valuation_enabled),
        );
        obj.insert(
            "valuation_model".to_string(),
            serde_json::json!(&state.valuation_model),
        );

        // `status` is what an uptime probe keys on, and it was only rewritten
        // when a cycle completed — so halting while the loop was idle left it
        // reporting "ok" for up to `max_sleep_seconds` beside a `halted: true`
        // that contradicted it. Derived here from the same live flag, unless
        // the agent is dead, which outranks everything.
        let dead = obj.get("status").and_then(|v| v.as_str()) == Some("dead");
        if !dead {
            obj.insert(
                "status".to_string(),
                serde_json::json!(if halted { "halted" } else { "ok" }),
            );
            if halted {
                obj.insert("agent_state".to_string(), serde_json::json!("HALTED"));
            } else if obj.get("agent_state").and_then(|v| v.as_str()) == Some("HALTED")
                || obj.get("agent_state").and_then(|v| v.as_str()) == Some("INITIALIZING")
            {
                obj.insert("agent_state".to_string(), serde_json::json!("ALIVE"));
            }
        }
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

// -- India Testing Handlers --

#[derive(Debug, serde::Deserialize)]
pub struct IndiaBacktestRunRequest {
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub csv_path: Option<String>,
    pub csv_content: Option<String>,
    pub initial_capital: Option<Decimal>,
    pub risk_pct_per_trade: Option<Decimal>,
    pub stop_loss_pct: Option<Decimal>,
    pub take_profit_pct: Option<Decimal>,
    pub train_ratio: Option<Decimal>,
    pub val_ratio: Option<Decimal>,
    pub test_ratio: Option<Decimal>,
}

async fn india_watchlist_handler() -> impl IntoResponse {
    let list = crate::india::instruments::nse_watchlist();
    Json(serde_json::to_value(&list).unwrap_or_default())
}

async fn india_backtest_history_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_india_backtests(20).await {
        Ok(rows) => Json(serde_json::to_value(&rows).unwrap_or_default()).into_response(),
        Err(e) => rows_unavailable(e),
    }
}

async fn india_backtest_run_handler(
    State(state): State<DashboardState>,
    Json(req): Json<IndiaBacktestRunRequest>,
) -> impl IntoResponse {
    use crate::india::backtest::{parse_and_validate_csv, run_backtest, NseBacktestConfig};
    let symbol = req.symbol.as_deref().unwrap_or("RELIANCE");
    let candles = if let Some(content) = req.csv_content.as_deref() {
        match parse_and_validate_csv(content) {
            Ok(candles) => candles,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Invalid OHLCV CSV: {e:#}") })),
                )
                    .into_response();
            }
        }
    } else if let Some(path) = req.csv_path.as_deref() {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Failed to read CSV at {path}: {e}") })),
                )
                    .into_response();
            }
        };
        match parse_and_validate_csv(&content) {
            Ok(candles) => candles,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Invalid OHLCV CSV: {e:#}") })),
                )
                    .into_response();
            }
        }
    } else {
        let history = match state.upstox_feed.fetch_intraday_5m_candles(symbol).await {
            Ok(history) => history,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("Could not load real Upstox 5-minute candles for {symbol}: {e:#}. Reauthorize Upstox or provide a historical OHLCV CSV.")
                    })),
                )
                    .into_response();
            }
        };
        history
            .into_iter()
            .map(|bar| crate::india::backtest::NseCandle {
                timestamp: bar.timestamp,
                symbol: bar.symbol,
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                volume: bar.volume,
            })
            .collect()
    };

    let symbols: std::collections::HashSet<_> = candles
        .iter()
        .map(|candle| candle.symbol.as_str())
        .collect();
    if symbols.len() != 1 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "The NSE strategy backtest currently evaluates one symbol per run. Choose one symbol or split the CSV by symbol."
            })),
        )
            .into_response();
    }

    let mut config = NseBacktestConfig::default();
    if let Some(c) = req.initial_capital {
        config.initial_capital_inr = c;
    }
    if let Some(r) = req.risk_pct_per_trade {
        config.max_position_pct = r;
    }
    if let Some(s) = req.stop_loss_pct {
        config.stop_loss_pct = s;
    }
    if let Some(t) = req.take_profit_pct {
        config.take_profit_pct = t;
    }
    if let Some(tr) = req.train_ratio {
        config.train_split_pct = tr;
    }
    if let Some(vr) = req.val_ratio {
        config.val_split_pct = vr;
    }

    let run_name = req.name.unwrap_or_else(|| {
        format!(
            "Pullback backtest {symbol} {}",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
        )
    });

    let result = match run_backtest(&candles, &config) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Backtest execution failed: {e:#}") })),
            )
                .into_response();
        }
    };

    let id = format!("bt-{}", uuid::Uuid::new_v4().simple());
    let config_json = serde_json::to_string(&config).unwrap_or_default();
    let results_json = serde_json::to_string(&result).unwrap_or_default();

    if let Err(e) = state
        .store
        .save_india_backtest(&id, &run_name, &config_json, &results_json, "COMPLETED")
        .await
    {
        warn!(error = %e, "Failed to persist India backtest run");
    }

    Json(serde_json::to_value(&result).unwrap_or_default()).into_response()
}

/// Unified accounting for India Sandbox (NSE Cash Equities).
/// Starting capital defaults to strictly ₹1,000 INR (configured via `india_capital_allocation_inr`).
/// Profit from sold positions is compounded back into `available_cash`.
/// US market money ($5.00 base) is completely isolated by architecture and cannot be mixed here.
fn compute_india_sandbox_accounting(
    base_capital: Decimal,
    orders: &[crate::db::store::IndiaSandboxOrderRecord],
    prices: &std::collections::HashMap<String, Decimal>,
) -> crate::india::simulator::IndiaPaperPortfolioState {
    crate::india::simulator::compute_portfolio_accounting(base_capital, orders, prices)
}

async fn india_sandbox_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let recent_orders = state
        .store
        .get_india_sandbox_orders(100)
        .await
        .unwrap_or_default();
    let watchlist = crate::india::instruments::nse_watchlist();
    let base_capital = state.dynamic_caps.read().await.india_capital_allocation_inr;

    let feed_info = state.upstox_feed.get_status_info().await;
    let ticks = state.upstox_feed.prices.read().await.clone();
    let prices: std::collections::HashMap<String, Decimal> = ticks
        .iter()
        .map(|(k, v)| (k.clone(), v.last_price))
        .collect();

    let portfolio = compute_india_sandbox_accounting(base_capital, &recent_orders, &prices);
    let unrealized_profit_inr: Decimal = portfolio
        .open_positions
        .iter()
        .map(|p| p.unrealized_pnl)
        .sum();

    let resp = serde_json::json!({
        "venue_id": "upstox_sandbox",
        "name": "Upstox Live Feed · Paper Trading Simulator",
        "currency": "INR",
        "currency_symbol": "₹",
        "timezone": "Asia/Kolkata",
        "trading_hours": "09:15 - 15:30 IST",
        "allowlisted_host": "api.upstox.com",
        "live_trading_allowed": false,
        "mode_description": "Upstox live market data feed connected to local Paper Trading Simulator. All buy/sell strategy decisions execute in the virtual simulator with configurable INR starting capital. Zero live orders are sent to Upstox.",
        "base_capital_inr": portfolio.base_capital_inr,
        "realized_profit_inr": portfolio.realized_profit_inr,
        "unrealized_profit_inr": unrealized_profit_inr,
        "open_positions_cost_inr": portfolio.open_positions_cost_inr,
        "available_cash_inr": portfolio.available_cash_inr,
        "total_equity_inr": portfolio.total_equity_inr,
        "holdings": portfolio.holdings,
        "open_positions": portfolio.open_positions,
        "recent_orders": recent_orders,
        "watchlist": watchlist,
        "feed_status": feed_info,
        "live_ticks": ticks,
    });

    Json(resp)
}

async fn india_sandbox_reset_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let base_capital = state.dynamic_caps.read().await.india_capital_allocation_inr;
    match state.store.clear_india_sandbox_orders().await {
        Ok(_) => {
            info!("India sandbox orders cleared - reset to ₹1,000 base capital");
            Json(serde_json::json!({
                "status": "success",
                "message": "Indian market sandbox orders reset successfully. Starting capital reset to ₹1,000 INR.",
                "base_capital_inr": base_capital,
                "available_cash_inr": base_capital,
                "realized_profit_inr": 0.0,
                "open_positions_cost_inr": 0.0,
                "total_equity_inr": base_capital
            }))
            .into_response()
        }
        Err(e) => {
            warn!("Failed to clear India sandbox orders: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    serde_json::json!({ "error": format!("Failed to reset India sandbox orders: {e}") }),
                ),
            )
                .into_response()
        }
    }
}

// --- Upstox Live Market Data Feed & Paper Simulator API Handlers ---

async fn upstox_auth_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let status = state.upstox_feed.get_status_info().await;
    Json(status)
}

#[derive(Debug, serde::Deserialize)]
pub struct UpstoxAuthTokenSubmitRequest {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub access_token: Option<String>,
    pub code: Option<String>,
}

async fn upstox_auth_token_handler(
    State(state): State<DashboardState>,
    Json(req): Json<UpstoxAuthTokenSubmitRequest>,
) -> impl IntoResponse {
    if req.client_id.is_some() || req.client_secret.is_some() {
        state
            .upstox_feed
            .set_credentials(req.client_id.clone(), req.client_secret.clone())
            .await;
    }

    if let Some(code) = req.code {
        match state.upstox_feed.exchange_code_for_token(&code).await {
            Ok(_) => {
                info!("Successfully exchanged OAuth code for Upstox access token");
                let status = state.upstox_feed.get_status_info().await;
                return Json(serde_json::json!({
                    "status": "success",
                    "message": "Upstox authorization code successfully exchanged for access token. Market feed connected.",
                    "feed": status,
                }))
                .into_response();
            }
            Err(e) => {
                warn!("Upstox code exchange failed: {e}");
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Token exchange failed: {e:#}") })),
                )
                    .into_response();
            }
        }
    }

    if let Some(token) = req.access_token {
        state.upstox_feed.set_access_token(token).await;
        let status = state.upstox_feed.get_status_info().await;
        return Json(serde_json::json!({
            "status": "success",
            "message": "Upstox access token configured. Market data feed connected.",
            "feed": status,
        }))
        .into_response();
    }

    let status = state.upstox_feed.get_status_info().await;
    Json(serde_json::json!({
        "status": "success",
        "message": "Upstox app credentials saved.",
        "feed": status,
    }))
    .into_response()
}

async fn upstox_feed_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let feed_info = state.upstox_feed.get_status_info().await;
    let ticks = state.upstox_feed.prices.read().await.clone();
    let mut live_prices_summary = serde_json::Map::new();
    for (sym, t) in &ticks {
        let prev_close = if t.close > Decimal::ZERO {
            t.close
        } else {
            t.last_price
        };
        let change_pct = if prev_close > Decimal::ZERO {
            ((t.last_price - prev_close) / prev_close) * rust_decimal_macros::dec!(100)
        } else {
            Decimal::ZERO
        };
        let change_pct_f64 = change_pct.to_f64().unwrap_or(0.0);
        let price_f64 = t.last_price.to_f64().unwrap_or(0.0);
        live_prices_summary.insert(
            sym.clone(),
            serde_json::json!({
                "price": price_f64,
                "price_str": t.last_price.to_string(),
                "time": t.timestamp.to_rfc3339(),
                "source": t.source,
                "change_pct": change_pct_f64,
            }),
        );
    }
    Json(serde_json::json!({
        "connected": feed_info.connected,
        "status": feed_info.status,
        "feed_type": feed_info.feed_type,
        "token_configured": feed_info.token_configured,
        "token_preview": feed_info.token_preview,
        "redirect_uri": feed_info.redirect_uri,
        "auth_dialog_url": feed_info.auth_dialog_url,
        "symbols_subscribed": feed_info.symbols,
        "last_tick_time": feed_info.last_update,
        "last_feed_timestamp": feed_info.last_feed_timestamp,
        "measured_delay_ms": feed_info.measured_delay_ms,
        "is_stale": feed_info.is_stale,
        "stale_reason": feed_info.stale_reason,
        "trading_paused_due_to_stale": feed_info.trading_paused_due_to_stale,
        "session": feed_info.session,
        "reconnect_count": feed_info.reconnect_count,
        "resubscribe_count": feed_info.resubscribe_count,
        "backoff_delay_secs": feed_info.backoff_delay_secs,
        "deduped_ticks_count": feed_info.deduped_ticks_count,
        "live_prices": live_prices_summary,
        "ticks": ticks,
        "feed": feed_info,
    }))
}

#[derive(Debug, serde::Deserialize)]
pub struct UpstoxOAuthCallbackQuery {
    pub code: Option<String>,
    pub error: Option<String>,
}

async fn upstox_oauth_callback_handler(
    State(state): State<DashboardState>,
    Query(query): Query<UpstoxOAuthCallbackQuery>,
) -> impl IntoResponse {
    if let Some(code) = query.code {
        match state.upstox_feed.exchange_code_for_token(&code).await {
            Ok(_) => {
                let html = r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>Upstox Market Data Feed Connected</title>
  <style>
    body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: #0b0f19; color: #f3f4f6; display: grid; place-items: center; min-height: 100vh; margin: 0; }
    .card { background: #111827; border: 1px solid #1f2937; border-radius: 12px; padding: 32px; max-width: 520px; text-align: center; box-shadow: 0 10px 25px rgba(0,0,0,0.5); }
    h1 { color: #10b981; margin-top: 0; font-size: 22px; }
    p { color: #9ca3af; font-size: 14px; line-height: 1.6; margin: 16px 0; }
    .badge { display: inline-block; padding: 4px 10px; background: #064e3b; color: #6ee7b7; border-radius: 20px; font-size: 12px; font-weight: 600; margin-bottom: 12px; }
    .btn { display: inline-block; padding: 10px 20px; background: #2563eb; color: #ffffff; text-decoration: none; border-radius: 6px; font-size: 14px; font-weight: 600; margin-top: 8px; transition: background 0.15s; }
    .btn:hover { background: #1d4ed8; }
  </style>
</head>
<body>
  <div class="card">
    <div class="badge">LIVE FEED AUTHORIZED</div>
    <h1>Upstox Feed Connected Successfully</h1>
    <p>Your Upstox Developer App authorization code was verified and exchanged for an active market data session.</p>
    <p><strong>Safety Protocol:</strong> Live prices are streaming to the strategy engine. All order actions are permanently locked to the local <strong>Paper Trading Simulator</strong> with ₹1,000 starting cash. Zero live orders are placed on Upstox.</p>
    <a href="/india-test" class="btn">Return to Dashboard</a>
  </div>
</body>
</html>"#;
                return (
                    StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    html,
                )
                    .into_response();
            }
            Err(e) => {
                let err_html = format!(
                    r#"<!DOCTYPE html>
<html>
<head><title>Authorization Failed</title><style>body{{font-family:sans-serif;background:#0b0f19;color:#f3f4f6;display:grid;place-items:center;min-height:100vh;margin:0}}.card{{background:#111827;border:1px solid #ef4444;border-radius:12px;padding:32px;max-width:520px;text-align:center}}h1{{color:#ef4444}}p{{color:#9ca3af}}a{{color:#60a5fa}}</style></head>
<body>
  <div class="card">
    <h1>Authorization Exchange Failed</h1>
    <p>Error: {}</p>
    <p><a href="/india-test">Return to Dashboard and check Client ID / Secret</a></p>
  </div>
</body>
</html>"#,
                    e
                );
                return (
                    StatusCode::BAD_REQUEST,
                    [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    err_html,
                )
                    .into_response();
            }
        }
    }

    let err_msg = query
        .error
        .unwrap_or_else(|| "No authorization code was returned by Upstox".to_string());
    (
        StatusCode::BAD_REQUEST,
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        format!(
            "<h1>Authorization Error</h1><p>{}</p><p><a href='/india-test'>Back</a></p>",
            err_msg
        ),
    )
        .into_response()
}

#[derive(Debug, serde::Deserialize)]
pub struct UpstoxStrategyTickRequest {
    pub symbol: String,
    pub price: Option<Decimal>,
    pub execute: Option<bool>,
}

async fn upstox_strategy_tick_handler(
    State(state): State<DashboardState>,
    Json(req): Json<UpstoxStrategyTickRequest>,
) -> impl IntoResponse {
    let sym = req.symbol.to_uppercase();
    let live_tick = state.upstox_feed.prices.read().await.get(&sym).cloned();
    if req.execute == Some(true) {
        let session = crate::india::session::detect_nse_session(chrono::Utc::now());
        let fresh_live_quote = live_tick
            .as_ref()
            .is_some_and(|tick| tick.last_price > Decimal::ZERO && tick.source == "UPSTOX_V3_LIVE");
        if !session.allows_order_entry || state.upstox_feed.is_stale().await || !fresh_live_quote {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Paper execution is paused until a fresh Upstox live quote is available during NSE trading hours.",
                    "paused": true,
                    "is_stale": state.upstox_feed.is_stale().await
                })),
            )
                .into_response();
        }
    }
    let current_price = if req.execute == Some(true) {
        live_tick
            .as_ref()
            .map(|tick| tick.last_price)
            .unwrap_or(Decimal::ZERO)
    } else {
        match req.price {
            Some(p) if p > Decimal::ZERO => p,
            _ => live_tick
                .as_ref()
                .filter(|tick| tick.source == "UPSTOX_V3_LIVE")
                .map(|tick| tick.last_price)
                .unwrap_or(Decimal::ZERO),
        }
    };

    let base_capital = state
        .risk
        .india_capital_allocation_inr
        .unwrap_or(rust_decimal_macros::dec!(1000.0));

    let all_orders = state
        .store
        .get_india_sandbox_orders(200)
        .await
        .unwrap_or_default();

    let ticks_snapshot: HashMap<String, Decimal> = state
        .upstox_feed
        .prices
        .read()
        .await
        .iter()
        .map(|(k, v)| (k.clone(), v.last_price))
        .collect();

    let portfolio = crate::india::simulator::compute_portfolio_accounting(
        base_capital,
        &all_orders,
        &ticks_snapshot,
    );

    let current_holding = portfolio
        .holdings
        .get(&sym)
        .map(|d| d.to_string().parse::<i64>().unwrap_or(0))
        .unwrap_or(0);

    let avg_entry = portfolio
        .open_positions
        .iter()
        .find(|p| p.symbol == sym)
        .map(|p| p.average_buy_price);

    let decision = crate::india::simulator::evaluate_strategy_tick(
        &sym,
        current_price,
        portfolio.available_cash_inr,
        current_holding,
        avg_entry,
    );

    let mut execution_result = None;
    if req.execute == Some(true) && (decision.action == "BUY" || decision.action == "SELL") {
        let order_req = crate::india::simulator::IndiaPaperOrderRequest {
            symbol: sym.clone(),
            side: decision.action.clone(),
            order_type: "LIMIT".to_string(),
            quantity: decision.quantity,
            price: Some(current_price),
            trigger_reason: Some(decision.reason.clone()),
        };

        match crate::india::simulator::execute_paper_order(
            &state.store,
            base_capital,
            &order_req,
            current_price,
        )
        .await
        {
            Ok(res) => {
                execution_result = Some(res);
            }
            Err(e) => {
                warn!("Strategy tick execution error: {e}");
            }
        }
    }

    Json(serde_json::json!({
        "status": "success",
        "symbol": sym,
        "price": current_price,
        "decision": decision,
        "execution": execution_result,
        "portfolio": portfolio,
    }))
    .into_response()
}

async fn upstox_simulator_order_handler(
    State(state): State<DashboardState>,
    Json(req): Json<crate::india::simulator::IndiaPaperOrderRequest>,
) -> impl IntoResponse {
    if state.upstox_feed.is_stale().await {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Paper order paused: Upstox feed has no fresh live quote.",
                "paused": true,
                "is_stale": true
            })),
        )
            .into_response();
    }
    let base_capital = state
        .risk
        .india_capital_allocation_inr
        .unwrap_or(rust_decimal_macros::dec!(1000.0));

    let live_price = state
        .upstox_feed
        .prices
        .read()
        .await
        .get(&req.symbol.to_uppercase())
        .filter(|tick| tick.source == "UPSTOX_V3_LIVE")
        .map(|tick| tick.last_price)
        .unwrap_or(Decimal::ZERO);
    if live_price <= Decimal::ZERO {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "No fresh Upstox live quote is available for this symbol.",
                "paused": true
            })),
        )
            .into_response();
    }

    match crate::india::simulator::execute_paper_order(&state.store, base_capital, &req, live_price)
        .await
    {
        Ok(result) => Json(serde_json::json!({
            "status": "success",
            "order": result.order,
            "costs": result.costs,
            "available_cash_inr": result.available_cash_inr,
            "total_equity_inr": result.total_equity_inr,
            "disclaimer": result.disclaimer,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("{e:#}") })),
        )
            .into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct IndiaChartQuery {
    pub symbol: Option<String>,
    pub range: Option<String>,
}

async fn india_chart_handler(
    State(state): State<DashboardState>,
    Query(query): Query<IndiaChartQuery>,
) -> impl IntoResponse {
    let symbol = query.symbol.unwrap_or_else(|| "RELIANCE".to_string());
    let range = query.range.unwrap_or_else(|| "1D".to_string());
    let orders = state
        .store
        .get_india_sandbox_orders(50)
        .await
        .unwrap_or_default();

    let live_tick = {
        let prices = state.upstox_feed.prices.read().await;
        prices.get(&symbol).cloned()
    };

    let chart =
        crate::india::chart::generate_nse_chart(&symbol, &range, &orders, live_tick.as_ref());
    Json(serde_json::to_value(&chart).unwrap_or_default())
}

#[derive(Debug, serde::Deserialize)]
pub struct IndiaSandboxOrderSubmitRequest {
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub quantity: i64,
    pub price: Option<Decimal>,
    pub allow_stale: Option<bool>,
}

async fn india_sandbox_order_handler(
    State(state): State<DashboardState>,
    Json(req): Json<IndiaSandboxOrderSubmitRequest>,
) -> impl IntoResponse {
    let client_order_id = format!("NSE-SBX-{}", uuid::Uuid::new_v4().simple());
    let now = chrono::Utc::now().to_rfc3339();

    // Exchange session guard: block fills outside regular trading hours (09:15 - 15:30 IST)
    let session = crate::india::session::detect_nse_session(chrono::Utc::now());
    if !session.allows_order_entry && !req.allow_stale.unwrap_or(false) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "Paper order paused: NSE Market is currently {} ({}). Regular continuous trading is 09:15 - 15:30 IST. Paper simulator does not simulate new fills outside active market sessions.",
                    session.status_badge, session.session_name
                ),
                "session": session,
                "paused": true,
                "market_closed": true
            })),
        )
            .into_response();
    }

    // Stale feed guard: pause paper orders if feed updates stopped for >15s
    if state.upstox_feed.is_stale().await && !req.allow_stale.unwrap_or(false) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Paper order paused: Upstox market data feed is stale (>15s since last tick). Execution paused to prevent fills on outdated quotes.",
                "is_stale": true,
                "paused": true
            })),
        )
            .into_response();
    }

    let instrument = crate::india::instruments::find_nse_instrument(&req.symbol);
    if instrument.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Symbol {:?} not in verified NSE universe", req.symbol)
            })),
        )
            .into_response();
    }

    if req.quantity <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Quantity must be a positive whole integer for NSE cash equities"
            })),
        )
            .into_response();
    }

    let base_price = crate::india::chart::get_company_info(&req.symbol).2;
    let order_price = match req.price {
        Some(p) if p > Decimal::ZERO => p,
        _ => base_price,
    };
    let price_str = order_price.to_string();

    let base_capital = state.dynamic_caps.read().await.india_capital_allocation_inr;
    let all_orders = state
        .store
        .get_india_sandbox_orders(500)
        .await
        .unwrap_or_default();
    let prices: std::collections::HashMap<String, Decimal> = state
        .upstox_feed
        .prices
        .read()
        .await
        .iter()
        .map(|(k, v)| (k.clone(), v.last_price))
        .collect();
    let portfolio = compute_india_sandbox_accounting(base_capital, &all_orders, &prices);
    let available_cash = portfolio.available_cash_inr;
    let realized_profit = portfolio.realized_profit_inr;
    let holdings = portfolio.holdings;

    let is_buy = req.side.eq_ignore_ascii_case("BUY");
    let is_sell = req.side.eq_ignore_ascii_case("SELL");

    if is_buy {
        let order_cost = order_price * Decimal::from(req.quantity);
        let entry_charges = crate::india::costs::IndianCostConfig::default()
            .calculate(order_cost, crate::venue::types::Side::Buy)
            .total;
        let total_needed = order_cost + entry_charges;
        if total_needed > available_cash {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "Insufficient Indian market cash. Indian trading is strictly restricted to initial ₹{:.2} capital + realized profits. Available cash: ₹{:.2}, but order cost plus estimated charges (₹{:.2}) is ₹{:.2}. US market funds ($) are completely isolated and cannot be used.",
                        base_capital,
                        available_cash,
                        entry_charges,
                        total_needed
                    ),
                    "available_cash_inr": available_cash,
                    "order_cost_inr": order_cost,
                    "estimated_charges_inr": entry_charges,
                    "total_needed_inr": total_needed,
                    "base_capital_inr": base_capital,
                    "realized_profit_inr": realized_profit,
                })),
            )
                .into_response();
        }
    } else if is_sell {
        let held_qty = holdings
            .get(&req.symbol.to_uppercase())
            .cloned()
            .unwrap_or(Decimal::ZERO);
        if Decimal::from(req.quantity) > held_qty {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "Cannot sell {} shares of {}: currently held {} shares in Indian sandbox portfolio.",
                        req.quantity,
                        req.symbol.to_uppercase(),
                        held_qty
                    ),
                    "held_quantity": held_qty,
                    "requested_quantity": req.quantity,
                })),
            )
                .into_response();
        }
    }

    let order_record = crate::db::store::IndiaSandboxOrderRecord {
        id: format!("ord-{}", uuid::Uuid::new_v4().simple()),
        client_order_id: client_order_id.clone(),
        venue_order_id: Some(format!("UPSTOX-SBX-{}", uuid::Uuid::new_v4().simple())),
        symbol: req.symbol.to_uppercase(),
        side: req.side.to_uppercase(),
        order_type: req.order_type.to_uppercase(),
        quantity: req.quantity,
        price: price_str.clone(),
        trigger_price: None,
        status: "ACKNOWLEDGED".to_string(),
        fill_quantity: req.quantity, // sandbox test execution simulation
        average_fill_price: Some(price_str),
        rejection_reason: None,
        created_at: now.clone(),
        updated_at: now,
    };

    if let Err(e) = state.store.save_india_sandbox_order(&order_record).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to record order: {e}") })),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "status": "success",
        "order": order_record,
        "disclaimer": "Upstox sandbox execution acknowledgment. Fills are simulated for testing purposes and do not represent live market liquidity or guaranteed execution."
    }))
    .into_response()
}

// -- US Market and Multi-Currency FX Testing Handlers --

async fn us_watchlist_handler() -> impl IntoResponse {
    let list = crate::us::instruments::us_watchlist();
    Json(serde_json::to_value(&list).unwrap_or_default())
}

#[derive(Debug, serde::Deserialize)]
pub struct UsChartQuery {
    pub symbol: Option<String>,
    pub range: Option<String>,
}

async fn us_chart_handler(
    State(state): State<DashboardState>,
    Query(query): Query<UsChartQuery>,
) -> impl IntoResponse {
    let symbol = query.symbol.unwrap_or_else(|| "SPY".to_string());
    let range = query.range.unwrap_or_else(|| "1D".to_string());
    let orders = state
        .store
        .get_us_paper_orders(50)
        .await
        .unwrap_or_default();

    let live_tick = state
        .alpaca_feed
        .prices
        .read()
        .await
        .get(&symbol.to_uppercase())
        .cloned();
    let feed_status = state.alpaca_feed.get_status().await;

    let chart = crate::us::chart::generate_us_chart(
        &symbol,
        &range,
        &orders,
        live_tick.as_ref(),
        Some(&feed_status),
    );
    Json(serde_json::to_value(&chart).unwrap_or_default())
}

async fn alpaca_feed_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let status = state.alpaca_feed.get_status().await;
    Json(serde_json::to_value(&status).unwrap_or_default())
}

#[derive(Debug, serde::Deserialize)]
pub struct PullbackBacktestParams {
    pub market: Option<String>, // "india" or "us"
    pub symbol: Option<String>,
    pub initial_capital: Option<Decimal>,
}

async fn pullback_backtest_handler(
    State(state): State<DashboardState>,
    axum::extract::Query(params): axum::extract::Query<PullbackBacktestParams>,
) -> impl IntoResponse {
    use crate::strategy::backtest::run_pullback_backtest;
    use crate::strategy::pullback::{FiveMinCandle, PullbackStrategyConfig};
    use rust_decimal_macros::dec;

    let market_lower = params.market.as_deref().unwrap_or("india").to_lowercase();
    let is_us = market_lower == "us";
    let default_capital = if is_us { dec!(10000.0) } else { dec!(500000.0) };
    let initial_capital = params.initial_capital.unwrap_or(default_capital);

    let config = PullbackStrategyConfig::default();

    let symbol = params
        .symbol
        .as_deref()
        .unwrap_or(if is_us { "SPY" } else { "RELIANCE" });
    let candles: Vec<FiveMinCandle> = if is_us {
        match state.alpaca_feed.fetch_intraday_5m_candles(symbol).await {
            Ok(bars) => bars,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("Could not load real Alpaca 5-minute candles for {symbol}: {e:#}. Check Alpaca data credentials and feed access. No synthetic data was substituted.")
                    })),
                )
                    .into_response();
            }
        }
    } else {
        match state.upstox_feed.fetch_intraday_5m_candles(symbol).await {
            Ok(bars) => bars,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("Could not load real Upstox 5-minute candles for {symbol}: {e:#}. Reauthorize Upstox or provide historical data. Never silently substituting synthetic or demo prices.")
                    })),
                )
                    .into_response();
            }
        }
    };

    if candles.len() < config.ema_slow_period + 10 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Insufficient real candle data (found {}, need at least {}). Never silently substituting synthetic or demo prices.", candles.len(), config.ema_slow_period + 10)
            })),
        )
            .into_response();
    }

    let report = run_pullback_backtest(&candles, is_us, initial_capital, &config, dec!(0.65));
    Json(serde_json::to_value(&report).unwrap_or_default()).into_response()
}

async fn us_backtest_history_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_us_backtests(20).await {
        Ok(rows) => Json(serde_json::to_value(&rows).unwrap_or_default()).into_response(),
        Err(e) => rows_unavailable(e),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct UsBacktestRunRequest {
    pub name: Option<String>,
    pub csv_path: Option<String>,
    pub csv_content: Option<String>,
    pub initial_capital: Option<Decimal>,
    pub risk_pct_per_trade: Option<Decimal>,
    pub stop_loss_pct: Option<Decimal>,
    pub take_profit_pct: Option<Decimal>,
}

async fn us_backtest_run_handler(
    State(state): State<DashboardState>,
    Json(req): Json<UsBacktestRunRequest>,
) -> impl IntoResponse {
    use crate::us::backtest::{parse_and_validate_us_csv, run_us_backtest, UsBacktestConfig};

    let content_res = if let Some(content) = req.csv_content {
        Ok(content)
    } else {
        let path = req.csv_path.as_deref().unwrap_or("data/us_sample_data.csv");
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read CSV at {path}: {e}"))
    };

    let csv_content = match content_res {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let candles = match parse_and_validate_us_csv(&csv_content) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Invalid US OHLCV CSV: {e:#}") })),
            )
                .into_response();
        }
    };

    let mut config = UsBacktestConfig::default();
    if let Some(c) = req.initial_capital {
        config.initial_capital_usd = c;
    }
    if let Some(r) = req.risk_pct_per_trade {
        config.max_position_pct = r;
    }
    if let Some(s) = req.stop_loss_pct {
        config.stop_loss_pct = s;
    }
    if let Some(t) = req.take_profit_pct {
        config.take_profit_pct = t;
    }

    let run_name = req.name.unwrap_or_else(|| {
        format!(
            "US Backtest {}",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
        )
    });

    let result = match run_us_backtest(&candles, &config) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Backtest execution failed: {e:#}") })),
            )
                .into_response();
        }
    };

    let id = format!("us-bt-{}", uuid::Uuid::new_v4().simple());
    let config_json = serde_json::to_string(&config).unwrap_or_default();
    let results_json = serde_json::to_string(&result).unwrap_or_default();

    if let Err(e) = state
        .store
        .save_us_backtest(&id, &run_name, &config_json, &results_json, "COMPLETED")
        .await
    {
        warn!(error = %e, "Failed to persist US backtest run");
    }

    Json(serde_json::to_value(&result).unwrap_or_default()).into_response()
}

/// Calculates cumulative realized profit, capital tied up in open positions,
/// remaining available cash, and current holdings in the US paper trading account.
///
/// Starting capital defaults to strictly $5.00 USD (configured via `us_capital_allocation_usd`).
/// Profit from sold positions is immediately compounded back into `available_cash`.
/// Indian market money (INR) is completely isolated by architecture and cannot be used here.
fn compute_us_paper_accounting(
    base_capital: Decimal,
    orders: &[crate::db::store::UsPaperOrderRecord],
) -> (
    Decimal,
    Decimal,
    Decimal,
    std::collections::HashMap<String, Decimal>,
) {
    use std::str::FromStr;
    let mut realized_profit = Decimal::ZERO;
    let mut open_positions_cost = Decimal::ZERO;
    let mut holdings: std::collections::HashMap<String, Vec<(Decimal, Decimal)>> =
        std::collections::HashMap::new();

    for o in orders.iter().rev() {
        // In real-world market trading, only FILLED orders execute and produce holdings/PnL.
        // Limit orders resting on the book (ACCEPTED, NEW, PENDING_NEW) or CANCELED orders do not execute.
        if !o.status.eq_ignore_ascii_case("FILLED") {
            continue;
        }
        let qty = Decimal::from(o.fill_quantity);
        if qty.is_zero() {
            continue;
        }
        let fill_price = o
            .average_fill_price
            .as_deref()
            .and_then(|p| Decimal::from_str(p).ok())
            .unwrap_or_else(|| Decimal::from_str(&o.price).unwrap_or(Decimal::ZERO));

        if o.side.eq_ignore_ascii_case("BUY") {
            holdings
                .entry(o.symbol.to_uppercase())
                .or_default()
                .push((fill_price, qty));
        } else if o.side.eq_ignore_ascii_case("SELL") {
            if let Some(buys) = holdings.get_mut(&o.symbol.to_uppercase()) {
                let mut remaining_to_sell = qty;
                while remaining_to_sell > Decimal::ZERO && !buys.is_empty() {
                    let (buy_p, buy_q) = buys.pop().unwrap();
                    if buy_q <= remaining_to_sell {
                        realized_profit += (fill_price - buy_p) * buy_q;
                        remaining_to_sell -= buy_q;
                    } else {
                        realized_profit += (fill_price - buy_p) * remaining_to_sell;
                        buys.push((buy_p, buy_q - remaining_to_sell));
                        remaining_to_sell = Decimal::ZERO;
                    }
                }
            }
        }
    }

    let mut current_quantities: std::collections::HashMap<String, Decimal> =
        std::collections::HashMap::new();
    for (sym, buys) in holdings.iter() {
        for (p, q) in buys {
            open_positions_cost += *p * *q;
            *current_quantities.entry(sym.clone()).or_default() += *q;
        }
    }

    let available_cash = (base_capital + realized_profit - open_positions_cost).max(Decimal::ZERO);
    (
        realized_profit,
        open_positions_cost,
        available_cash,
        current_quantities,
    )
}

async fn us_paper_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let recent_orders = state
        .store
        .get_us_paper_orders(100)
        .await
        .unwrap_or_default();
    let watchlist = crate::us::instruments::us_watchlist();
    let base_capital = state.dynamic_caps.read().await.us_capital_allocation_usd;

    let (realized_profit, open_positions_cost, available_cash, holdings) =
        compute_us_paper_accounting(base_capital, &recent_orders);

    let total_equity = available_cash + open_positions_cost;

    let alpaca_feed_status = serde_json::json!({
        "connected": true,
        "feed_source": "Alpaca Stock Data WebSocket V2",
        "feed_type": state.alpaca_feed.feed_type.read().await.clone(),
        "mode": "Simulated Paper Trading (Backend-enforced)",
        "status": "ACTIVE",
        "starting_capital_usd": base_capital,
    });

    let resp = serde_json::json!({
        "venue_id": "alpaca",
        "name": "Alpaca US Paper Trading (Stocks & ETFs)",
        "currency": "USD",
        "currency_symbol": "$",
        "timezone": "America/New_York",
        "trading_hours": "09:30 - 16:00 ET",
        "allowlisted_host": "paper-api.alpaca.markets",
        "live_trading_allowed": false,
        "alpaca_account": alpaca_feed_status,
        "mode_description": "Live paper market execution. Orders execute in local paper simulator against real-time market data.",
        "base_capital_usd": base_capital,
        "realized_profit_usd": realized_profit,
        "open_positions_cost_usd": open_positions_cost,
        "available_cash_usd": available_cash,
        "total_equity_usd": total_equity,
        "holdings": holdings,
        "recent_orders": recent_orders,
        "watchlist": watchlist,
    });

    Json(resp)
}

async fn us_paper_reset_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let base_capital = state.dynamic_caps.read().await.us_capital_allocation_usd;
    match state.store.clear_us_paper_orders().await {
        Ok(_) => {
            let _ = sqlx::query(
                "UPDATE trades SET status = 'CLOSED', close_reason = 'paper_reset', closed_at = ? WHERE venue_id = 'alpaca' AND status IN ('OPEN', 'PARTIAL')"
            )
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(state.store.pool())
            .await;

            info!(base_capital = %base_capital, "US paper orders cleared - reset to base capital");
            Json(serde_json::json!({
                "status": "success",
                "message": format!("US market paper orders reset successfully. Starting capital reset to ${:.2} USD.", base_capital),
                "base_capital_usd": base_capital,
                "available_cash_usd": base_capital,
                "realized_profit_usd": 0.0,
                "open_positions_cost_usd": 0.0,
                "total_equity_usd": base_capital
            })).into_response()
        }
        Err(e) => {
            warn!("Failed to clear US paper orders: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    serde_json::json!({ "error": format!("Failed to reset US paper orders: {e}") }),
                ),
            )
                .into_response()
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct UsPaperOrderSubmitRequest {
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub quantity: i64,
    pub price: Option<Decimal>,
    pub allow_stale: Option<bool>,
}

async fn us_paper_order_handler(
    State(state): State<DashboardState>,
    Json(req): Json<UsPaperOrderSubmitRequest>,
) -> impl IntoResponse {
    let client_order_id = format!("US-PPR-{}", uuid::Uuid::new_v4().simple());
    let now = chrono::Utc::now().to_rfc3339();

    // Session gating: check US market regular trading hours
    let session = crate::us::session::detect_us_session(chrono::Utc::now());
    if !session.allows_order_entry && !req.allow_stale.unwrap_or(false) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "Market closed — strategy paused: US regular session is closed (session: {}). Order rejected.",
                    session.session_name
                ),
                "session": session.session_name,
                "strategy_paused": true,
            })),
        )
            .into_response();
    }

    let instrument = crate::us::instruments::find_us_instrument(&req.symbol);
    if instrument.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Symbol {:?} not in verified US watchlist", req.symbol)
            })),
        )
            .into_response();
    }

    if req.quantity <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Quantity must be a positive integer"
            })),
        )
            .into_response();
    }

    let is_market = req.order_type.eq_ignore_ascii_case("MARKET");
    let is_buy = req.side.eq_ignore_ascii_case("BUY");
    let is_sell = req.side.eq_ignore_ascii_case("SELL");

    // Price determination with live feed & slippage
    let live_price_opt = state.alpaca_feed.get_price(&req.symbol).await;
    let base_price =
        live_price_opt.unwrap_or_else(|| crate::us::chart::get_us_company_info(&req.symbol).3);
    let mut order_price = match req.price {
        Some(p) if p > Decimal::ZERO => p,
        _ => base_price,
    };

    // Apply simulated execution slippage for market orders: +0.02% for buy, -0.02% for sell
    if is_market {
        let slippage = order_price * rust_decimal_macros::dec!(0.0002);
        order_price = if is_buy {
            order_price + slippage
        } else {
            (order_price - slippage).max(rust_decimal_macros::dec!(0.01))
        };
    }
    let price_str = order_price.round_dp(4).to_string();

    let base_capital = state.dynamic_caps.read().await.us_capital_allocation_usd;
    let all_orders = state
        .store
        .get_us_paper_orders(500)
        .await
        .unwrap_or_default();
    let (realized_profit, _open_positions_cost, available_cash, holdings) =
        compute_us_paper_accounting(base_capital, &all_orders);

    let total_notional = order_price * Decimal::from(req.quantity);

    // US Statutory regulatory fees: SEC Section 31 ($27.80 per million) + FINRA TAF ($0.000166 per share, max $8.30) on sells
    let (sec_fee, finra_taf) = if is_sell {
        let sec = (total_notional * rust_decimal_macros::dec!(0.0000278)).round_dp(4);
        let taf = (Decimal::from(req.quantity) * rust_decimal_macros::dec!(0.000166))
            .min(rust_decimal_macros::dec!(8.30))
            .round_dp(4);
        (sec, taf)
    } else {
        (Decimal::ZERO, Decimal::ZERO)
    };

    if is_buy {
        let order_cost = total_notional;
        if order_cost > available_cash {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "Insufficient US market cash. US trading is strictly restricted to initial $5.00 capital + realized profits. Available cash: ${:.2}, but order cost is ${:.2}. Indian market funds (₹) are completely isolated and cannot be used.",
                        available_cash,
                        order_cost
                    ),
                    "available_cash_usd": available_cash,
                    "order_cost_usd": order_cost,
                    "base_capital_usd": base_capital,
                    "realized_profit_usd": realized_profit,
                })),
            )
                .into_response();
        }
    } else if is_sell {
        let held_qty = holdings
            .get(&req.symbol.to_uppercase())
            .cloned()
            .unwrap_or(Decimal::ZERO);
        let requested_qty = Decimal::from(req.quantity);
        if requested_qty > held_qty {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "Cannot sell {} shares of {}. Current open position held in US paper account is only {} shares.",
                        req.quantity,
                        req.symbol,
                        held_qty
                    ),
                    "current_holding": held_qty,
                })),
            )
                .into_response();
        }
    }

    let venue_order_id = Some(format!("US-SIM-{}", uuid::Uuid::new_v4().simple()));
    let order_status = if is_market {
        "FILLED".to_string()
    } else {
        "OPEN".to_string()
    };
    let fill_qty = if is_market { req.quantity } else { 0i64 };
    let avg_fill_price = if is_market {
        Some(price_str.clone())
    } else {
        None
    };

    let order_record = crate::db::store::UsPaperOrderRecord {
        id: format!("ord-{}", uuid::Uuid::new_v4().simple()),
        client_order_id: client_order_id.clone(),
        venue_order_id,
        symbol: req.symbol.to_uppercase(),
        side: req.side.to_uppercase(),
        order_type: req.order_type.to_uppercase(),
        quantity: req.quantity,
        price: price_str.clone(),
        trigger_price: None,
        status: order_status,
        fill_quantity: fill_qty,
        average_fill_price: avg_fill_price,
        rejection_reason: None,
        created_at: now.clone(),
        updated_at: now,
    };

    if let Err(e) = state.store.save_us_paper_order(&order_record).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to record US paper order: {e}") })),
        )
            .into_response();
    }

    let mut updated_orders = all_orders.clone();
    updated_orders.insert(0, order_record.clone());
    let (new_realized_profit, new_open_cost, new_available_cash, new_holdings) =
        compute_us_paper_accounting(base_capital, &updated_orders);

    Json(serde_json::json!({
        "status": "success",
        "order": order_record,
        "fees": {
            "sec_section_31_usd": sec_fee,
            "finra_taf_usd": finra_taf,
            "total_regulatory_fees_usd": sec_fee + finra_taf,
        },
        "base_capital_usd": base_capital,
        "realized_profit_usd": new_realized_profit,
        "open_positions_cost_usd": new_open_cost,
        "available_cash_usd": new_available_cash,
        "total_equity_usd": new_available_cash + new_open_cost,
        "holdings": new_holdings,
        "disclaimer": "Simulated paper trading only. Backend-enforced safety: no live Alpaca order APIs called."
    }))
    .into_response()
}

async fn fx_rate_handler() -> impl IntoResponse {
    let fx = crate::market::fx::get_current_fx_rate();
    Json(serde_json::to_value(&fx).unwrap_or_default())
}

async fn combined_portfolio_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    use std::str::FromStr;
    let fx = crate::market::fx::get_current_fx_rate();
    let _trades = state
        .store
        .get_open_venue_trades()
        .await
        .unwrap_or_default();

    let india_unrealized_inr = Decimal::ZERO;
    let us_unrealized_usd = Decimal::ZERO;

    let resolved_trades = state.store.get_resolved_trades().await.unwrap_or_default();
    let mut india_realized_inr = Decimal::ZERO;
    let mut us_realized_usd = Decimal::ZERO;
    for rt in &resolved_trades {
        let pnl = rt
            .pnl
            .as_deref()
            .and_then(|p| Decimal::from_str(p).ok())
            .unwrap_or(Decimal::ZERO);
        us_realized_usd += pnl;
    }

    // Compute India sandbox realized P&L using unified accounting (₹1,000 base + compounded profits)
    let india_orders = state
        .store
        .get_india_sandbox_orders(100)
        .await
        .unwrap_or_default();
    let (india_base_capital, us_base_capital) = {
        let caps = state.dynamic_caps.read().await;
        (
            caps.india_capital_allocation_inr,
            caps.us_capital_allocation_usd,
        )
    };
    let india_portfolio = compute_india_sandbox_accounting(
        india_base_capital,
        &india_orders,
        &std::collections::HashMap::new(),
    );
    india_realized_inr += india_portfolio.realized_profit_inr;

    // Compute US paper realized P&L using unified accounting (base + compounded profits)
    let us_orders = state
        .store
        .get_us_paper_orders(100)
        .await
        .unwrap_or_default();
    let (us_paper_realized_profit, _us_open_cost, _us_cash, _us_holdings) =
        compute_us_paper_accounting(us_base_capital, &us_orders);
    us_realized_usd += us_paper_realized_profit;

    let india_costs_inr = Decimal::ZERO;
    let us_costs_usd = state
        .store
        .get_total_api_cost()
        .await
        .unwrap_or(Decimal::ZERO);

    let india_equity_inr = india_base_capital + india_realized_inr + india_unrealized_inr;
    let us_equity_usd = us_base_capital + us_realized_usd + us_unrealized_usd;

    let summary = crate::market::fx::CombinedPortfolioSummary::calculate(
        &fx,
        india_equity_inr,
        india_realized_inr,
        india_unrealized_inr,
        india_costs_inr,
        us_equity_usd,
        us_realized_usd,
        us_unrealized_usd,
        us_costs_usd,
    );

    Json(serde_json::to_value(&summary).unwrap_or_default())
}

async fn intraday_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let now = chrono::Utc::now();
    let nse_session = crate::venue::session::TradingSession::nse();
    let us_session = crate::venue::session::TradingSession::us_equity_regular();

    let nse_state = nse_session.state_at(now);
    let us_state = us_session.state_at(now);

    let nse_close = nse_session.current_session_close(now);
    let us_close = us_session.current_session_close(now);

    let nse_entry_permitted = nse_session.entry_permitted_at(now, 30);
    let us_entry_permitted = us_session.entry_permitted_at(now, 30);

    let nse_exit_due = nse_session.is_intraday_exit_due(now, 15);
    let us_exit_due = us_session.is_intraday_exit_due(now, 15);

    let ist = chrono_tz::Asia::Kolkata;
    let et = chrono_tz::America::New_York;

    let now_ist = now
        .with_timezone(&ist)
        .format("%Y-%m-%d %H:%M:%S IST")
        .to_string();
    let now_et = now
        .with_timezone(&et)
        .format("%Y-%m-%d %H:%M:%S ET")
        .to_string();

    let fx = crate::market::fx::get_current_fx_rate();
    let (cap_inr, cap_usd) = {
        let caps = state.dynamic_caps.read().await;
        (
            caps.india_capital_allocation_inr,
            caps.us_capital_allocation_usd,
        )
    };

    Json(serde_json::json!({
        "timestamp": now.to_rfc3339(),
        "emergency_stop_active": state.kill_switch.is_tripped() || state.risk.emergency_stop,
        "fx_rate": fx,
        "india": {
            "market": "NSE Cash Equities",
            "currency": "INR",
            "currency_symbol": "₹",
            "timezone": "Asia/Kolkata",
            "local_time": now_ist,
            "session_state": match nse_state {
                crate::venue::session::SessionState::Open(_) => "OPEN",
                crate::venue::session::SessionState::Closed { .. } => "CLOSED",
            },
            "entry_permitted": nse_entry_permitted,
            "exit_due": nse_exit_due,
            "session_close": nse_close.map(|dt| dt.to_rfc3339()),
            "test_mode": "Historical Backtest & Upstox Sandbox (Live Blocked)",
            "live_orders_blocked": true,
            "capital_allocation_inr": cap_inr,
            "max_exposure_pct": state.risk.india_max_exposure_pct,
            "max_daily_loss_inr": state.risk.india_max_daily_loss_inr,
        },
        "us": {
            "market": "US Equities & ETFs",
            "currency": "USD",
            "currency_symbol": "$",
            "timezone": "America/New_York",
            "local_time": now_et,
            "session_state": match us_state {
                crate::venue::session::SessionState::Open(_) => "OPEN",
                crate::venue::session::SessionState::Closed { .. } => "CLOSED",
            },
            "entry_permitted": us_entry_permitted,
            "exit_due": us_exit_due,
            "session_close": us_close.map(|dt| dt.to_rfc3339()),
            "test_mode": "Historical Backtest & Alpaca Paper (Live Blocked)",
            "live_orders_blocked": true,
            "capital_allocation_usd": cap_usd,
            "max_exposure_pct": state.risk.us_max_exposure_pct,
            "max_daily_loss_usd": state.risk.us_max_daily_loss_usd,
        }
    }))
}

fn deserialize_flexible_decimal<'de, D>(deserializer: D) -> Result<Option<Decimal>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let opt = Option::<serde_json::Value>::deserialize(deserializer)?;
    match opt {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                Ok(Some(Decimal::from(i)))
            } else if let Some(u) = n.as_u64() {
                Ok(Some(Decimal::from(u)))
            } else if let Some(f) = n.as_f64() {
                Decimal::from_f64_retain(f)
                    .map(Some)
                    .ok_or_else(|| serde::de::Error::custom("invalid float decimal"))
            } else {
                Err(serde::de::Error::custom("invalid number"))
            }
        }
        Some(serde_json::Value::String(s)) => s
            .parse::<Decimal>()
            .map(Some)
            .map_err(serde::de::Error::custom),
        _ => Err(serde::de::Error::custom("expected number or string")),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct UpdateCapsRequest {
    #[serde(default, deserialize_with = "deserialize_flexible_decimal")]
    pub india_capital_allocation_inr: Option<Decimal>,
    #[serde(default, deserialize_with = "deserialize_flexible_decimal")]
    pub us_capital_allocation_usd: Option<Decimal>,
    pub india_enabled: Option<bool>,
    pub us_enabled: Option<bool>,
    pub polymarket_enabled: Option<bool>,
}

#[derive(Debug, serde::Deserialize)]
pub struct VenueToggleRequest {
    pub venue: String,
    pub enabled: bool,
}

#[derive(Debug, serde::Deserialize)]
pub struct LogsQuery {
    pub lines: Option<usize>,
}

async fn venue_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let caps = state.dynamic_caps.read().await;
    let halted = state.kill_switch.is_tripped();
    Json(serde_json::json!({
        "master_halted": halted,
        "india": {
            "name": "Upstox NSE Cash Equities",
            "enabled": caps.india_enabled,
            "capital_inr": caps.india_capital_allocation_inr,
            "currency": "INR",
            "status": if halted { "HALTED" } else if caps.india_enabled { "RUNNING" } else { "PAUSED" }
        },
        "us": {
            "name": "Alpaca US Equities",
            "enabled": caps.us_enabled,
            "capital_usd": caps.us_capital_allocation_usd,
            "currency": "USD",
            "status": if halted { "HALTED" } else if caps.us_enabled { "RUNNING" } else { "PAUSED" }
        },
        "polymarket": {
            "name": "Polymarket Prediction Markets",
            "enabled": caps.polymarket_enabled,
            "capital_usd": state.initial_bankroll,
            "currency": "USD",
            "status": if halted { "HALTED" } else if caps.polymarket_enabled { "RUNNING" } else { "PAUSED" }
        }
    }))
}

async fn venue_toggle_handler(
    State(state): State<DashboardState>,
    Json(req): Json<VenueToggleRequest>,
) -> impl IntoResponse {
    let mut caps = state.dynamic_caps.write().await;
    let newly_enabled = match req.venue.to_lowercase().as_str() {
        "india" | "nse" | "upstox" => {
            caps.india_enabled = req.enabled;
            req.enabled
        }
        "us" | "alpaca" => {
            caps.us_enabled = req.enabled;
            req.enabled
        }
        "polymarket" | "poly" => {
            caps.polymarket_enabled = req.enabled;
            req.enabled
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Unknown venue '{}'", req.venue) })),
            )
                .into_response();
        }
    };

    if newly_enabled && state.kill_switch.is_tripped() {
        let _ = state.kill_switch.clear();
    }

    Json(serde_json::json!({
        "status": "success",
        "venue": req.venue,
        "enabled": req.enabled,
        "master_halted": state.kill_switch.is_tripped(),
    }))
    .into_response()
}

async fn logs_handler(axum::extract::Query(query): axum::extract::Query<LogsQuery>) -> impl IntoResponse {
    const LOG_PATH: &str = "/tmp/moneyai.log";
    const MAX_LINES: usize = 1_000;
    const READ_BYTES: u64 = 512 * 1024;

    let n = query.lines.unwrap_or(200).min(MAX_LINES);

    let lines: Vec<String> = tokio::task::spawn_blocking(move || -> Vec<String> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = match std::fs::File::open(LOG_PATH) {
            Ok(f) => f,
            Err(_) => return vec![format!("Log file not found: {LOG_PATH}")],
        };
        let file_len = f.seek(SeekFrom::End(0)).unwrap_or(0);
        let offset = file_len.saturating_sub(READ_BYTES);
        if f.seek(SeekFrom::Start(offset)).is_err() {
            return vec![];
        }
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_err() {
            return vec!["[error reading log file]".to_string()];
        }
        let content = if offset > 0 {
            buf.splitn(2, '\n').nth(1).unwrap_or("")
        } else {
            &buf
        };
        content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .take(n)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    })
    .await
    .unwrap_or_else(|_| vec!["[task failed]".to_string()]);

    Json(serde_json::json!({
        "lines": lines,
        "count": lines.len(),
    }))
}

async fn pro_terminal_handler() -> impl IntoResponse {
    let html = include_str!("../../static/apextrader.html");
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"),
            (header::PRAGMA, "no-cache"),
            (header::EXPIRES, "0"),
        ],
        html,
    )
}

async fn settings_caps_get_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let caps = state.dynamic_caps.read().await;
    Json(serde_json::json!({
        "india_capital_allocation_inr": caps.india_capital_allocation_inr,
        "us_capital_allocation_usd": caps.us_capital_allocation_usd,
        "india_enabled": caps.india_enabled,
        "us_enabled": caps.us_enabled,
        "polymarket_enabled": caps.polymarket_enabled,
        "default_india_capital_inr": 1000.0,
        "default_us_capital_usd": 5.0,
        "currency_inr": "₹",
        "currency_usd": "$"
    }))
}

async fn settings_caps_post_handler(
    State(state): State<DashboardState>,
    Json(req): Json<UpdateCapsRequest>,
) -> impl IntoResponse {
    let mut caps = state.dynamic_caps.write().await;
    if let Some(inr) = req.india_capital_allocation_inr {
        if inr <= Decimal::ZERO {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "NSE India capital allocation cap must be a positive number"
                })),
            )
                .into_response();
        }
        caps.india_capital_allocation_inr = inr;
    }
    if let Some(usd) = req.us_capital_allocation_usd {
        if usd <= Decimal::ZERO {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "US Equities capital allocation cap must be a positive number"
                })),
            )
                .into_response();
        }
        caps.us_capital_allocation_usd = usd;
    }
    if let Some(en) = req.india_enabled {
        caps.india_enabled = en;
    }
    if let Some(en) = req.us_enabled {
        caps.us_enabled = en;
    }
    if let Some(en) = req.polymarket_enabled {
        caps.polymarket_enabled = en;
    }

    info!(
        india_inr = %caps.india_capital_allocation_inr,
        us_usd = %caps.us_capital_allocation_usd,
        india_enabled = caps.india_enabled,
        us_enabled = caps.us_enabled,
        "Capital allocation caps updated via Settings"
    );

    Json(serde_json::json!({
        "status": "success",
        "message": "Capital allocation caps updated successfully. New limits applied to paper trading and header ribbon.",
        "india_capital_allocation_inr": caps.india_capital_allocation_inr,
        "us_capital_allocation_usd": caps.us_capital_allocation_usd,
        "india_enabled": caps.india_enabled,
        "us_enabled": caps.us_enabled,
        "polymarket_enabled": caps.polymarket_enabled,
    }))
    .into_response()
}

#[derive(Debug, serde::Deserialize)]
pub struct EmergencyStopRequest {
    pub stop: bool,
    pub reason: Option<String>,
}

async fn emergency_stop_handler(
    State(state): State<DashboardState>,
    Json(req): Json<EmergencyStopRequest>,
) -> impl IntoResponse {
    if req.stop {
        let reason = req
            .reason
            .unwrap_or_else(|| "Operator triggered account-wide emergency stop".to_string());
        state.kill_switch.trip(crate::agent::kill_switch::Halt::new(
            crate::agent::kill_switch::HaltSource::Api,
            crate::risk::circuit_breaker::HaltScope::UntilResume,
            reason,
            chrono::Utc::now(),
        ));
        warn!("Account-wide emergency stop triggered via API");
    } else {
        let _ = state.kill_switch.clear();
        let _ = state.store.clear_halts("api", chrono::Utc::now()).await;
        info!("Account-wide emergency stop cleared via API");
    }

    Json(serde_json::json!({
        "emergency_stop_active": state.kill_switch.is_tripped(),
        "status": if state.kill_switch.is_tripped() { "HALTED" } else { "ACTIVE" }
    }))
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

    /// One temp directory for the whole test binary.
    ///
    /// The HALT path has to outlive every `DashboardState` built from it, and
    /// the previous version bought that with `std::mem::forget` — which left
    /// a directory behind per test, per run, forever. A single `OnceLock`
    /// gives the same lifetime and is cleaned up with the process.
    ///
    /// Each switch gets a distinct file name so concurrent tests cannot see
    /// each other's HALT.
    fn halt_path() -> std::path::PathBuf {
        static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = DIR.get_or_init(|| tempfile::tempdir().expect("tempdir"));
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        dir.path().join(format!("HALT-{n}"))
    }

    async fn state_with_token(token: Option<&str>) -> DashboardState {
        let store = Store::new(":memory:").await.unwrap();
        let switch = Arc::new(KillSwitch::new(halt_path()));
        DashboardState::new(
            store,
            HealthState::new(),
            dec!(100),
            token.map(str::to_string),
            switch,
            crate::config::RiskConfig::default(),
            AgentMode::Paper,
            false,
            "minimaxai/minimax-m3.1-flash".to_string(),
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
        post_with(app, uri, auth, true).await
    }

    async fn post_with(
        app: Router,
        uri: &str,
        auth: Option<&str>,
        control_header: bool,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = HttpRequest::builder().method("POST").uri(uri);
        if let Some(a) = auth {
            req = req.header(header::AUTHORIZATION, a);
        }
        if control_header {
            req = req.header(CONTROL_HEADER, "1");
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
    /// A `POST` with no custom header is a CORS *simple request*: any page
    /// the operator browses can issue one at `localhost:8080`, the browser
    /// sends it, and the side effect lands even though the response is
    /// opaque. That is a drive-by kill switch — and, worse, a drive-by
    /// *resume* lifting the drawdown halt the README says only a person can
    /// clear. Requiring a header `fetch` will not send cross-origin without a
    /// preflight this server never answers is what closes it.
    #[tokio::test]
    async fn a_control_request_without_the_custom_header_is_refused() {
        let app = build_router(state_with_token(None).await);
        for route in ["/api/halt", "/api/resume", "/api/reconcile/ack"] {
            let (code, _) = post_with(app.clone(), route, None, false).await;
            assert_eq!(
                code,
                StatusCode::FORBIDDEN,
                "{route} must refuse a simple cross-site POST"
            );
        }
    }

    #[tokio::test]
    async fn the_control_routes_still_work_with_the_header() {
        let state = state_with_token(None).await;
        let switch = state.kill_switch.clone();
        let app = build_router(state);
        let (code, _) = post(app, "/api/halt", None).await;
        assert_eq!(code, StatusCode::OK);
        assert!(switch.is_tripped());
    }

    /// A browser that announces a foreign origin is refused outright, header
    /// or not — the header alone would not survive an attacker who can get a
    /// preflight answered by some future CORS layer.
    #[tokio::test]
    async fn a_cross_origin_control_request_is_refused() {
        let app = build_router(state_with_token(None).await);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/halt")
                    .header(header::HOST, "localhost:8080")
                    .header(header::ORIGIN, "https://evil.example")
                    .header(CONTROL_HEADER, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_same_origin_control_request_is_allowed() {
        let state = state_with_token(None).await;
        let switch = state.kill_switch.clone();
        let app = build_router(state);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/halt")
                    .header(header::HOST, "localhost:8080")
                    .header(header::ORIGIN, "http://localhost:8080")
                    .header(CONTROL_HEADER, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(switch.is_tripped());
    }

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
