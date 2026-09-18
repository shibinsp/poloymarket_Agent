//! Agent health state, served by the dashboard at `GET /api/health`.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::RwLock;

use crate::market::models::AgentState;

/// Shared health state updated by the agent loop.
#[derive(Clone)]
pub struct HealthState {
    inner: Arc<RwLock<HealthData>>,
}

#[derive(Debug, Clone, Serialize)]
struct HealthData {
    status: String,
    agent_state: String,
    cycle_number: u64,
    started_at: DateTime<Utc>,
    last_cycle_at: Option<DateTime<Utc>>,
    uptime_seconds: i64,
}

impl HealthState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HealthData {
                status: "ok".to_string(),
                agent_state: "INITIALIZING".to_string(),
                cycle_number: 0,
                started_at: Utc::now(),
                last_cycle_at: None,
                uptime_seconds: 0,
            })),
        }
    }

    /// Get health data as a serializable JSON value.
    pub async fn to_json(&self) -> serde_json::Value {
        let data = self.inner.read().await;
        serde_json::to_value(&*data).unwrap_or(serde_json::json!({"status": "error"}))
    }

    /// Awaited rather than spawned: the caller breaks out of the main loop
    /// immediately after recording a death or a fatal failure run, and a
    /// detached task could lose that final write to runtime teardown.
    pub async fn record_cycle(&self, cycle_number: u64, state: AgentState) {
        let mut data = self.inner.write().await;
        data.cycle_number = cycle_number;
        data.agent_state = state.to_string();
        data.last_cycle_at = Some(Utc::now());
        data.uptime_seconds = (Utc::now() - data.started_at).num_seconds();
        data.status = if state == AgentState::Dead {
            "dead".to_string()
        } else {
            "ok".to_string()
        };
    }

    /// Mark a failed cycle so an external monitor sees it, without touching
    /// `cycle_number`/`last_cycle_at` — those still reflect the last cycle
    /// that actually completed.
    pub async fn record_failure(&self) {
        let mut data = self.inner.write().await;
        data.uptime_seconds = (Utc::now() - data.started_at).num_seconds();
        if data.status != "dead" {
            data.status = "degraded".to_string();
        }
    }
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_state_creation() {
        let state = HealthState::new();
        // Should be constructable without async runtime
        let _ = state.clone();
    }

    #[tokio::test]
    async fn test_health_state_update() {
        let state = HealthState::new();
        state.record_cycle(5, AgentState::Alive).await;

        let data = state.inner.read().await;
        assert_eq!(data.cycle_number, 5);
        assert_eq!(data.agent_state, "ALIVE");
        assert_eq!(data.status, "ok");
    }

    #[tokio::test]
    async fn test_health_state_record_failure() {
        let state = HealthState::new();
        state.record_cycle(3, AgentState::Alive).await;
        state.record_failure().await;

        let data = state.inner.read().await;
        assert_eq!(data.status, "degraded");
        // The last successful cycle's data is preserved, not overwritten.
        assert_eq!(data.cycle_number, 3);
    }

    #[tokio::test]
    async fn test_health_state_dead() {
        let state = HealthState::new();
        state.record_cycle(10, AgentState::Dead).await;

        let data = state.inner.read().await;
        assert_eq!(data.status, "dead");
        assert_eq!(data.agent_state, "DEAD");
    }
}
