//! API cost tracking and survival logic.
//!
//! Tracks every cost (Claude API calls, gas fees, VPS amortization)
//! and provides enhanced survival checks with unrealized PnL.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::info;

use crate::db::store::Store;
use crate::market::models::AgentState;

/// Fixed costs amortized per 10-minute cycle.
/// VPS: ~$4.50/month ÷ 30 days ÷ 144 cycles/day ≈ $0.001
const VPS_COST_PER_CYCLE: &str = "0.001";

/// Cost categories tracked by the agent.
#[derive(Debug, Clone)]
pub struct CycleCosts {
    /// Claude API cost this cycle.
    pub api_cost: Decimal,
    /// Polygon gas fees this cycle (minimal on Polygon).
    pub gas_cost: Decimal,
    /// Amortized VPS cost per cycle.
    pub vps_cost: Decimal,
}

impl CycleCosts {
    pub fn new(api_cost: Decimal) -> Self {
        Self {
            api_cost,
            gas_cost: dec!(0.0001), // Polygon gas is negligible
            vps_cost: VPS_COST_PER_CYCLE.parse().unwrap_or(dec!(0.001)),
        }
    }

    /// Total cost for this cycle.
    pub fn total(&self) -> Decimal {
        self.api_cost + self.gas_cost + self.vps_cost
    }
}

/// Estimate the cost of the next cycle based on recent history.
///
/// Uses the average of the last N cycles, or a default if no history.
pub async fn estimate_next_cycle_cost(store: &Store, lookback_cycles: u64) -> Decimal {
    let total = store.get_total_api_cost().await.unwrap_or(Decimal::ZERO);

    // Get cycle count from the latest cycle number
    let cycle_count = match store.get_latest_cycle().await {
        Ok(Some(c)) => (c.cycle_number + 1) as u64,
        _ => 0,
    };

    if cycle_count == 0 {
        // No history — use default estimate for one Claude call
        return dec!(0.01);
    }

    let effective_count = cycle_count.min(lookback_cycles);
    if effective_count == 0 {
        return dec!(0.01);
    }

    // Average API cost per cycle + fixed costs
    let avg_api = total / Decimal::from(effective_count);
    let fixed = CycleCosts::new(Decimal::ZERO);

    avg_api + fixed.gas_cost + fixed.vps_cost
}

/// Enhanced survival check that factors in unrealized PnL and projected costs.
pub fn enhanced_survival_check(
    wallet_balance: Decimal,
    unrealized_pnl: Decimal,
    next_cycle_cost: Decimal,
    death_threshold: Decimal,
    api_reserve: Decimal,
    low_fuel_threshold: Decimal,
) -> AgentState {
    let effective_balance = wallet_balance + unrealized_pnl;

    if effective_balance <= death_threshold {
        return AgentState::Dead;
    }

    // Can't afford next cycle's API costs
    if wallet_balance < next_cycle_cost + api_reserve {
        return AgentState::CriticalSurvival;
    }

    if wallet_balance < low_fuel_threshold {
        return AgentState::LowFuel;
    }

    AgentState::Alive
}

/// Check if a trade's projected edge justifies the API cost spent to find it.
///
/// Returns true if projected profit > API cost for the evaluation.
pub fn edge_justifies_cost(
    position_size: Decimal,
    edge: Decimal,
    api_cost_for_evaluation: Decimal,
) -> bool {
    let projected_profit = position_size * edge;
    projected_profit > api_cost_for_evaluation
}

/// Calculate the "burn rate" — average cost per cycle over the agent's lifetime.
pub async fn burn_rate(store: &Store) -> Decimal {
    let total = store.get_total_api_cost().await.unwrap_or(Decimal::ZERO);
    let cycle_count = match store.get_latest_cycle().await {
        Ok(Some(c)) => c.cycle_number + 1,
        _ => return Decimal::ZERO,
    };

    if cycle_count <= 0 {
        return Decimal::ZERO;
    }

    let fixed_per_cycle = CycleCosts::new(Decimal::ZERO);
    let avg_api = total / Decimal::from(cycle_count);
    avg_api + fixed_per_cycle.gas_cost + fixed_per_cycle.vps_cost
}

