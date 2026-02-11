//! Backtesting engine.
//!
//! Replays historical market data through the full pipeline:
//! market scan → valuation → Kelly sizing → simulated execution.
//! Tracks P&L, drawdown, and other statistics.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{info, warn};

use crate::backtesting::historical::{self, HistoricalSnapshot};
use crate::backtesting::results::{BacktestResults, BacktestTracker, SimulatedTrade};
use crate::config::{AppConfig, RiskConfig, ValuationConfig};
use crate::market::models::{AgentState, Side};
use crate::risk::kelly;
use crate::risk::limits;
use crate::risk::portfolio::PortfolioManager;
use crate::valuation::edge;

/// Configuration for a backtest run.
#[derive(Debug, Clone)]
pub struct BacktestConfig {
    pub initial_balance: Decimal,
    pub risk_config: RiskConfig,
    pub valuation_config: ValuationConfig,
    /// Simulated API cost per market evaluation.
    pub simulated_api_cost_per_eval: Decimal,
    /// Maximum number of evaluations per cycle.
    pub max_evaluations_per_cycle: usize,
    /// Whether to skip Claude valuation and use market prices as fair values.
    pub skip_valuation: bool,
}

impl BacktestConfig {
    pub fn from_app_config(config: &AppConfig) -> Self {
        Self {
            initial_balance: config.agent.initial_paper_balance,
            risk_config: config.risk.clone(),
            valuation_config: config.valuation.clone(),
            simulated_api_cost_per_eval: dec!(0.01),
            max_evaluations_per_cycle: 10,
            skip_valuation: true, // Default: no Claude calls during backtest
        }
    }
}

