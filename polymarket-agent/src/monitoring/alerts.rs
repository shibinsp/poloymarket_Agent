//! Discord/Telegram alert system.
//!
//! Sends notifications via Discord webhooks for trade events,
//! state changes, and daily summaries.

use anyhow::Result;
use rust_decimal::Decimal;
use serde::Serialize;
use tracing::{info, warn};

use crate::market::models::{AgentState, Side};
use crate::monitoring::metrics::PerformanceMetrics;

/// Discord webhook client.
pub struct AlertClient {
    webhook_url: Option<String>,
    http: reqwest::Client,
    enabled: bool,
}

/// Discord webhook message format.
#[derive(Debug, Serialize)]
struct DiscordMessage {
    content: String,
    username: String,
}

impl AlertClient {
    pub fn new(webhook_url: Option<String>, enabled: bool) -> Self {
        Self {
            enabled: enabled && webhook_url.is_some(),
            webhook_url,
            http: reqwest::Client::new(),
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

        match self.http.post(url).json(&payload).send().await {
            Ok(response) => {
                if !response.status().is_success() {
                    warn!(
                        status = %response.status(),
                        "Discord webhook returned non-success status"
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to send Discord alert");
            }
        }

        Ok(())
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
        let msg = format!(
            "**Daily Summary**\n```\n{}\n```",
            metrics.summary()
        );
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
