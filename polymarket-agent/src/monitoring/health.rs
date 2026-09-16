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

    pub fn record_cycle(&self, cycle_number: u64, state: AgentState) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut data = inner.write().await;
            data.cycle_number = cycle_number;
            data.agent_state = state.to_string();
            data.last_cycle_at = Some(Utc::now());
            data.uptime_seconds = (Utc::now() - data.started_at).num_seconds();
            data.status = if state == AgentState::Dead {
                "dead".to_string()
            } else {
                "ok".to_string()
            };
        });
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
        state.record_cycle(5, AgentState::Alive);

        // Give the spawned task time to complete
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let data = state.inner.read().await;
        assert_eq!(data.cycle_number, 5);
        assert_eq!(data.agent_state, "ALIVE");
        assert_eq!(data.status, "ok");
    }

    #[tokio::test]
    async fn test_health_state_dead() {
        let state = HealthState::new();
        state.record_cycle(10, AgentState::Dead);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let data = state.inner.read().await;
        assert_eq!(data.status, "dead");
        assert_eq!(data.agent_state, "DEAD");
    }
}
