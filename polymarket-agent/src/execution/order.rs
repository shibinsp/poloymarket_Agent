//! Order building and submission.
//!
//! Constructs orders from opportunities, applies risk checks,
//! and submits via the Polymarket client.

use anyhow::{bail, Result};
use rust_decimal::Decimal;
use tracing::{info, instrument, warn};

use crate::config::ExecutionConfig;
use crate::market::models::{Opportunity, Side};
use crate::market::polymarket::PolymarketClient;

/// An order ready for submission.
#[derive(Debug, Clone)]
pub struct PreparedOrder {
    pub token_id: String,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
    pub market_id: String,
    pub market_question: String,
    pub edge: Decimal,
    pub fair_value: Decimal,
    pub confidence: Decimal,
    pub kelly_raw: Decimal,
    pub kelly_adjusted: Decimal,
}

/// Result of an order execution attempt.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub order_id: String,
    pub token_id: String,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
    pub status: OrderStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderStatus {
    Filled,
    Rejected(String),
}

/// Build a prepared order from an opportunity.
///
/// Selects the correct token and price based on the recommended side.
/// Applies slippage limit to the order price.
pub fn prepare_order(
    opportunity: &Opportunity,
    kelly_raw: Decimal,
    kelly_adjusted: Decimal,
    config: &ExecutionConfig,
) -> Result<PreparedOrder> {
    let side = opportunity.recommended_side;
    let midpoint = opportunity.order_book.midpoint;

    // Find the token for the recommended side by matching outcome name (TRD-04).
    // Do NOT rely on array index — Polymarket API doesn't guarantee order.
    // Also compute `reference_price`: the fair mid-price for this side, used
    // below as the slippage baseline (not `best_price` itself).
    let (token_id, best_price, reference_price) = match side {
        Side::Yes => {
            // Buying YES: find token with outcome "Yes"
            let token = opportunity
                .market
                .tokens
                .iter()
                .find(|t| t.outcome.eq_ignore_ascii_case("yes"))
                .or_else(|| opportunity.market.tokens.first())
                .ok_or_else(|| anyhow::anyhow!("No YES token found"))?;
            let ask_price = opportunity
                .order_book
                .asks
                .first()
                .map(|a| a.price)
                .unwrap_or(midpoint);
            (token.token_id.clone(), ask_price, midpoint)
        }
        Side::No => {
            // Buying NO: find token with outcome "No"
            let token = opportunity
                .market
                .tokens
                .iter()
                .find(|t| t.outcome.eq_ignore_ascii_case("no"))
                .or_else(|| opportunity.market.tokens.last())
                .ok_or_else(|| anyhow::anyhow!("No NO token found"))?;
            // For NO side, we bid on the NO token at (1 - yes_bid_price)
            let bid_price = opportunity
                .order_book
                .bids
                .first()
                .map(|b| b.price)
                .unwrap_or(midpoint);
            let no_price = Decimal::ONE - bid_price;
            (token.token_id.clone(), no_price, Decimal::ONE - midpoint)
        }
    };

    // Apply slippage limit: never pay more than max_slippage_pct above the
    // order book's reference (mid) price. Previously this compared best_price
    // against a bound derived from best_price itself, which is always >=
    // best_price by construction — the cap could never actually bind.
    let max_price = reference_price * (Decimal::ONE + config.max_slippage_pct);
    let order_price = best_price.min(max_price);

    // Size in number of shares (position_usd / price)
    let size = if order_price > Decimal::ZERO {
        opportunity.kelly_size / order_price
    } else {
        return Err(anyhow::anyhow!("Order price is zero"));
    };

    if size <= Decimal::ZERO {
        bail!("Calculated order size is zero or negative");
    }

    Ok(PreparedOrder {
        token_id,
        side,
        price: order_price,
        size,
        market_id: opportunity.market.condition_id.clone(),
        market_question: opportunity.market.question.clone(),
        edge: opportunity.edge,
        fair_value: opportunity.fair_value,
        confidence: opportunity.confidence,
        kelly_raw,
        kelly_adjusted,
    })
}

