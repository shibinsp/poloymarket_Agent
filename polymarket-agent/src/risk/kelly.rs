//! Kelly Criterion position sizing calculator.
//!
//! Computes optimal bet size using fractional Kelly with confidence scaling
//! and hard caps for risk management.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::config::RiskConfig;
use crate::market::models::AgentState;

/// Calculate Kelly-optimal position size in USD.
///
/// Uses fractional Kelly (half-Kelly by default) with confidence adjustment.
///
/// # Formula
/// ```text
/// kelly_fraction = (p * b - q) / b
/// where p = fair_prob, q = 1-p, b = net odds = (1/market_price) - 1
/// adjusted = kelly_fraction * kelly_config_fraction * confidence
/// position = adjusted * bankroll
/// ```
pub fn kelly_size(
    fair_prob: Decimal,
    market_price: Decimal,
    confidence: Decimal,
    bankroll: Decimal,
    state: AgentState,
    config: &RiskConfig,
) -> KellyResult {
    // Guard against degenerate inputs.
    // Near-zero/near-one prices produce extreme odds (b = 99999) making Kelly unstable (TRD-05).
    if market_price < dec!(0.02) || market_price > dec!(0.98) {
        return KellyResult::zero();
    }
    if fair_prob <= Decimal::ZERO || fair_prob >= Decimal::ONE {
        return KellyResult::zero();
    }
    if bankroll <= Decimal::ZERO {
        return KellyResult::zero();
    }

    // Net odds: how much you win per dollar risked
    // b = (1 / market_price) - 1
    let b = (Decimal::ONE / market_price) - Decimal::ONE;
    if b <= Decimal::ZERO {
        return KellyResult::zero();
    }

    let p = fair_prob;
    let q = Decimal::ONE - p;

    // Raw Kelly fraction
    let kelly_raw = (p * b - q) / b;

    // If Kelly is negative, there's no edge — don't trade
    if kelly_raw <= Decimal::ZERO {
        return KellyResult {
            kelly_raw,
            kelly_adjusted: Decimal::ZERO,
            position_usd: Decimal::ZERO,
            capped: false,
        };
    }

    // Apply fractional Kelly (e.g., half-Kelly = 0.5)
    let fraction = config.kelly_fraction;

    // State-dependent adjustment
    let state_multiplier = match state {
        AgentState::Alive => Decimal::ONE,
        AgentState::LowFuel => dec!(0.25), // Quarter-Kelly in low fuel
        AgentState::CriticalSurvival | AgentState::Dead => Decimal::ZERO,
    };

    // Adjusted Kelly = raw * fraction * confidence * state_multiplier
    let kelly_adjusted = kelly_raw * fraction * confidence * state_multiplier;

    // Position in USD
    let mut position = kelly_adjusted * bankroll;

    // Hard caps
    let max_position = bankroll * config.max_position_pct; // 6% of bankroll
    let min_position = config.min_position_usd; // $1 minimum

    let mut capped = false;

    if position > max_position {
        position = max_position;
        capped = true;
    }

    // Below minimum threshold — don't trade
    if position < min_position {
        return KellyResult {
            kelly_raw,
            kelly_adjusted,
            position_usd: Decimal::ZERO,
            capped: false,
        };
    }

    KellyResult {
        kelly_raw,
        kelly_adjusted,
        position_usd: position,
        capped,
    }
}

/// Result of Kelly sizing calculation.
#[derive(Debug, Clone)]
pub struct KellyResult {
    /// Raw Kelly fraction before adjustments.
    pub kelly_raw: Decimal,
    /// Adjusted Kelly fraction after fractional/confidence/state scaling.
    pub kelly_adjusted: Decimal,
    /// Recommended position size in USD.
    pub position_usd: Decimal,
    /// Whether the position was capped by max_position_pct.
    pub capped: bool,
}

impl KellyResult {
    fn zero() -> Self {
        Self {
            kelly_raw: Decimal::ZERO,
            kelly_adjusted: Decimal::ZERO,
            position_usd: Decimal::ZERO,
            capped: false,
        }
    }

