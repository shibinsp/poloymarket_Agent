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
#[allow(dead_code)]
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
    // Only prediction markets settle. A continuous position is closed by
    // trading out of it, and its `market_id` is a symbol like "BTC/USD" —
    // sending those to the Polymarket resolution API is one wasted request per
    // open position per cycle, each logging a warning, and if Gamma ever
    // answered for one of those strings the settlement path would book an
    // equity position as a prediction-market win or loss.
    let open_trades: Vec<_> = store
        .get_open_trades()
        .await?
        .into_iter()
        .filter(|t| t.direction == "YES" || t.direction == "NO")
        .collect();
    if open_trades.is_empty() {
        return Ok(Vec::new());
    }

    // Deduplicate market IDs
    let mut market_ids: Vec<String> = open_trades.iter().map(|t| t.market_id.clone()).collect();
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
                    if let Err(e) =
                        calibration::record_resolution(store.pool(), market_id, actual_outcome)
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
pub(crate) struct MarketResolution {
    /// Whether YES won (YES outcome price = 1.0).
    pub(crate) yes_won: bool,
}

/// Fetch market resolution status from Gamma API.
/// Returns `Ok(None)` if the market hasn't resolved yet.
pub(crate) async fn fetch_market_resolution(
    http: &reqwest::Client,
    gamma_base_url: &str,
    condition_id: &str,
) -> Result<Option<MarketResolution>> {
    let markets: Vec<GammaResolutionResponse> =
        crate::market::polymarket::gamma_markets_by_condition_id_as(
            http,
            gamma_base_url,
            condition_id,
        )
        .await
        .context("Failed to fetch Gamma resolution")?;

    let market = match markets.first() {
        Some(m) => m,
        None => return Ok(None),
    };

    // Gamma no longer sends `resolved`, so a closed market counts as settled
    // once its final prices are a clean 1 / 0 (see `settled_yes_won`). A
    // closed market still awaiting its result shows ["0", "0"] or mid prices.
    if !market.closed.unwrap_or(false) {
        return Ok(None);
    }
    if market.resolved == Some(false) {
        return Ok(None);
    }

    // Parse outcome prices to determine winner.
    // Resolved market outcome_prices are typically ["1", "0"] or ["0", "1"].
    let prices_str = match &market.outcome_prices {
        Some(s) if !s.is_empty() => s.as_str(),
        _ => {
            warn!(condition_id = %condition_id, "Resolved market missing outcome_prices — skipping");
            return Ok(None);
        }
    };

    let prices: Vec<String> = match serde_json::from_str(prices_str) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                condition_id = %condition_id,
                prices_str = %prices_str,
                error = %e,
                "Failed to parse outcome_prices JSON — skipping resolution"
            );
            return Ok(None);
        }
    };

    Ok(settled_yes_won(&prices).map(|yes_won| MarketResolution { yes_won }))
}

/// Whether the first outcome (YES) won, from a closed market's final prices.
/// `None` unless the prices are a clean settlement: one side at least 0.99
/// and the other at most 0.01. First outcome is YES, second is NO.
fn settled_yes_won(prices: &[String]) -> Option<bool> {
    let p: Vec<Decimal> = prices.iter().filter_map(|s| Decimal::from_str(s).ok()).collect();
    if p.len() != 2 {
        return None;
    }
    let (yes, no) = (p[0], p[1]);
    if yes >= dec!(0.99) && no <= dec!(0.01) {
        Some(true)
    } else if no >= dec!(0.99) && yes <= dec!(0.01) {
        Some(false)
    } else {
        None
    }
}

