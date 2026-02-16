//! Trade resolution pipeline.
//!
//! Checks whether markets with open trades have resolved,
//! settles positions (computes P&L, updates trade status),
//! and feeds resolved outcomes into the calibration system.

use anyhow::{Context, Result};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use std::str::FromStr;
use tracing::{info, warn};

use crate::db::store::{Store, TradeRecord};
use crate::market::models::Side;
use crate::valuation::calibration;

/// Lightweight response from Gamma API for resolution checking.
/// Only fetches the fields we need to determine if a market has resolved.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaResolutionResponse {
    condition_id: Option<String>,
    /// Whether the market is closed (no longer trading).
    closed: Option<bool>,
    /// Whether the market has a final resolution.
    resolved: Option<bool>,
    /// JSON-encoded string: "[\"0.025\", \"0.975\"]" — final prices after resolution.
    /// For a resolved YES market: ["1", "0"]. For resolved NO: ["0", "1"].
    outcome_prices: Option<String>,
}

/// The result of resolving a single trade.
#[derive(Debug)]
pub struct ResolutionResult {
    pub trade_id: i64,
    pub market_id: String,
    pub pnl: Decimal,
    pub won: bool,
}

/// Check all open trades for market resolution and settle any that have resolved.
///
/// Flow:
/// 1. Fetch all OPEN trades from the database
/// 2. Deduplicate by market_id (one API call per market, not per trade)
/// 3. Query Gamma API for each market's resolution status
/// 4. For resolved markets: compute P&L, update trade status, feed calibration
pub async fn check_and_settle(
    store: &Store,
    http: &reqwest::Client,
    gamma_base_url: &str,
) -> Result<Vec<ResolutionResult>> {
    let open_trades = store.get_open_trades().await?;
    if open_trades.is_empty() {
        return Ok(Vec::new());
    }

    // Deduplicate market IDs
    let mut market_ids: Vec<String> = open_trades
        .iter()
        .map(|t| t.market_id.clone())
        .collect();
    market_ids.sort();
    market_ids.dedup();

    info!(
        open_trades = open_trades.len(),
        unique_markets = market_ids.len(),
        "Checking market resolutions"
    );

    let mut results = Vec::new();

    for market_id in &market_ids {
        // Query Gamma API for this specific market
        let resolution = match fetch_market_resolution(http, gamma_base_url, market_id).await {
            Ok(Some(r)) => r,
            Ok(None) => continue, // Market not found or not resolved
            Err(e) => {
                warn!(market_id = %market_id, error = %e, "Failed to check market resolution");
                continue;
            }
        };

        // Settle each trade on this market
        let market_trades: Vec<&TradeRecord> = open_trades
            .iter()
            .filter(|t| &t.market_id == market_id)
            .collect();

        for trade in market_trades {
            let trade_id = match trade.id {
                Some(id) => id,
                None => continue,
            };

            match settle_trade(store, trade, &resolution).await {
                Ok(result) => {
                    // Feed calibration system
                    let actual_outcome = if resolution.yes_won {
                        Decimal::ONE
                    } else {
                        Decimal::ZERO
                    };
                    if let Err(e) = calibration::record_resolution(
                        store.pool(),
                        market_id,
                        actual_outcome,
                    )
                    .await
                    {
                        warn!(error = %e, "Failed to record calibration resolution");
                    }

                    results.push(result);
                }
                Err(e) => {
                    warn!(
                        trade_id,
                        market_id = %market_id,
                        error = %e,
                        "Failed to settle trade"
                    );
                }
            }
        }
    }

    if !results.is_empty() {
        let total_pnl: Decimal = results.iter().map(|r| r.pnl).sum();
        let wins = results.iter().filter(|r| r.won).count();
        let losses = results.len() - wins;
        info!(
            settled = results.len(),
            wins,
            losses,
            total_pnl = %total_pnl,
            "Trades settled"
        );
    }

    Ok(results)
}

/// Parsed resolution state for a market.
struct MarketResolution {
    /// Whether YES won (YES outcome price = 1.0).
    yes_won: bool,
}

/// Fetch market resolution status from Gamma API.
/// Returns `Ok(None)` if the market hasn't resolved yet.
async fn fetch_market_resolution(
    http: &reqwest::Client,
    gamma_base_url: &str,
    condition_id: &str,
) -> Result<Option<MarketResolution>> {
    let url = format!("{}/markets", gamma_base_url);

    let response = http
        .get(&url)
        .query(&[("condition_id", condition_id)])
        .send()
        .await
        .context("HTTP request to Gamma API failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Gamma API returned {status}: {body}");
    }

    let markets: Vec<GammaResolutionResponse> = response
        .json()
        .await
        .context("Failed to deserialize Gamma resolution response")?;

    let market = match markets.first() {
        Some(m) => m,
        None => return Ok(None),
    };

    // Market must be both closed and resolved
    let closed = market.closed.unwrap_or(false);
    let resolved = market.resolved.unwrap_or(false);

    if !closed || !resolved {
        return Ok(None);
    }

    // Parse outcome prices to determine winner.
    // Resolved market outcome_prices are typically ["1", "0"] or ["0", "1"].
    let prices_str = market
        .outcome_prices
        .as_deref()
        .unwrap_or("[]");
    let prices: Vec<String> = serde_json::from_str(prices_str).unwrap_or_default();

    // First outcome is YES, second is NO
    let yes_price = prices
        .first()
        .and_then(|s| Decimal::from_str(s).ok())
        .unwrap_or(Decimal::ZERO);

    let yes_won = yes_price > dec!(0.5);

    Ok(Some(MarketResolution { yes_won }))
}