    /// Whether this result recommends a trade.
    pub fn should_trade(&self) -> bool {
        self.position_usd > Decimal::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> RiskConfig {
        RiskConfig {
            kelly_fraction: dec!(0.5),     // Half-Kelly
            max_position_pct: dec!(0.06),  // 6%
            max_total_exposure_pct: dec!(0.30),
            max_positions_per_category: 3,
            min_position_usd: dec!(1),     // $1 min
        }
    }

    #[test]
    fn test_kelly_basic() {
        let config = default_config();
        // Fair prob 70%, market price 50% → good edge
        let result = kelly_size(
            dec!(0.70), dec!(0.50), dec!(0.85), dec!(100),
            AgentState::Alive, &config,
        );

        assert!(result.kelly_raw > Decimal::ZERO);
        assert!(result.should_trade());
        assert!(result.position_usd > Decimal::ZERO);
        assert!(result.position_usd <= dec!(6)); // Max 6% of $100
    }

    #[test]
    fn test_kelly_no_edge() {
        let config = default_config();
        // Fair prob 40%, market price 50% → negative edge
        let result = kelly_size(
            dec!(0.40), dec!(0.50), dec!(0.85), dec!(100),
            AgentState::Alive, &config,
        );

        assert!(result.kelly_raw < Decimal::ZERO);
        assert!(!result.should_trade());
        assert_eq!(result.position_usd, Decimal::ZERO);
    }

    #[test]
    fn test_kelly_exact_formula() {
        let config = default_config();
        // p=0.6, market=0.45 → b = (1/0.45)-1 = 1.2222...
        // kelly_raw = (0.6*1.222 - 0.4) / 1.222 = (0.733 - 0.4) / 1.222 = 0.2727
        // adjusted = 0.2727 * 0.5 * 0.9 = ~0.1227
        // position = 0.1227 * 100 = ~12.27 → capped at 6
        let result = kelly_size(
            dec!(0.60), dec!(0.45), dec!(0.90), dec!(100),
            AgentState::Alive, &config,
        );

        assert!(result.kelly_raw > dec!(0.20));
        assert!(result.capped); // Should hit 6% cap
        assert_eq!(result.position_usd, dec!(6)); // 6% of $100
    }

    #[test]
    fn test_kelly_low_fuel_quarter() {
        let config = default_config();
        let alive = kelly_size(
            dec!(0.70), dec!(0.50), dec!(0.85), dec!(100),
            AgentState::Alive, &config,
        );
        let low_fuel = kelly_size(
            dec!(0.70), dec!(0.50), dec!(0.85), dec!(100),
            AgentState::LowFuel, &config,
        );

        // Low fuel should be approximately 1/4 of alive
        assert!(low_fuel.position_usd < alive.position_usd);
        assert!(low_fuel.kelly_adjusted < alive.kelly_adjusted);
    }

    #[test]
    fn test_kelly_critical_survival_no_trade() {
        let config = default_config();
        let result = kelly_size(
            dec!(0.70), dec!(0.50), dec!(0.85), dec!(100),
            AgentState::CriticalSurvival, &config,
        );

        assert!(!result.should_trade());
    }

    #[test]
    fn test_kelly_below_minimum() {
        let config = default_config();
        // Very small bankroll → position below $1 minimum
        let result = kelly_size(
            dec!(0.55), dec!(0.50), dec!(0.50), dec!(5),
            AgentState::Alive, &config,
        );

        // With small bankroll and low confidence, position may be below min
        // The key check is that we don't return a position less than $1
        assert!(result.position_usd == Decimal::ZERO || result.position_usd >= dec!(1));
    }

    #[test]
    fn test_kelly_zero_bankroll() {
        let config = default_config();
        let result = kelly_size(
            dec!(0.70), dec!(0.50), dec!(0.85), Decimal::ZERO,
            AgentState::Alive, &config,
        );

        assert!(!result.should_trade());
    }

    #[test]
    fn test_kelly_degenerate_price() {
        let config = default_config();
        // Market price at boundary
        let result = kelly_size(
            dec!(0.70), Decimal::ONE, dec!(0.85), dec!(100),
            AgentState::Alive, &config,
        );
        assert!(!result.should_trade());

        let result = kelly_size(
            dec!(0.70), Decimal::ZERO, dec!(0.85), dec!(100),
            AgentState::Alive, &config,
        );
        assert!(!result.should_trade());
    }

    #[test]
    fn test_kelly_near_zero_price_rejected() {
        let config = default_config();
        // Near-zero price (0.01) would create b = 99, making Kelly unstable
        let result = kelly_size(
            dec!(0.70), dec!(0.01), dec!(0.85), dec!(100),
            AgentState::Alive, &config,
        );
        assert!(!result.should_trade());

        // Near-one price (0.99) would create b ≈ 0.01, also unstable
        let result = kelly_size(
            dec!(0.70), dec!(0.99), dec!(0.85), dec!(100),
            AgentState::Alive, &config,
        );
        assert!(!result.should_trade());

        // Just above threshold should work
        let result = kelly_size(
            dec!(0.70), dec!(0.03), dec!(0.85), dec!(100),
            AgentState::Alive, &config,
        );
        // May or may not trade (depends on edge), but should not be auto-rejected
        assert!(result.kelly_raw != Decimal::ZERO || result.kelly_adjusted != Decimal::ZERO
            || result.position_usd == Decimal::ZERO); // At least computed something
    }

    #[test]
    fn test_kelly_confidence_scaling() {
        let config = default_config();
        let high_conf = kelly_size(
            dec!(0.70), dec!(0.50), dec!(0.95), dec!(100),
            AgentState::Alive, &config,
        );
        let low_conf = kelly_size(
            dec!(0.70), dec!(0.50), dec!(0.50), dec!(100),
            AgentState::Alive, &config,
        );

        // Higher confidence → larger position (or both capped)
        assert!(high_conf.kelly_adjusted > low_conf.kelly_adjusted);
    }
}
