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

    // Find the token for the recommended side by matching outcome name (TRD-04).
    // Do NOT rely on array index — Polymarket API doesn't guarantee order.
    let (token_id, best_price) = match side {
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
                .unwrap_or(opportunity.order_book.midpoint);
            (token.token_id.clone(), ask_price)
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
                .unwrap_or(opportunity.order_book.midpoint);
            let no_price = Decimal::ONE - bid_price;
            (token.token_id.clone(), no_price)
        }
    };

    // Apply slippage limit: don't pay more than best_price * (1 + slippage)
    let max_price = best_price * (Decimal::ONE + config.max_slippage_pct);
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
pub async fn execute_order(
    client: &PolymarketClient,
    order: &PreparedOrder,
) -> ExecutionResult {
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
    use rust_decimal_macros::dec;
    use crate::market::models::{
        Market, MarketCategory, OrderBookSnapshot, PriceLevel, TokenInfo,
    };
    use chrono::Utc;

    fn test_config() -> ExecutionConfig {
        ExecutionConfig {
            order_type: "limit".to_string(),
            order_ttl_seconds: 60,
            max_slippage_pct: dec!(0.02),
            max_retries: 3,
        }
    }

    fn test_opportunity(side: Side, kelly_size: Decimal) -> Opportunity {
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
                    price: dec!(0.58),
                    size: dec!(500),
                }],
                asks: vec![PriceLevel {
                    price: dec!(0.62),
                    size: dec!(500),
                }],
                spread: dec!(0.04),
                midpoint: dec!(0.60),
                implied_probability: dec!(0.60),
                timestamp: Utc::now(),
            },
            fair_value: dec!(0.75),
            confidence: dec!(0.85),
            edge: dec!(0.15),
            recommended_side: side,
            kelly_size,
        }
    }

    #[test]
    fn test_prepare_order_yes_side() {
        let config = test_config();
        let opp = test_opportunity(Side::Yes, dec!(6));

        let order = prepare_order(&opp, dec!(0.27), dec!(0.12), &config).unwrap();

        assert_eq!(order.side, Side::Yes);
        assert_eq!(order.token_id, "tok_yes");
        // Price should be the best ask: 0.62
        assert_eq!(order.price, dec!(0.62));
        // Size = 6 / 0.62 = ~9.677
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
        // NO price = 1 - best_bid(0.58) = 0.42
        assert_eq!(order.price, dec!(0.42));
        // Size = 5 / 0.42 = ~11.9
        assert!(order.size > dec!(11));
    }

    #[test]
    fn test_prepare_order_zero_kelly() {
        let config = test_config();
        let opp = test_opportunity(Side::Yes, Decimal::ZERO);

        let result = prepare_order(&opp, Decimal::ZERO, Decimal::ZERO, &config);
        assert!(result.is_err());
    }
}
