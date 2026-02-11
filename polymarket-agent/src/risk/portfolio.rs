//! Portfolio state and constraint tracking.
//!
//! Tracks current positions and enforces portfolio-level risk limits.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::info;

use crate::config::RiskConfig;
use crate::market::models::{MarketCategory, Opportunity, Side};

/// Tracks the current portfolio state for risk management.
pub struct PortfolioManager {
    config: RiskConfig,
    positions: Vec<Position>,
}

/// A tracked position in the portfolio.
#[derive(Debug, Clone)]
pub struct Position {
    pub market_id: String,
    pub token_id: String,
    pub category: MarketCategory,
    pub side: Side,
    pub size_usd: Decimal,
    pub entry_price: Decimal,
}

impl PortfolioManager {
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            positions: Vec::new(),
        }
    }

    /// Check if a new opportunity passes all portfolio constraints.
    pub fn check_constraints(&self, opportunity: &Opportunity, bankroll: Decimal) -> ConstraintCheck {
        let mut violations = Vec::new();

        // 1. Max total exposure
        let current_exposure = self.total_exposure();
        let new_exposure = current_exposure + opportunity.kelly_size;
        let max_exposure = bankroll * self.config.max_total_exposure_pct;
        if new_exposure > max_exposure {
            violations.push(format!(
                "Total exposure {new_exposure} would exceed max {max_exposure}"
            ));
        }

        // 2. Max positions per category
        let category_count = self.positions_in_category(&opportunity.market.category);
        if category_count >= self.config.max_positions_per_category as usize {
            violations.push(format!(
                "Already {} positions in {:?} (max {})",
                category_count,
                opportunity.market.category,
                self.config.max_positions_per_category
            ));
        }

        // 3. No duplicate position in same market
        if self.has_position(&opportunity.market.condition_id) {
            violations.push(format!(
                "Already have position in market {}",
                opportunity.market.condition_id
            ));
        }

        // 4. Spread check (order book liquidity)
        let _max_spread = self.config.max_position_pct; // Re-use as spread proxy
        if opportunity.order_book.spread > dec!(0.05) {
            violations.push(format!(
                "Spread {:.2}% too wide (max 5%)",
                opportunity.order_book.spread * dec!(100)
            ));
        }

        if violations.is_empty() {
            ConstraintCheck::Pass
        } else {
            ConstraintCheck::Fail(violations)
        }
    }

    /// Reduce position size to fit within portfolio constraints.
    pub fn adjust_size(&self, size: Decimal, bankroll: Decimal) -> Decimal {
        let current_exposure = self.total_exposure();
        let max_exposure = bankroll * self.config.max_total_exposure_pct;
        let remaining_capacity = max_exposure - current_exposure;

        if remaining_capacity <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        size.min(remaining_capacity)
    }

    /// Record a new position in the portfolio.
    pub fn add_position(&mut self, position: Position) {
        info!(
            market_id = %position.market_id,
            side = %position.side,
            size = %position.size_usd,
            "Position added to portfolio"
        );
        self.positions.push(position);
    }

    /// Remove a position (e.g., on market resolution).
    pub fn remove_position(&mut self, market_id: &str) {
        self.positions.retain(|p| p.market_id != market_id);
    }

    /// Total USD exposure across all positions.
    pub fn total_exposure(&self) -> Decimal {
        self.positions.iter().map(|p| p.size_usd).sum()
    }

    /// Number of positions in a given category.
    fn positions_in_category(&self, category: &MarketCategory) -> usize {
        self.positions
            .iter()
            .filter(|p| &p.category == category)
            .count()
    }

    /// Whether we already have a position in a given market.
    fn has_position(&self, market_id: &str) -> bool {
        self.positions.iter().any(|p| p.market_id == market_id)
    }

    /// Current number of open positions.
    pub fn position_count(&self) -> usize {
        self.positions.len()
    }
}

/// Result of portfolio constraint checking.
#[derive(Debug)]
pub enum ConstraintCheck {
    Pass,
    Fail(Vec<String>),
}