/// Log a detailed cost breakdown for the current cycle.
pub fn log_cost_breakdown(cycle: u64, costs: &CycleCosts, cumulative_api_cost: Decimal) {
    info!(
        cycle,
        api_cost = %costs.api_cost,
        gas_cost = %costs.gas_cost,
        vps_cost = %costs.vps_cost,
        total_cycle_cost = %costs.total(),
        cumulative_api_cost = %cumulative_api_cost,
        "Cycle cost breakdown"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cycle_costs() {
        let costs = CycleCosts::new(dec!(0.05));
        assert_eq!(costs.api_cost, dec!(0.05));
        assert!(costs.total() > dec!(0.05)); // Includes gas + VPS
        assert!(costs.total() < dec!(0.06)); // But not much more
    }

    #[test]
    fn test_enhanced_survival_alive() {
        let state = enhanced_survival_check(
            dec!(100),  // wallet
            dec!(5),    // unrealized PnL
            dec!(0.05), // next cycle cost
            dec!(0),    // death threshold
            dec!(2),    // api reserve
            dec!(10),   // low fuel
        );
        assert_eq!(state, AgentState::Alive);
    }

    #[test]
    fn test_enhanced_survival_dead_with_pnl() {
        // Wallet $0 but unrealized PnL $5 — still alive because effective > 0
        let state = enhanced_survival_check(
            dec!(0),
            dec!(5),
            dec!(0.05),
            dec!(0),
            dec!(2),
            dec!(10),
        );
        // Wallet < next_cycle_cost + api_reserve → CriticalSurvival
        assert_eq!(state, AgentState::CriticalSurvival);
    }

    #[test]
    fn test_enhanced_survival_dead() {
        let state = enhanced_survival_check(
            dec!(0),
            dec!(-1), // negative unrealized
            dec!(0.05),
            dec!(0),
            dec!(2),
            dec!(10),
        );
        assert_eq!(state, AgentState::Dead);
    }

    #[test]
    fn test_enhanced_survival_critical() {
        // Wallet $1.50, reserve $2, next cost $0.05 → can't afford
        let state = enhanced_survival_check(
            dec!(1.50),
            dec!(0),
            dec!(0.05),
            dec!(0),
            dec!(2),
            dec!(10),
        );
        assert_eq!(state, AgentState::CriticalSurvival);
    }

    #[test]
    fn test_enhanced_survival_low_fuel() {
        let state = enhanced_survival_check(
            dec!(8),    // below low_fuel threshold of $10
            dec!(0),
            dec!(0.05),
            dec!(0),
            dec!(2),
            dec!(10),
        );
        assert_eq!(state, AgentState::LowFuel);
    }

    #[test]
    fn test_edge_justifies_cost_yes() {
        // $5 position, 10% edge = $0.50 profit > $0.05 API cost
        assert!(edge_justifies_cost(dec!(5), dec!(0.10), dec!(0.05)));
    }

    #[test]
    fn test_edge_justifies_cost_no() {
        // $1 position, 1% edge = $0.01 profit < $0.05 API cost
        assert!(!edge_justifies_cost(dec!(1), dec!(0.01), dec!(0.05)));
    }

    #[tokio::test]
    async fn test_estimate_next_cycle_cost_no_history() {
        let store = Store::new(":memory:").await.unwrap();
        let cost = estimate_next_cycle_cost(&store, 10).await;
        assert_eq!(cost, dec!(0.01)); // Default estimate
    }

    #[tokio::test]
    async fn test_burn_rate_no_history() {
        let store = Store::new(":memory:").await.unwrap();
        let rate = burn_rate(&store).await;
        assert_eq!(rate, Decimal::ZERO);
    }
}