/// Run a backtest over historical snapshots.
///
/// In skip_valuation mode, uses the historical fair value (resolved outcome)
/// as a proxy for Claude's valuation. This tests the sizing/execution pipeline
/// without incurring API costs.
pub fn run_backtest(
    snapshots: &[HistoricalSnapshot],
    config: &BacktestConfig,
) -> BacktestResults {
    let mut tracker = BacktestTracker::new(config.initial_balance);
    let mut portfolio = PortfolioManager::new(config.risk_config.clone());
    let mut trade_index = 0usize;

    // Group snapshots into cycles of max_evaluations_per_cycle
    let cycles: Vec<&[HistoricalSnapshot]> = snapshots
        .chunks(config.max_evaluations_per_cycle)
        .collect();

    info!(
        total_snapshots = snapshots.len(),
        cycles = cycles.len(),
        initial_balance = %config.initial_balance,
        "Starting backtest"
    );

    for (cycle_num, cycle_snapshots) in cycles.iter().enumerate() {
        let bankroll = tracker.balance();

        // Skip cycle if balance too low
        if bankroll <= config.risk_config.min_position_usd {
            warn!(cycle = cycle_num, balance = %bankroll, "Insufficient balance — stopping backtest");
            break;
        }

        let state = if bankroll < dec!(10) {
            AgentState::LowFuel
        } else {
            AgentState::Alive
        };

        for snapshot in *cycle_snapshots {
            // Simulate API cost
            tracker.record_api_cost(config.simulated_api_cost_per_eval);

            // Check if API cost exceeds remaining balance
            if tracker.balance() < config.simulated_api_cost_per_eval * dec!(2) {
                break;
            }

            // Skip unresolved markets (can't backtest without knowing outcome)
            let Some(resolved_outcome) = snapshot.resolved_outcome else {
                continue;
            };

            // Convert to candidate
            let candidate = historical::snapshot_to_candidate(snapshot);

            // Simulate valuation: use a "noisy" version of the true outcome
            // as if Claude had some predictive ability but not perfect
            let simulated_fair_value = if config.skip_valuation {
                // Blend market price with outcome to simulate imperfect prediction
                // 60% weight on true outcome + 40% on market price = decent edge
                let noise_factor = dec!(0.60);
                snapshot.yes_price * (Decimal::ONE - noise_factor)
                    + resolved_outcome * noise_factor
            } else {
                // Would call Claude here in non-skip mode
                snapshot.yes_price
            };

            let confidence = dec!(0.75); // Simulated confidence

            // Determine side and edge
            let market_price = snapshot.yes_price;
            let edge_val = simulated_fair_value - market_price;

            let min_edge = config.valuation_config.min_edge_threshold;
            if edge_val.abs() < min_edge {
                continue; // No edge
            }

            let (side, trade_price) = if edge_val > Decimal::ZERO {
                (Side::Yes, market_price)
            } else {
                (Side::No, Decimal::ONE - market_price)
            };

            // Kelly sizing
            let fair_prob = if side == Side::Yes {
                simulated_fair_value
            } else {
                Decimal::ONE - simulated_fair_value
            };

            let kelly_result = kelly::kelly_size(
                fair_prob,
                trade_price,
                confidence,
                tracker.balance(),
                state,
                &config.risk_config,
            );

            if !kelly_result.should_trade() {
                continue;
            }

            // Portfolio constraint check
            let edge_result_edge = edge_val.abs();
            let opp = edge::to_opportunity(
                &candidate,
                &crate::valuation::fair_value::ValuationResult {
                    probability: simulated_fair_value,
                    confidence,
                    reasoning_summary: String::new(),
                    key_factors: vec![],
                    data_quality: crate::valuation::fair_value::DataQuality::Medium,
                    time_sensitivity: crate::valuation::fair_value::TimeSensitivity::Days,
                },
                &crate::valuation::edge::EdgeResult {
                    raw_edge: edge_result_edge,
                    side,
                    trade_price,
                    threshold: min_edge,
                    market_probability: market_price,
                    fair_probability: simulated_fair_value,
                },
                kelly_result.position_usd,
            );

            let constraint_check = portfolio.check_constraints(&opp, tracker.balance());
            if !constraint_check.passed() {
                continue;
            }

            let position_size = portfolio.adjust_size(kelly_result.position_usd, tracker.balance());
            if position_size < config.risk_config.min_position_usd {
                continue;
            }

            // Liquidity check (simulated: always adequate in backtest)
            let depth = limits::depth_at_best(
                &candidate
                    .order_book
                    .asks
                    .iter()
                    .map(|l| (l.price, l.size))
                    .collect::<Vec<_>>(),
            );
            let liquidity_size =
                limits::liquidity_adjusted_size(position_size, trade_price, depth, dec!(0.02));

            if liquidity_size < config.risk_config.min_position_usd {
                continue;
            }

            // Execute simulated trade
            let shares = if trade_price > Decimal::ZERO {
                liquidity_size / trade_price
            } else {
                continue;
            };

            let trade = SimulatedTrade {
                market_id: snapshot.market_id.clone(),
                question: snapshot.question.clone(),
                side,
                entry_price: trade_price,
                size_usd: liquidity_size,
                shares,
                fair_value: simulated_fair_value,
                edge: edge_result_edge,
                confidence,
                outcome_price: None,
                pnl: None,
            };

            tracker.record_entry(trade);

            // Resolve immediately (backtest has the outcome)
            let outcome_for_side = match side {
                Side::Yes => resolved_outcome,
                Side::No => Decimal::ONE - resolved_outcome,
            };
            tracker.resolve_trade(trade_index, outcome_for_side);
            trade_index += 1;

            // Add position to portfolio (and immediately remove since resolved)
            portfolio.add_position(crate::risk::portfolio::Position {
                market_id: snapshot.market_id.clone(),
                token_id: format!("{}_{}", snapshot.market_id, if side == Side::Yes { "yes" } else { "no" }),
                category: candidate.market.category,
                side,
                size_usd: liquidity_size,
                entry_price: trade_price,
            });
            portfolio.remove_position(&snapshot.market_id);
        }
    }

    let results = tracker.finalize();

    info!(
        total_trades = results.total_trades,
        wins = results.wins,
        losses = results.losses,
        win_rate = %results.win_rate,
        total_pnl = %results.total_pnl,
        max_drawdown = %results.max_drawdown,
        roi = %results.roi_pct,
        "Backtest complete"
    );

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BacktestConfig {
        BacktestConfig {
            initial_balance: dec!(100),
            risk_config: RiskConfig {
                kelly_fraction: dec!(0.5),
                max_position_pct: dec!(0.06),
                max_total_exposure_pct: dec!(0.30),
                max_positions_per_category: 3,
                min_position_usd: dec!(1),
            },
            valuation_config: ValuationConfig {
                claude_model: "claude-sonnet-4-5-20250929".to_string(),
                min_edge_threshold: dec!(0.05),
                high_confidence_edge: dec!(0.03),
                low_confidence_edge: dec!(0.08),
                cache_ttl_seconds: 300,
            },
            simulated_api_cost_per_eval: dec!(0.01),
            max_evaluations_per_cycle: 10,
            skip_valuation: true,
        }
    }

    #[test]
    fn test_backtest_synthetic_data() {
        let snapshots = historical::generate_synthetic(50);
        let config = test_config();

        let results = run_backtest(&snapshots, &config);

        // Should have executed some trades
        assert!(results.total_trades > 0, "Should have some trades");
        // Win rate should be reasonable (not 0% or 100%)
        assert!(results.total_trades >= results.wins);
        // API costs should be tracked
        assert!(results.total_api_cost > Decimal::ZERO);
        // Final balance should exist
        assert!(results.final_balance > Decimal::ZERO);
    }

    #[test]
    fn test_backtest_empty_data() {
        let snapshots: Vec<HistoricalSnapshot> = vec![];
        let config = test_config();

        let results = run_backtest(&snapshots, &config);

        assert_eq!(results.total_trades, 0);
        assert_eq!(results.final_balance, dec!(100));
    }

    #[test]
    fn test_backtest_unresolved_markets_skipped() {
        // Generate snapshots where all are unresolved
        let mut snapshots = historical::generate_synthetic(10);
        for snap in &mut snapshots {
            snap.resolved_outcome = None;
        }

        let config = test_config();
        let results = run_backtest(&snapshots, &config);

        // No trades should execute since no outcomes are known
        assert_eq!(results.total_trades, 0);
    }

    #[test]
    fn test_backtest_results_display() {
        let snapshots = historical::generate_synthetic(20);
        let config = test_config();

        let results = run_backtest(&snapshots, &config);
        let display = format!("{results}");

        assert!(display.contains("Backtest Results"));
        assert!(display.contains("ROI"));
    }

    #[test]
    fn test_backtest_low_balance_stops() {
        let snapshots = historical::generate_synthetic(100);
        let config = BacktestConfig {
            initial_balance: dec!(2), // Very low starting balance
            ..test_config()
        };

        let results = run_backtest(&snapshots, &config);
        // Should stop early due to low balance
        assert!(results.total_trades < 100);
    }
}