/// Settle a single trade based on market resolution.
///
/// P&L calculation:
/// - YES trade that wins: (1.0 - entry_price) × size
/// - YES trade that loses: (0.0 - entry_price) × size (negative)
/// - NO trade that wins: entry_price × size (we bought NO at entry_price, payout = 1 - entry)
/// - NO trade that loses: -(1.0 - entry_price) × size
async fn settle_trade(
    store: &Store,
    trade: &TradeRecord,
    resolution: &MarketResolution,
) -> Result<ResolutionResult> {
    let trade_id = trade.id.unwrap();
    let entry_price = Decimal::from_str(&trade.entry_price)
        .context("Invalid entry_price in trade record")?;
    let size = Decimal::from_str(&trade.size)
        .context("Invalid size in trade record")?;

    let side = match trade.direction.as_str() {
        "YES" => Side::Yes,
        "NO" => Side::No,
        other => anyhow::bail!("Unknown trade direction: {other}"),
    };

    // Did this trade win?
    let won = match side {
        Side::Yes => resolution.yes_won,
        Side::No => !resolution.yes_won,
    };

    // P&L calculation
    let pnl = if won {
        // Winner receives $1 per share
        match side {
            Side::Yes => (Decimal::ONE - entry_price) * size,
            Side::No => (Decimal::ONE - entry_price) * size, // Bought NO at entry, pays out (1 - entry)... wait
            // NO tokens: entry_price is what we paid for the NO token.
            // If NO wins, payout = $1 per NO share. Profit = (1 - entry_price) * size.
        }
    } else {
        // Loser gets nothing — loss is what we paid
        -entry_price * size
    };

    let status = if won { "RESOLVED_WIN" } else { "RESOLVED_LOSS" };
    let now = Utc::now();

    store
        .update_trade_status(trade_id, status, Some(pnl), Some(now))
        .await
        .context("Failed to update trade status")?;

    info!(
        trade_id,
        market_id = %trade.market_id,
        side = %trade.direction,
        entry_price = %entry_price,
        pnl = %pnl,
        won,
        "Trade settled"
    );

    Ok(ResolutionResult {
        trade_id,
        market_id: trade.market_id.clone(),
        pnl,
        won,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::store::TradeRecord;

    fn open_yes_trade(id: i64, entry: &str, size: &str) -> TradeRecord {
        TradeRecord {
            id: Some(id),
            cycle: 1,
            market_id: "mkt_1".to_string(),
            market_question: Some("Will X happen?".to_string()),
            direction: "YES".to_string(),
            entry_price: entry.to_string(),
            size: size.to_string(),
            edge_at_entry: "0.10".to_string(),
            claude_fair_value: "0.70".to_string(),
            confidence: "0.85".to_string(),
            kelly_raw: "0.20".to_string(),
            kelly_adjusted: "0.10".to_string(),
            status: "OPEN".to_string(),
            pnl: None,
            created_at: None,
            resolved_at: None,
        }
    }

    #[tokio::test]
    async fn test_settle_yes_trade_wins() {
        let store = Store::new(":memory:").await.unwrap();
        let trade = open_yes_trade(0, "0.60", "10");
        let trade_id = store.insert_trade(&trade).await.unwrap();
        let mut stored = store.get_open_trades().await.unwrap();
        let t = &stored[0];

        let resolution = MarketResolution { yes_won: true };
        let result = settle_trade(&store, t, &resolution).await.unwrap();

        assert!(result.won);
        // PnL = (1.0 - 0.60) * 10 = 4.0
        assert_eq!(result.pnl, dec!(4.0));

        let resolved = store.get_resolved_trades().await.unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].status, "RESOLVED_WIN");
    }

    #[tokio::test]
    async fn test_settle_yes_trade_loses() {
        let store = Store::new(":memory:").await.unwrap();
        let trade = open_yes_trade(0, "0.60", "10");
        store.insert_trade(&trade).await.unwrap();
        let stored = store.get_open_trades().await.unwrap();
        let t = &stored[0];

        let resolution = MarketResolution { yes_won: false };
        let result = settle_trade(&store, t, &resolution).await.unwrap();

        assert!(!result.won);
        // PnL = -0.60 * 10 = -6.0
        assert_eq!(result.pnl, dec!(-6.0));
    }

    #[tokio::test]
    async fn test_settle_no_trade_wins() {
        let store = Store::new(":memory:").await.unwrap();
        let mut trade = open_yes_trade(0, "0.40", "10");
        trade.direction = "NO".to_string();
        store.insert_trade(&trade).await.unwrap();
        let stored = store.get_open_trades().await.unwrap();
        let t = &stored[0];

        let resolution = MarketResolution { yes_won: false }; // NO wins
        let result = settle_trade(&store, t, &resolution).await.unwrap();

        assert!(result.won);
        // PnL = (1.0 - 0.40) * 10 = 6.0
        assert_eq!(result.pnl, dec!(6.0));
    }
}
