//! Agent health state, served by the dashboard at `GET /api/health`.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
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
    /// When the next cycle is expected to have completed. A watchdog compares
    /// the clock against this rather than against a fixed multiple of the
    /// cycle interval: with a session-aware scheduler a legitimate sleep runs
    /// to `max_sleep_seconds`, and a fixed multiple would call every quiet
    /// night a stall.
    next_cycle_due: Option<DateTime<Utc>>,
    /// Whether alert delivery itself is failing, so an operator can tell the
    /// difference between nothing going wrong and nothing getting through.
    alerts_delivering: bool,
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
                next_cycle_due: None,
                alerts_delivering: true,
            })),
        }
    }

    /// Get health data as a serializable JSON value.
    pub async fn to_json(&self) -> serde_json::Value {
        self.to_json_at(Utc::now()).await
    }

    async fn to_json_at(&self, now: DateTime<Utc>) -> serde_json::Value {
        let data = self.inner.read().await;
        let mut value =
            serde_json::to_value(&*data).unwrap_or(serde_json::json!({"status": "error"}));

        // Uptime follows the clock, not the last write. Recomputing it only
        // when a cycle completes left it reading zero for as long as the first
        // cycle took — and a cycle that never completes is exactly when
        // somebody loads this endpoint to find out what is wrong.
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "uptime_seconds".to_string(),
                serde_json::json!((now - data.started_at).num_seconds()),
            );
        }
        value
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
        // Distinct from "ok": an uptime probe that cannot tell a trading
        // agent from a halted one is not monitoring anything.
        data.status = match state {
            AgentState::Dead => "dead".to_string(),
            AgentState::Halted => "halted".to_string(),
            _ => "ok".to_string(),
        };
    }

    /// Record when the next cycle should have completed by.
    pub async fn expect_cycle_by(&self, at: DateTime<Utc>) {
        self.inner.write().await.next_cycle_due = Some(at);
    }

    /// Record whether alerts are currently reaching their destination.
    pub async fn record_alert_delivery(&self, delivering: bool) {
        self.inner.write().await.alerts_delivering = delivering;
    }

    /// How late the next cycle is, or `None` if it is not yet due.
    pub async fn overdue_by(&self, now: DateTime<Utc>) -> Option<Duration> {
        let due = self.inner.read().await.next_cycle_due?;
        (now > due).then(|| now - due)
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

    fn at(minute: i64) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 9, 19, 12, 0, 0).unwrap() + Duration::minutes(minute)
    }

    /// Uptime has to advance without a completed cycle. It previously only
    /// moved when record_cycle or record_failure wrote it, so a process stuck
    /// in its first cycle reported zero indefinitely.
    #[tokio::test]
    async fn uptime_advances_without_a_completed_cycle() {
        let state = HealthState::new();
        let started: DateTime<Utc> =
            serde_json::from_value(state.to_json().await.get("started_at").unwrap().clone())
                .unwrap();

        let json = state.to_json_at(started + Duration::seconds(137)).await;
        assert_eq!(json["uptime_seconds"], 137);
        // And no cycle has been recorded, which is the situation under test.
        assert!(json["last_cycle_at"].is_null());
    }

    #[tokio::test]
    async fn a_cycle_that_is_not_yet_due_is_not_overdue() {
        let state = HealthState::new();
        state.expect_cycle_by(at(10)).await;
        assert_eq!(state.overdue_by(at(9)).await, None);
        assert_eq!(state.overdue_by(at(10)).await, None);
    }

    #[tokio::test]
    async fn overdue_reports_how_late_the_cycle_is() {
        let state = HealthState::new();
        state.expect_cycle_by(at(10)).await;
        assert_eq!(state.overdue_by(at(13)).await, Some(Duration::minutes(3)));
    }

    /// Before the loop has scheduled anything there is nothing to be late for,
    /// so a freshly started agent must not be reported as stalled.
    #[tokio::test]
    async fn nothing_is_overdue_before_a_cycle_has_been_scheduled() {
        let state = HealthState::new();
        assert_eq!(state.overdue_by(at(1000)).await, None);
    }

    /// The whole point of anchoring to the schedule: a long closed-market
    /// sleep is not a stall, however far past the cycle interval it runs.
    #[tokio::test]
    async fn a_long_scheduled_sleep_is_not_overdue() {
        let state = HealthState::new();
        // An hour-long weekend sleep, checked forty minutes in.
        state.expect_cycle_by(at(60)).await;
        assert_eq!(state.overdue_by(at(40)).await, None);
    }

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