/// Settle a single trade based on market resolution.
///
/// P&L calculation. `entry_price` is what we paid per share of the token we
/// bought — the YES price for YES trades, the NO price for NO trades — so the
/// same formula applies to both sides:
/// - winner: (1.0 - entry_price) × size — each winning share pays out $1
/// - loser: -entry_price × size — losing shares expire worthless
async fn settle_trade(
    store: &Store,
    trade: &TradeRecord,
    resolution: &MarketResolution,
) -> Result<ResolutionResult> {
    let trade_id = trade.id.unwrap();
    let entry_price =
        Decimal::from_str(&trade.entry_price).context("Invalid entry_price in trade record")?;
    let size = Decimal::from_str(&trade.size).context("Invalid size in trade record")?;

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

    // A winning share pays out $1; a losing share expires worthless.
    let pnl = if won {
        (Decimal::ONE - entry_price) * size
    } else {
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

    use crate::db::store::{Store, VenueTradeRecord};

    /// Only prediction markets settle. A continuous position's `market_id` is
    /// a symbol like "BTC/USD", and sending those to the Polymarket
    /// resolution API is a wasted request per position per cycle — and if
    /// Gamma ever answered for one, the settlement path would book an equity
    /// position as a prediction-market win or loss.
    ///
    #[tokio::test]
    async fn continuous_positions_are_never_sent_to_the_resolution_api() {
        let store = Store::new(":memory:").await.expect("store");
        store
            .insert_venue_trade(&VenueTradeRecord {
                cycle: 1,
                venue_id: "alpaca".to_string(),
                symbol: "BTC/USD".to_string(),
                asset_class: "crypto_spot".to_string(),
                display_name: None,
                side: "BUY".to_string(),
                entry_price: "100".to_string(),
                quantity: "1".to_string(),
                notional: "100".to_string(),
                avg_fill_price: Some("100".to_string()),
                edge_at_entry: "0.05".to_string(),
                fair_value: "0.6".to_string(),
                confidence: "0.8".to_string(),
                risk_pct: "0.0075".to_string(),
                stop_pct: "0.03".to_string(),
                status: "OPEN".to_string(),
                stop_price: None,
                target_price: None,
                horizon_hours: None,
                client_order_id: Some("c-1".to_string()),
                venue_order_id: None,
                strategy: None,
            })
            .await
            .expect("insert");

        // A live mock, so the assertion is on requests *made* rather than on
        // the call happening to fail. check_and_settle logs and continues past
        // a failed lookup, so an unreachable host would pass either way.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let out = check_and_settle(&store, &http, &server.uri())
            .await
            .expect("settles nothing");

        assert!(out.is_empty());
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            0,
            "a crypto position must not be looked up in a prediction-market API"
        );
    }
    use crate::db::store::TradeRecord;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn only_clean_final_prices_count_as_settled() {
        assert_eq!(settled_yes_won(&s(&["1", "0"])), Some(true));
        assert_eq!(settled_yes_won(&s(&["0", "1"])), Some(false));
        assert_eq!(settled_yes_won(&s(&["0.995", "0.005"])), Some(true));
        // Closed but still waiting for its result.
        assert_eq!(settled_yes_won(&s(&["0", "0"])), None);
        assert_eq!(settled_yes_won(&s(&["0.9825", "0.0175"])), None);
        assert_eq!(settled_yes_won(&s(&["0.5", "0.5"])), None);
        assert_eq!(settled_yes_won(&s(&["1"])), None);
    }

    /// Gamma no longer sends `resolved`; a closed market with clean final
    /// prices settles, an open one does not, and only our market's row counts.
    #[tokio::test]
    async fn a_closed_market_settles_without_a_resolved_field() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::query_param("condition_ids", "0xohio"))
            .and(wiremock::matchers::query_param("closed", "true"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"conditionId": "0xohio", "closed": true, "outcomePrices": "[\"1\", \"0\"]"}
            ])))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"conditionId": "0xother", "closed": true, "outcomePrices": "[\"0\", \"1\"]"}
            ])))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let r = fetch_market_resolution(&http, &server.uri(), "0xohio")
            .await
            .unwrap()
            .expect("settled");
        assert!(r.yes_won);
        assert!(fetch_market_resolution(&http, &server.uri(), "0xopen")
            .await
            .unwrap()
            .is_none());
    }

    fn open_yes_trade(id: i64, entry: &str, size: &str) -> TradeRecord {
        TradeRecord {
            id: Some(id),
            cycle: 1,
            market_id: "mkt_1".to_string(),
            market_question: Some("Will X happen?".to_string()),
            direction: "YES".to_string(),
            entry_price: entry.to_string(),
            size: size.to_string(),
            quantity: None,
            edge_at_entry: "0.10".to_string(),
            claude_fair_value: "0.70".to_string(),
            confidence: "0.85".to_string(),
            kelly_raw: "0.20".to_string(),
            kelly_adjusted: "0.10".to_string(),
            status: "OPEN".to_string(),
            pnl: None,
            created_at: None,
            resolved_at: None,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_settle_yes_trade_wins() {
        let store = Store::new(":memory:").await.unwrap();
        let trade = open_yes_trade(0, "0.60", "10");
        store.insert_trade(&trade).await.unwrap();
        let stored = store.get_open_trades().await.unwrap();
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
