//! Web dashboard — axum HTTP server serving REST API + embedded HTML.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use rust_decimal::Decimal;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::db::store::Store;
use crate::monitoring::health::HealthState;
use crate::monitoring::metrics::compute_metrics;

/// Shared state accessible by all dashboard route handlers.
#[derive(Clone)]
pub struct DashboardState {
    store: Arc<Store>,
    health: HealthState,
    initial_bankroll: Decimal,
}

impl DashboardState {
    pub fn new(store: Store, health: HealthState, initial_bankroll: Decimal) -> Self {
        Self {
            store: Arc::new(store),
            health,
            initial_bankroll,
        }
    }
}

/// Spawn the dashboard HTTP server. Returns a handle that can be aborted.
pub fn spawn_dashboard(state: DashboardState, bind: &str, port: u16) -> JoinHandle<()> {
    let addr = format!("{bind}:{port}");
    let addr_clone = addr.clone();

    tokio::spawn(async move {
        let app = Router::new()
            .route("/", get(index_handler))
            .route("/api/health", get(health_handler))
            .route("/api/metrics", get(metrics_handler))
            .route("/api/trades", get(trades_handler))
            .route("/api/trades/all", get(trades_all_handler))
            .route("/api/cycles", get(cycles_latest_handler))
            .route("/api/cycles/all", get(cycles_all_handler))
            .route("/api/costs", get(costs_handler))
            .with_state(state);

        let listener = match tokio::net::TcpListener::bind(&addr_clone).await {
            Ok(l) => {
                info!(addr = %addr_clone, "Dashboard server listening");
                l
            }
            Err(e) => {
                warn!(error = %e, addr = %addr_clone, "Failed to bind dashboard server");
                return;
            }
        };

        if let Err(e) = axum::serve(listener, app).await {
            warn!(error = %e, "Dashboard server error");
        }
    })
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