/// Execute a prepared order via the Polymarket client.
#[instrument(skip(client, order), fields(
    market = %order.market_id,
    side = %order.side,
    price = %order.price,
    size = %order.size,
))]
pub async fn execute_order(client: &PolymarketClient, order: &PreparedOrder) -> ExecutionResult {
    match client
        .place_limit_order(&order.token_id, order.side, order.price, order.size)
        .await
    {
        Ok(order_id) => {
            info!(
                order_id = %order_id,
                edge = %order.edge,
                "Order executed successfully"
            );
            ExecutionResult {
                order_id,
                token_id: order.token_id.clone(),
                side: order.side,
                price: order.price,
                size: order.size,
                status: OrderStatus::Filled,
            }
        }
        Err(e) => {
            warn!(
                error = %e,
                "Order execution failed"
            );
            ExecutionResult {
                order_id: String::new(),
                token_id: order.token_id.clone(),
                side: order.side,
                price: order.price,
                size: order.size,
                status: OrderStatus::Rejected(e.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::models::{Market, MarketCategory, OrderBookSnapshot, PriceLevel, TokenInfo};
    use chrono::Utc;
    use rust_decimal_macros::dec;

    fn test_config() -> ExecutionConfig {
        ExecutionConfig {
            order_type: "limit".to_string(),
            order_ttl_seconds: 60,
            max_slippage_pct: dec!(0.02),
            max_retries: 3,
        }
    }

    fn test_opportunity_with_book(
        side: Side,
        kelly_size: Decimal,
        bid: Decimal,
        ask: Decimal,
    ) -> Opportunity {
        let midpoint = (bid + ask) / dec!(2);
        Opportunity {
            market: Market {
                condition_id: "m1".to_string(),
                question: "Will BTC hit 100k?".to_string(),
                outcomes: vec!["Yes".to_string(), "No".to_string()],
                tokens: vec![
                    TokenInfo {
                        token_id: "tok_yes".to_string(),
                        outcome: "Yes".to_string(),
                        price: dec!(0.60),
                    },
                    TokenInfo {
                        token_id: "tok_no".to_string(),
                        outcome: "No".to_string(),
                        price: dec!(0.40),
                    },
                ],
                end_date: Utc::now() + chrono::Duration::days(7),
                category: MarketCategory::Crypto,
                volume_24h: dec!(50000),
                active: true,
            },
            order_book: OrderBookSnapshot {
                token_id: "tok_yes".to_string(),
                bids: vec![PriceLevel {
                    price: bid,
                    size: dec!(500),
                }],
                asks: vec![PriceLevel {
                    price: ask,
                    size: dec!(500),
                }],
                spread: ask - bid,
                midpoint,
                implied_probability: midpoint,
                timestamp: Utc::now(),
            },
            fair_value: dec!(0.75),
            confidence: dec!(0.85),
            edge: dec!(0.15),
            recommended_side: side,
            kelly_size,
        }
    }

    /// Tight spread (±0.83% around midpoint 0.60) — well within the default
    /// 2% slippage tolerance, so the cap should never bind here.
    fn test_opportunity(side: Side, kelly_size: Decimal) -> Opportunity {
        test_opportunity_with_book(side, kelly_size, dec!(0.595), dec!(0.605))
    }

    #[test]
    fn test_prepare_order_yes_side() {
        let config = test_config();
        let opp = test_opportunity(Side::Yes, dec!(6));

        let order = prepare_order(&opp, dec!(0.27), dec!(0.12), &config).unwrap();

        assert_eq!(order.side, Side::Yes);
        assert_eq!(order.token_id, "tok_yes");
        // Ask (0.605) is within 2% of midpoint (0.60), so price = best ask
        assert_eq!(order.price, dec!(0.605));
        // Size = 6 / 0.605 = ~9.917
        assert!(order.size > dec!(9));
        assert!(order.size < dec!(10));
        assert_eq!(order.edge, dec!(0.15));
    }

    #[test]
    fn test_prepare_order_no_side() {
        let config = test_config();
        let opp = test_opportunity(Side::No, dec!(5));

        let order = prepare_order(&opp, dec!(0.20), dec!(0.10), &config).unwrap();

        assert_eq!(order.side, Side::No);
        assert_eq!(order.token_id, "tok_no");
        // NO price = 1 - best_bid(0.595) = 0.405, within 2% of NO midpoint (0.40)
        assert_eq!(order.price, dec!(0.405));
        // Size = 5 / 0.405 = ~12.3
        assert!(order.size > dec!(11));
    }

    #[test]
    fn test_prepare_order_yes_side_slippage_capped() {
        // Wide spread: ask (0.62) is 3.33% above midpoint (0.60), beyond the
        // 2% slippage tolerance, so the order price must be capped rather
        // than chasing the ask.
        let config = test_config();
        let opp = test_opportunity_with_book(Side::Yes, dec!(6), dec!(0.58), dec!(0.62));

        let order = prepare_order(&opp, dec!(0.27), dec!(0.12), &config).unwrap();

        // Capped at midpoint * 1.02 = 0.612, not the raw ask of 0.62
        assert_eq!(order.price, dec!(0.612));
    }

    #[test]
    fn test_prepare_order_no_side_slippage_capped() {
        // Wide spread: NO price (1 - 0.58 = 0.42) is 5% above the NO
        // midpoint (0.40), beyond the 2% slippage tolerance.
        let config = test_config();
        let opp = test_opportunity_with_book(Side::No, dec!(5), dec!(0.58), dec!(0.62));

        let order = prepare_order(&opp, dec!(0.20), dec!(0.10), &config).unwrap();

        // Capped at (1 - midpoint) * 1.02 = 0.40 * 1.02 = 0.408, not 0.42
        assert_eq!(order.price, dec!(0.408));
    }

    #[test]
    fn test_prepare_order_zero_kelly() {
        let config = test_config();
        let opp = test_opportunity(Side::Yes, Decimal::ZERO);

        let result = prepare_order(&opp, Decimal::ZERO, Decimal::ZERO, &config);
        assert!(result.is_err());
    }
}
