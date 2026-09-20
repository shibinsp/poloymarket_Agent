//! Discord/Telegram alert system.
//!
//! Sends notifications via Discord webhooks for trade events,
//! state changes, and daily summaries.
//!
//! Alongside those there are *anomalies*: the agent noticing that something
//! about its own operation is wrong. Trade alerts are a nice-to-have; an
//! anomaly is the only reason an operator finds out that orders are being
//! rejected at 3am. They are deduped, because the failures worth alerting on
//! are exactly the ones that repeat every cycle, and an operator who is paged
//! three hundred times stops reading.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use tracing::{error, warn};

use crate::market::models::{AgentState, Side};
use crate::monitoring::metrics::PerformanceMetrics;

/// How long the same anomaly stays quiet after firing.
const DEDUPE_WINDOW: Duration = Duration::minutes(30);

/// How loud an anomaly is. Distinct from the anomaly itself because the same
/// kind changes urgency with context: one rejected order is a notice, a venue
/// rejecting everything is critical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertLevel {
    Notice,
    Warning,
    Critical,
}

impl AlertLevel {
    fn label(&self) -> &'static str {
        match self {
            AlertLevel::Notice => "NOTICE",
            AlertLevel::Warning => "WARNING",
            AlertLevel::Critical => "CRITICAL",
        }
    }
}

/// Something wrong with the agent's own operation, as opposed to a trading
/// outcome. Each variant is a dedupe bucket, so they are split by *what an
/// operator would do about it* rather than by where in the code they arise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnomalyKind {
    /// The loop has not completed a cycle when one was due.
    StalledCycle,
    /// A venue failed repeatedly — unreachable, refusing auth, or erroring.
    VenueUnreachable,
    /// A write to SQLite failed. The ledger is now behind reality.
    DbWriteFailed,
    /// The daily valuation budget is gone; no new views until it resets.
    BudgetExhausted,
    /// Local records and the venue disagree about positions or balance.
    ReconciliationMismatch,
    /// The venue refused an order.
    OrderRejected,
    /// An order's fate is unknown — a submit that timed out may still have
    /// been accepted, so this is never treated as "did not happen".
    OrderStateUnknown,
    /// An order has sat unresolved past the point where it should have been
    /// filled or cancelled.
    StaleOrder,
}

impl AnomalyKind {
    /// Stable identifier — used as a dedupe key and reported on /api/health,
    /// so it must not drift with the Debug formatting.
    pub fn as_str(&self) -> &'static str {
        match self {
            AnomalyKind::StalledCycle => "stalled_cycle",
            AnomalyKind::VenueUnreachable => "venue_unreachable",
            AnomalyKind::DbWriteFailed => "db_write_failed",
            AnomalyKind::BudgetExhausted => "budget_exhausted",
            AnomalyKind::ReconciliationMismatch => "reconciliation_mismatch",
            AnomalyKind::OrderRejected => "order_rejected",
            AnomalyKind::OrderStateUnknown => "order_state_unknown",
            AnomalyKind::StaleOrder => "stale_order",
        }
    }
}

/// Discord webhook client.
pub struct AlertClient {
    webhook_url: Option<String>,
    http: reqwest::Client,
    enabled: bool,
    /// When each `(kind, scope)` last fired. Keyed by scope as well as kind so
    /// one dead venue cannot mask a second one going down behind the same
    /// 30-minute window.
    recent: Mutex<HashMap<(AnomalyKind, String), DateTime<Utc>>>,
    /// Whether the last delivery attempt failed. Alert delivery is its own
    /// failure domain: if the webhook is down, silence means nothing, so the
    /// health endpoint has to be able to say so.
    delivery_failing: AtomicBool,
}

/// Discord webhook message format.
#[derive(Debug, Serialize)]
struct DiscordMessage {
    content: String,
    username: String,
}

