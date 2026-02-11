//! Edge calculation (fair value vs market price).
//!
//! Determines whether a market is mispriced enough to trade,
//! with thresholds adjusted by confidence level.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::config::ValuationConfig;
use crate::market::models::{MarketCandidate, Opportunity, Side};
use crate::valuation::fair_value::{DataQuality, ValuationResult};

/// Calculate edge and determine if a trade opportunity exists.
pub fn evaluate_edge(
    candidate: &MarketCandidate,
    valuation: &ValuationResult,
    config: &ValuationConfig,
) -> Option<EdgeResult> {
    let market_prob = candidate.order_book.implied_probability;
    let fair_prob = valuation.probability;

    // Raw edge = |fair_value - market_implied_prob|
    let raw_edge = (fair_prob - market_prob).abs();

    // Determine threshold based on confidence
    let threshold = edge_threshold(valuation, config);

    // Skip low-confidence valuations entirely
    if valuation.confidence < dec!(0.4) || valuation.data_quality == DataQuality::Low {
        return None;
    }

    if raw_edge < threshold {
        return None;
    }

    // Determine which side to trade
    let side = if fair_prob > market_prob {
        Side::Yes // Market underprices YES → buy YES
    } else {
        Side::No // Market overprices YES → buy NO
    };

    // Effective price for the side we want to trade
    let trade_price = match side {
        Side::Yes => candidate.order_book.midpoint,
        Side::No => Decimal::ONE - candidate.order_book.midpoint,
    };

    Some(EdgeResult {
        raw_edge,
        threshold,
        side,
        fair_probability: fair_prob,
        market_probability: market_prob,
        trade_price,
    })
}

/// Determine the edge threshold based on confidence level.
fn edge_threshold(valuation: &ValuationResult, config: &ValuationConfig) -> Decimal {
    if valuation.confidence >= dec!(0.8) {
        config.high_confidence_edge // 6% for high confidence
    } else if valuation.confidence >= dec!(0.5) {
        config.min_edge_threshold // 8% for medium confidence
    } else {
        config.low_confidence_edge // 10% for low confidence
    }
}

/// Result of edge evaluation.
#[derive(Debug, Clone)]
pub struct EdgeResult {
    /// Absolute difference between fair value and market price.
    pub raw_edge: Decimal,
    /// Threshold that was applied.
    pub threshold: Decimal,
    /// Which side to trade (Yes or No).
    pub side: Side,
    /// Claude's fair probability estimate.
    pub fair_probability: Decimal,
    /// Market's implied probability.
    pub market_probability: Decimal,
    /// The price we'd trade at.
    pub trade_price: Decimal,
}