impl ConstraintCheck {
    pub fn passed(&self) -> bool {
        matches!(self, ConstraintCheck::Pass)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::models::{
        Market, MarketCategory, OrderBookSnapshot, PriceLevel, TokenInfo,
    };
    use chrono::Utc;

    fn test_config() -> RiskConfig {
        RiskConfig {
            kelly_fraction: dec!(0.5),
            max_position_pct: dec!(0.06),
            max_total_exposure_pct: dec!(0.30),
            max_positions_per_category: 3,
            min_position_usd: dec!(1),
        }
    }

    fn test_opportunity(market_id: &str, category: MarketCategory, kelly_size: Decimal) -> Opportunity {
        Opportunity {
            market: Market {
                condition_id: market_id.to_string(),
                question: "Test?".to_string(),
                outcomes: vec!["Yes".to_string(), "No".to_string()],
                tokens: vec![TokenInfo {
                    token_id: "tok1".to_string(),
                    outcome: "Yes".to_string(),
                    price: dec!(0.50),
                }],
                end_date: Utc::now() + chrono::Duration::days(7),
                category,
                volume_24h: dec!(10000),
                active: true,
            },
            order_book: OrderBookSnapshot {
                token_id: "tok1".to_string(),
                bids: vec![PriceLevel { price: dec!(0.48), size: dec!(100) }],
                asks: vec![PriceLevel { price: dec!(0.52), size: dec!(100) }],
                spread: dec!(0.04),
                midpoint: dec!(0.50),
                implied_probability: dec!(0.50),
                timestamp: Utc::now(),
            },
            fair_value: dec!(0.65),
            confidence: dec!(0.85),
            edge: dec!(0.15),
            recommended_side: Side::Yes,
            kelly_size,
        }
    }

    #[test]
    fn test_portfolio_pass_constraints() {
        let pm = PortfolioManager::new(test_config());
        let opp = test_opportunity("m1", MarketCategory::Weather, dec!(5));
        let result = pm.check_constraints(&opp, dec!(100));
        assert!(result.passed());
    }

    #[test]
    fn test_portfolio_fail_exposure() {
        let mut pm = PortfolioManager::new(test_config());
        // Add positions totaling $28 of $100 bankroll
        for i in 0..4 {
            pm.add_position(Position {
                market_id: format!("m{i}"),
                token_id: format!("t{i}"),
                category: MarketCategory::Sports,
                side: Side::Yes,
                size_usd: dec!(7),
                entry_price: dec!(0.50),
            });
        }
        assert_eq!(pm.total_exposure(), dec!(28));

        // $5 more would bring to $33, exceeding 30% of $100
        let opp = test_opportunity("m5", MarketCategory::Weather, dec!(5));
        let result = pm.check_constraints(&opp, dec!(100));
        assert!(!result.passed());
    }

    #[test]
    fn test_portfolio_fail_category_limit() {
        let mut pm = PortfolioManager::new(test_config());
        // Add 3 crypto positions (max is 3)
        for i in 0..3 {
            pm.add_position(Position {
                market_id: format!("c{i}"),
                token_id: format!("t{i}"),
                category: MarketCategory::Crypto,
                side: Side::Yes,
                size_usd: dec!(2),
                entry_price: dec!(0.50),
            });
        }

        let opp = test_opportunity("c3", MarketCategory::Crypto, dec!(2));
        let result = pm.check_constraints(&opp, dec!(100));
        assert!(!result.passed());
    }

    #[test]
    fn test_portfolio_fail_duplicate() {
        let mut pm = PortfolioManager::new(test_config());
        pm.add_position(Position {
            market_id: "m1".to_string(),
            token_id: "t1".to_string(),
            category: MarketCategory::Weather,
            side: Side::Yes,
            size_usd: dec!(3),
            entry_price: dec!(0.50),
        });

        let opp = test_opportunity("m1", MarketCategory::Weather, dec!(3));
        let result = pm.check_constraints(&opp, dec!(100));
        assert!(!result.passed());
    }

    #[test]
    fn test_adjust_size() {
        let mut pm = PortfolioManager::new(test_config());
        // Current exposure: $20
        pm.add_position(Position {
            market_id: "m1".to_string(),
            token_id: "t1".to_string(),
            category: MarketCategory::Weather,
            side: Side::Yes,
            size_usd: dec!(20),
            entry_price: dec!(0.50),
        });

        // Max exposure: 30% of $100 = $30, remaining = $10
        let adjusted = pm.adjust_size(dec!(15), dec!(100));
        assert_eq!(adjusted, dec!(10));
    }

    #[test]
    fn test_remove_position() {
        let mut pm = PortfolioManager::new(test_config());
        pm.add_position(Position {
            market_id: "m1".to_string(),
            token_id: "t1".to_string(),
            category: MarketCategory::Weather,
            side: Side::Yes,
            size_usd: dec!(5),
            entry_price: dec!(0.50),
        });
        assert_eq!(pm.position_count(), 1);

        pm.remove_position("m1");
        assert_eq!(pm.position_count(), 0);
    }
}