impl AlertClient {
    pub fn new(webhook_url: Option<String>, enabled: bool) -> Self {
        // A cycle is never cancelled mid-flight (see main.rs), so every call
        // made inside one must be time-boxed or a stalled endpoint wedges the
        // whole agent. Alerts are the least critical call in a cycle, so they
        // get the shortest timeout.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            enabled: enabled && webhook_url.is_some(),
            webhook_url,
            http,
            recent: Mutex::new(HashMap::new()),
            delivery_failing: AtomicBool::new(false),
        }
    }

    /// Send a raw message to Discord.
    async fn send(&self, message: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let Some(ref url) = self.webhook_url else {
            return Ok(());
        };

        let payload = DiscordMessage {
            content: message.to_string(),
            username: "Polymarket Agent".to_string(),
        };

        // A delivery failure is logged at error, not warn: the whole point of
        // an alert is that someone finds out, and the operator's only clue
        // that they are not finding out is this line and /api/health.
        match self.http.post(url).json(&payload).send().await {
            Ok(response) if response.status().is_success() => {
                self.delivery_failing.store(false, Ordering::Relaxed);
            }
            Ok(response) => {
                self.delivery_failing.store(true, Ordering::Relaxed);
                error!(
                    status = %response.status(),
                    "Discord webhook returned non-success status — alerts are not being delivered"
                );
            }
            Err(e) => {
                self.delivery_failing.store(true, Ordering::Relaxed);
                error!(error = %e, "Failed to send Discord alert — alerts are not being delivered");
            }
        }

        Ok(())
    }

    /// Whether the last delivery attempt failed, so the health endpoint can
    /// report that silence is not the same as calm.
    pub fn delivery_failing(&self) -> bool {
        self.delivery_failing.load(Ordering::Relaxed)
    }

    /// Report an operational anomaly, deduped for 30 minutes per
    /// `(kind, scope)`.
    ///
    /// `scope` names the thing that is wrong — a venue id, a symbol, an order
    /// id — and is part of the dedupe key. Deduping on the kind alone would
    /// mean a second venue failing during the first one's quiet window never
    /// alerts at all; deduping on the full detail string would defeat dedupe
    /// entirely, because detail usually carries a varying error message.
    ///
    /// Returns whether the anomaly passed its quiet window. A client with no
    /// webhook configured still returns true: the dedupe decision is about the
    /// anomaly, not about whether anyone happens to be listening.
    pub async fn anomaly(
        &self,
        level: AlertLevel,
        kind: AnomalyKind,
        scope: &str,
        detail: &str,
    ) -> Result<bool> {
        self.anomaly_at(Utc::now(), level, kind, scope, detail)
            .await
    }

    /// `anomaly` with the clock supplied — for callers that already hold the
    /// cycle's `now`, and for tests that would otherwise have to sleep for
    /// half an hour.
    pub async fn anomaly_at(
        &self,
        now: DateTime<Utc>,
        level: AlertLevel,
        kind: AnomalyKind,
        scope: &str,
        detail: &str,
    ) -> Result<bool> {
        // Always log, even when the alert itself is suppressed: the logs are
        // the record of how often something is failing, and dedupe is about
        // not paging an operator, not about hiding the frequency.
        match level {
            AlertLevel::Critical | AlertLevel::Warning => {
                error!(kind = kind.as_str(), scope, detail, "Anomaly")
            }
            AlertLevel::Notice => warn!(kind = kind.as_str(), scope, detail, "Anomaly"),
        }

        if !self.should_fire(now, kind, scope) {
            return Ok(false);
        }

        let msg = format!(
            "**[{}] {}**\n\
             Scope: {}\n\
             {}",
            level.label(),
            kind.as_str(),
            if scope.is_empty() { "agent" } else { scope },
            detail
        );
        self.send(&msg).await?;

        // The quiet window is recorded optimistically above, before delivery
        // is attempted. If the attempt failed, take it back: otherwise the
        // one and only try never arrived and the anomaly stays suppressed for
        // the next thirty minutes, which is precisely the window in which an
        // operator most needs to hear about it.
        if self.delivery_failing() {
            self.forget(kind, scope);
            return Ok(false);
        }
        Ok(true)
    }

    /// Drop a quiet-window entry so the next tick may try again.
    fn forget(&self, kind: AnomalyKind, scope: &str) {
        let mut recent = self
            .recent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        recent.remove(&(kind, scope.to_string()));
    }

    /// Whether this `(kind, scope)` is outside its quiet window, recording the
    /// firing if so. The lock is released before any await.
    fn should_fire(&self, now: DateTime<Utc>, kind: AnomalyKind, scope: &str) -> bool {
        // A panic while holding this lock must not disable alerting for the
        // rest of the process, so a poisoned mutex is recovered rather than
        // unwrapped.
        let mut recent = self
            .recent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let key = (kind, scope.to_string());
        match recent.get(&key) {
            Some(&last) if now - last < DEDUPE_WINDOW => false,
            _ => {
                recent.insert(key, now);
                true
            }
        }
    }

    /// Alert: New trade placed.
    pub async fn trade_placed(
        &self,
        market: &str,
        side: Side,
        size: Decimal,
        price: Decimal,
        edge: Decimal,
    ) -> Result<()> {
        let msg = format!(
            "**Trade Placed**\n\
             Market: {market}\n\
             Side: {side} @ ${price}\n\
             Size: ${size}\n\
             Edge: {:.1}%",
            edge * Decimal::from(100),
        );
        self.send(&msg).await
    }

    /// Alert: Trade resolved.
    pub async fn trade_resolved(
        &self,
        market: &str,
        side: Side,
        pnl: Decimal,
        won: bool,
    ) -> Result<()> {
        let emoji = if won { "+" } else { "" };
        let outcome = if won { "WIN" } else { "LOSS" };
        let msg = format!(
            "**Trade Resolved: {outcome}**\n\
             Market: {market}\n\
             Side: {side}\n\
             P&L: {emoji}${pnl}"
        );
        self.send(&msg).await
    }

    /// Alert: Agent state change.
    pub async fn state_change(
        &self,
        old_state: AgentState,
        new_state: AgentState,
        balance: Decimal,
    ) -> Result<()> {
        let urgency = match new_state {
            AgentState::Dead => "CRITICAL",
            AgentState::CriticalSurvival => "WARNING",
            AgentState::LowFuel => "NOTICE",
            AgentState::Alive => "INFO",
        };

        let msg = format!(
            "**[{urgency}] State Change**\n\
             {old_state} -> {new_state}\n\
             Balance: ${balance}"
        );
        self.send(&msg).await
    }

    /// Alert: Bankroll milestone reached.
    pub async fn bankroll_milestone(&self, balance: Decimal, milestone: Decimal) -> Result<()> {
        let msg = format!(
            "**Bankroll Milestone!**\n\
             Balance reached ${milestone}\n\
             Current: ${balance}"
        );
        self.send(&msg).await
    }

    /// Alert: Daily performance summary.
    pub async fn daily_summary(&self, metrics: &PerformanceMetrics) -> Result<()> {
        let msg = format!("**Daily Summary**\n```\n{}\n```", metrics.summary());
        self.send(&msg).await
    }

    /// Alert: Agent death.
    pub async fn agent_death(&self, cycle: u64, balance: Decimal) -> Result<()> {
        let msg = format!(
            "**AGENT DEATH**\n\
             Cycle: {cycle}\n\
             Final balance: ${balance}\n\
             The agent has been shut down due to insufficient funds."
        );
        self.send(&msg).await
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Bankroll milestones to watch for.
const MILESTONES: &[u64] = &[50, 100, 200, 500, 1000, 2000, 5000, 10000];

/// Check if a new bankroll level has crossed a milestone.
pub fn check_milestone(old_balance: Decimal, new_balance: Decimal) -> Option<Decimal> {
    for &m in MILESTONES {
        let milestone = Decimal::from(m);
        if old_balance < milestone && new_balance >= milestone {
            return Some(milestone);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    #[test]
    fn test_alert_client_disabled() {
        let client = AlertClient::new(None, false);
        assert!(!client.is_enabled());
    }

    #[test]
    fn test_alert_client_enabled_with_url() {
        let client = AlertClient::new(
            Some("https://discord.com/api/webhooks/123/abc".to_string()),
            true,
        );
        assert!(client.is_enabled());
    }

    #[test]
    fn test_alert_client_disabled_no_url() {
        let client = AlertClient::new(None, true);
        assert!(!client.is_enabled());
    }

    #[test]
    fn test_check_milestone_crosses() {
        // Old $90, new $105 → crosses $100
        let milestone = check_milestone(dec!(90), dec!(105));
        assert_eq!(milestone, Some(dec!(100)));
    }

    #[test]
    fn test_check_milestone_no_cross() {
        // Old $110, new $115 → no milestone
        let milestone = check_milestone(dec!(110), dec!(115));
        assert!(milestone.is_none());
    }

    #[test]
    fn test_check_milestone_exact() {
        // Old $99, new $100 → crosses $100
        let milestone = check_milestone(dec!(99), dec!(100));
        assert_eq!(milestone, Some(dec!(100)));
    }

    #[test]
    fn test_check_milestone_first() {
        // Old $40, new $55 → crosses $50
        let milestone = check_milestone(dec!(40), dec!(55));
        assert_eq!(milestone, Some(dec!(50)));
    }

    /// Anomalies are deduped per `(kind, scope)`; these run against a client
    /// with no webhook, so they exercise the decision without a network.
    fn silent_client() -> AlertClient {
        AlertClient::new(None, false)
    }

    fn t(minutes: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 19, 12, 0, 0).unwrap() + Duration::minutes(minutes)
    }

    #[tokio::test]
    async fn the_same_anomaly_stays_quiet_inside_the_window() {
        let client = silent_client();
        let fire = |at, detail: &'static str| {
            client.anomaly_at(
                at,
                AlertLevel::Warning,
                AnomalyKind::VenueUnreachable,
                "alpaca",
                detail,
            )
        };

        assert!(fire(t(0), "connection refused").await.unwrap());
        assert!(!fire(t(1), "connection refused").await.unwrap());
        // A different message is still the same problem — dedupe must not be
        // defeated by an error string that varies per attempt.
        assert!(!fire(t(29), "connection reset").await.unwrap());
    }

    #[tokio::test]
    async fn the_same_anomaly_fires_again_after_the_window() {
        let client = silent_client();
        let fire = |at| {
            client.anomaly_at(
                at,
                AlertLevel::Warning,
                AnomalyKind::VenueUnreachable,
                "alpaca",
                "down",
            )
        };

        assert!(fire(t(0)).await.unwrap());
        assert!(!fire(t(29)).await.unwrap());
        assert!(fire(t(30)).await.unwrap());
        // The window restarts from the second firing, not the first.
        assert!(!fire(t(31)).await.unwrap());
    }

    /// The reason scope is part of the key: a second venue failing during the
    /// first one's quiet window has to get through.
    #[tokio::test]
    async fn a_second_scope_is_not_masked_by_the_first() {
        let client = silent_client();
        let fire = |scope: &'static str| {
            client.anomaly_at(
                t(0),
                AlertLevel::Critical,
                AnomalyKind::VenueUnreachable,
                scope,
                "down",
            )
        };

        assert!(fire("alpaca").await.unwrap());
        assert!(fire("polymarket").await.unwrap());
        assert!(!fire("alpaca").await.unwrap());
    }

    #[tokio::test]
    async fn different_kinds_dedupe_independently() {
        let client = silent_client();
        assert!(client
            .anomaly_at(
                t(0),
                AlertLevel::Warning,
                AnomalyKind::OrderRejected,
                "BTC/USD",
                "no"
            )
            .await
            .unwrap());
        assert!(client
            .anomaly_at(
                t(0),
                AlertLevel::Warning,
                AnomalyKind::StaleOrder,
                "BTC/USD",
                "old"
            )
            .await
            .unwrap());
    }

    /// The quiet window is recorded before delivery is attempted, so a failed
    /// send would otherwise suppress the anomaly for the next thirty minutes —
    /// and the single attempt that was made never arrived.
    ///
    /// The webhook fails once and then succeeds, which is what makes this
    /// discriminating: if the window survived the failure the retry would be
    /// suppressed and never reach the now-healthy endpoint.
    #[tokio::test]
    async fn a_failed_delivery_is_retried_rather_than_suppressed() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let client = AlertClient::new(Some(server.uri()), true);

        let first = client
            .anomaly_at(
                t(0),
                AlertLevel::Critical,
                AnomalyKind::StalledCycle,
                "",
                "late",
            )
            .await
            .unwrap();
        assert!(!first, "the 503 means nobody was told");
        assert!(client.delivery_failing());

        // One minute later — deep inside the 30-minute window.
        let second = client
            .anomaly_at(
                t(1),
                AlertLevel::Critical,
                AnomalyKind::StalledCycle,
                "",
                "late",
            )
            .await
            .unwrap();
        assert!(
            second,
            "the retry must go out: the first attempt never arrived"
        );
        assert!(!client.delivery_failing());

        // Now that one has landed, the window applies normally.
        let third = client
            .anomaly_at(
                t(2),
                AlertLevel::Critical,
                AnomalyKind::StalledCycle,
                "",
                "late",
            )
            .await
            .unwrap();
        assert!(!third, "a delivered alert does start the quiet window");
    }

    #[tokio::test]
    async fn a_client_with_no_webhook_reports_delivery_as_healthy() {
        let client = silent_client();
        client
            .anomaly_at(
                t(0),
                AlertLevel::Notice,
                AnomalyKind::StalledCycle,
                "",
                "late",
            )
            .await
            .unwrap();
        // Nothing was attempted, so nothing failed. Reporting a delivery
        // failure here would have every un-alerted deployment look broken.
        assert!(!client.delivery_failing());
    }

    #[test]
    fn anomaly_kind_names_are_stable_and_distinct() {
        // These strings are a dedupe key and a health-endpoint field; a
        // duplicate would silently merge two buckets.
        let kinds = [
            AnomalyKind::StalledCycle,
            AnomalyKind::VenueUnreachable,
            AnomalyKind::DbWriteFailed,
            AnomalyKind::BudgetExhausted,
            AnomalyKind::ReconciliationMismatch,
            AnomalyKind::OrderRejected,
            AnomalyKind::OrderStateUnknown,
            AnomalyKind::StaleOrder,
        ];
        let mut names: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[tokio::test]
    async fn test_send_disabled_noop() {
        let client = AlertClient::new(None, false);
        // Should not error even though no URL
        client
            .trade_placed("Test market?", Side::Yes, dec!(5), dec!(0.60), dec!(0.10))
            .await
            .unwrap();
    }
}