/// Convert edge result + candidate + valuation into a full Opportunity.
pub fn to_opportunity(
    candidate: &MarketCandidate,
    valuation: &ValuationResult,
    edge: &EdgeResult,
    kelly_size: Decimal,
) -> Opportunity {
    Opportunity {
        market: candidate.market.clone(),
        order_book: candidate.order_book.clone(),
        fair_value: valuation.probability,
        confidence: valuation.confidence,
        edge: edge.raw_edge,
        recommended_side: edge.side,
        kelly_size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::models::{
        Market, MarketCategory, OrderBookSnapshot, PriceLevel, TokenInfo,
    };
    use chrono::Utc;

    fn test_config() -> ValuationConfig {
        ValuationConfig {
            claude_model: "claude-sonnet-4-20250514".to_string(),
            min_edge_threshold: dec!(0.08),
            high_confidence_edge: dec!(0.06),
            low_confidence_edge: dec!(0.10),
            cache_ttl_seconds: 300,
        }
    }

    fn test_candidate(midpoint: Decimal) -> MarketCandidate {
        MarketCandidate {
            market: Market {
                condition_id: "test".to_string(),
                question: "Will it rain?".to_string(),
                outcomes: vec!["Yes".to_string(), "No".to_string()],
                tokens: vec![TokenInfo {
                    token_id: "123".to_string(),
                    outcome: "Yes".to_string(),
                    price: midpoint,
                }],
                end_date: Utc::now() + chrono::Duration::days(7),
                category: MarketCategory::Weather,
                volume_24h: dec!(10000),
                active: true,
            },
            order_book: OrderBookSnapshot {
                token_id: "123".to_string(),
                bids: vec![PriceLevel {
                    price: midpoint - dec!(0.02),
                    size: dec!(100),
                }],
                asks: vec![PriceLevel {
                    price: midpoint + dec!(0.02),
                    size: dec!(100),
                }],
                spread: dec!(0.04),
                midpoint,
                implied_probability: midpoint,
                timestamp: Utc::now(),
            },
        }
    }

    fn test_valuation(probability: Decimal, confidence: Decimal) -> ValuationResult {
        ValuationResult {
            probability,
            confidence,
            reasoning_summary: "Test".to_string(),
            key_factors: vec!["test".to_string()],
            data_quality: DataQuality::High,
            time_sensitivity: crate::valuation::fair_value::TimeSensitivity::Days,
        }
    }

    #[test]
    fn test_edge_buy_yes() {
        let config = test_config();
        let candidate = test_candidate(dec!(0.50));
        // Claude says 65% probability, market says 50% → 15% edge → buy YES
        let valuation = test_valuation(dec!(0.65), dec!(0.85));

        let result = evaluate_edge(&candidate, &valuation, &config).unwrap();
        assert_eq!(result.raw_edge, dec!(0.15));
        assert_eq!(result.side, Side::Yes);
    }

    #[test]
    fn test_edge_buy_no() {
        let config = test_config();
        let candidate = test_candidate(dec!(0.70));
        // Claude says 50% probability, market says 70% → 20% edge → buy NO
        let valuation = test_valuation(dec!(0.50), dec!(0.85));

        let result = evaluate_edge(&candidate, &valuation, &config).unwrap();
        assert_eq!(result.raw_edge, dec!(0.20));
        assert_eq!(result.side, Side::No);
    }

    #[test]
    fn test_edge_below_threshold() {
        let config = test_config();
        let candidate = test_candidate(dec!(0.50));
        // 3% edge at high confidence (threshold 6%) → no trade
        let valuation = test_valuation(dec!(0.53), dec!(0.85));

        let result = evaluate_edge(&candidate, &valuation, &config);
        assert!(result.is_none());
    }

    #[test]
    fn test_edge_low_confidence_skipped() {
        let config = test_config();
        let candidate = test_candidate(dec!(0.50));
        // Big edge but confidence too low → skip
        let valuation = test_valuation(dec!(0.80), dec!(0.30));

        let result = evaluate_edge(&candidate, &valuation, &config);
        assert!(result.is_none());
    }

    #[test]
    fn test_edge_threshold_varies_by_confidence() {
        let config = test_config();

        // High confidence → 6% threshold
        let high = test_valuation(dec!(0.50), dec!(0.90));
        assert_eq!(edge_threshold(&high, &config), dec!(0.06));

        // Medium confidence → 8% threshold
        let medium = test_valuation(dec!(0.50), dec!(0.60));
        assert_eq!(edge_threshold(&medium, &config), dec!(0.08));

        // Low confidence → 10% threshold
        let low = test_valuation(dec!(0.50), dec!(0.45));
        assert_eq!(edge_threshold(&low, &config), dec!(0.10));
    }

    #[test]
    fn test_low_data_quality_skipped() {
        let config = test_config();
        let candidate = test_candidate(dec!(0.50));
        let mut valuation = test_valuation(dec!(0.80), dec!(0.85));
        valuation.data_quality = DataQuality::Low;

        let result = evaluate_edge(&candidate, &valuation, &config);
        assert!(result.is_none());
    }
}
