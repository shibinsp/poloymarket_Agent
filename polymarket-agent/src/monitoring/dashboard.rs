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
use axum::routing::get;
use axum::Router;
use rust_decimal::Decimal;
use subtle::ConstantTimeEq;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::config::AgentMode;
use crate::db::store::Store;
use crate::monitoring::health::HealthState;
use crate::monitoring::metrics::compute_metrics;

const LOOPBACK: &str = "127.0.0.1";

/// Shared state accessible by all dashboard route handlers.
#[derive(Clone)]
pub struct DashboardState {
    store: Arc<Store>,
    health: HealthState,
    initial_bankroll: Decimal,
    api_token: Option<Arc<str>>,
}

impl DashboardState {
    pub fn new(
        store: Store,
        health: HealthState,
        initial_bankroll: Decimal,
        api_token: Option<String>,
    ) -> Self {
        Self {
            store: Arc::new(store),
            health,
            initial_bankroll,
            api_token: api_token
                .filter(|t| !t.trim().is_empty())
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

async fn health_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let data = state.health.to_json().await;
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
        let store = Store::new(":memory:").await.unwrap();
        let state = DashboardState::new(
            store,
            HealthState::new(),
            dec!(100),
            token.map(str::to_string),
        );
        build_router(state)
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
