//! Kelly Criterion tests — supplementary tests for calibration discount
//! and edge cases not covered by the unit tests in src/risk/kelly.rs.

use polymarket_agent::risk::kelly::kelly_size;
use polymarket_agent::config::RiskConfig;
use polymarket_agent::market::models::AgentState;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn default_config() -> RiskConfig {
    RiskConfig {
        kelly_fraction: dec!(0.5),
        max_position_pct: dec!(0.06),
        max_total_exposure_pct: dec!(0.30),
        max_positions_per_category: 3,
        min_position_usd: dec!(1),
    }
}

#[test]
fn kelly_with_calibration_discount_reduces_position() {
    let config = default_config();
    // Simulate calibration by reducing confidence externally before calling kelly_size.
    // Original confidence 0.90, discount factor 0.85 → effective confidence 0.765.
    let original = kelly_size(
        dec!(0.70), dec!(0.50), dec!(0.90), dec!(100),
        AgentState::Alive, &config,
    );
    let discounted = kelly_size(
        dec!(0.70), dec!(0.50), dec!(0.765), dec!(100),
        AgentState::Alive, &config,
    );

    // Discounted confidence should yield equal or smaller position.
    assert!(discounted.kelly_adjusted <= original.kelly_adjusted);
}

#[test]
fn kelly_cap_behavior_is_deterministic() {
    let config = default_config();
    // Large edge should hit the cap reliably.
    let result = kelly_size(
        dec!(0.90), dec!(0.30), dec!(0.95), dec!(1000),
        AgentState::Alive, &config,
    );
    assert!(result.capped);
    assert_eq!(result.position_usd, dec!(60)); // 6% of $1000
}

#[test]
fn kelly_symmetric_probabilities_zero_edge() {
    let config = default_config();
    // Fair prob equals market price → zero edge.
    let result = kelly_size(
        dec!(0.50), dec!(0.50), dec!(0.90), dec!(100),
        AgentState::Alive, &config,
    );
    assert!(!result.should_trade());
}

#[test]
fn kelly_near_unity_probability_still_caps() {
    let config = default_config();
    // Very high confidence in YES at a low price — should cap.
    let result = kelly_size(
        dec!(0.99), dec!(0.10), dec!(0.95), dec!(100),
        AgentState::Alive, &config,
    );
    assert!(result.capped);
    assert_eq!(result.position_usd, dec!(6));
}

#[test]
fn kelly_dead_state_always_zero() {
    let config = default_config();
    let result = kelly_size(
        dec!(0.90), dec!(0.30), dec!(0.95), dec!(1000),
        AgentState::Dead, &config,
    );
    assert!(!result.should_trade());
    assert_eq!(result.position_usd, Decimal::ZERO);
}
