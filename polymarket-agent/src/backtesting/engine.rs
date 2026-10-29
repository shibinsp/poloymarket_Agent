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
    pub taker_fee_pct: Decimal,
    /// Simulated API cost per market evaluation.
    pub simulated_api_cost_per_eval: Decimal,
    /// Maximum number of evaluations per cycle.
    pub max_evaluations_per_cycle: usize,
    /// Skip external valuation calls and use a supplied out-of-sample forecast
    /// when available; otherwise use the market prior.
    pub skip_valuation: bool,
}

impl BacktestConfig {
    pub fn from_app_config(config: &AppConfig) -> Self {
        Self {
            initial_balance: config.agent.initial_paper_balance,
            risk_config: config.risk.clone(),
            valuation_config: config.valuation.clone(),
            taker_fee_pct: config.polymarket.taker_fee_pct,
            simulated_api_cost_per_eval: dec!(0.01),
            max_evaluations_per_cycle: 10,
            skip_valuation: true, // Default: no Claude calls during backtest
        }
    }
}

/// Run a backtest over historical snapshots.
///
/// A forecast supplied in the historical file is used only if it was created
/// out of sample at that snapshot's timestamp. The resolved outcome is used
/// exclusively as the target for settlement, never to construct a forecast.
pub fn run_backtest(snapshots: &[HistoricalSnapshot], config: &BacktestConfig) -> BacktestResults {
    let mut tracker = BacktestTracker::new(config.initial_balance);
    let mut portfolio = PortfolioManager::new(config.risk_config.clone());
    let mut trade_index = 0usize;

    // Group snapshots into cycles of max_evaluations_per_cycle
    let cycles: Vec<&[HistoricalSnapshot]> =
        snapshots.chunks(config.max_evaluations_per_cycle).collect();

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

            if !(Decimal::ZERO..=Decimal::ONE).contains(&resolved_outcome)
                || (resolved_outcome != Decimal::ZERO && resolved_outcome != Decimal::ONE)
                || !(Decimal::ZERO..=Decimal::ONE).contains(&snapshot.yes_price)
                || !(Decimal::ZERO..=Decimal::ONE).contains(&snapshot.no_price)
                || snapshot.model_probability.is_some_and(|probability| {
                    !(Decimal::ZERO..=Decimal::ONE).contains(&probability)
                })
            {
                warn!(
                    market_id = %snapshot.market_id,
                    "Skipping historical snapshot with invalid probability or outcome"
                );
                continue;
            }

            if let Some(model_probability) = snapshot.model_probability {
                tracker.record_forecast(model_probability, snapshot.yes_price, resolved_outcome);
            }

            // Convert to candidate
            let candidate = historical::snapshot_to_candidate(snapshot);

            // A missing model forecast is just the market prior. Never use
            // `resolved_outcome` to synthesize an edge: that leaks the answer
            // into the strategy and makes the backtest meaningless.
            let simulated_fair_value = if config.skip_valuation {
                snapshot.model_probability.unwrap_or(snapshot.yes_price)
            } else {
                // Backtests do not call the live model. Use the no-signal
                // market prior unless an out-of-sample forecast was supplied.
                snapshot.yes_price
            };

            let confidence = dec!(0.75); // Simulated confidence
            let valuation = crate::valuation::fair_value::ValuationResult {
                probability: simulated_fair_value,
                confidence,
                reasoning_summary: "Historical out-of-sample forecast".to_string(),
                key_factors: vec![],
                data_quality: crate::valuation::fair_value::DataQuality::High,
                time_sensitivity: crate::valuation::fair_value::TimeSensitivity::Days,
            };
            let Some(edge_result) = edge::evaluate_edge(
                &candidate,
                &valuation,
                &config.valuation_config,
                config.taker_fee_pct,
            ) else {
                continue;
            };
            let side = edge_result.side;
            let trade_price = edge_result.trade_price;

            // Kelly sizing
            let kelly_result = kelly::kelly_size(
                edge_result.fair_probability,
                trade_price * (Decimal::ONE + config.taker_fee_pct),
                confidence,
                tracker.balance(),
                state,
                &config.risk_config,
            );

            if !kelly_result.should_trade() {
                continue;
            }

            // Portfolio constraint check
            let opp = edge::to_opportunity(
                &candidate,
                &valuation,
                &edge_result,
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

            // Liquidity check — use the order-book side matching the traded
            // side, consistent with the live-trading path in lifecycle.rs.
            let depth = limits::depth_at_best(
                &candidate
                    .order_book
                    .levels_for_side(side)
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
            let all_in_price = trade_price * (Decimal::ONE + config.taker_fee_pct);
            let shares = if all_in_price > Decimal::ZERO {
                liquidity_size / all_in_price
            } else {
                continue;
            };
            let entry_fee = shares * trade_price * config.taker_fee_pct;

            let trade = SimulatedTrade {
                market_id: snapshot.market_id.clone(),
                question: snapshot.question.clone(),
                side,
                entry_price: trade_price,
                size_usd: liquidity_size,
                shares,
                fair_value: simulated_fair_value,
                edge: edge_result.net_edge,
                confidence,
                entry_fee_usd: entry_fee,
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
                token_id: format!(
                    "{}_{}",
                    snapshot.market_id,
                    if side == Side::Yes { "yes" } else { "no" }
                ),
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
                ..RiskConfig::default()
            },
            valuation_config: ValuationConfig {
                provider: crate::config::LlmProvider::Anthropic,
                model: "claude-sonnet-4-5-20250929".to_string(),
                base_url: None,
                input_price_per_million: None,
                output_price_per_million: None,
                max_tokens: 1024,
                min_edge_threshold: dec!(0.05),
                high_confidence_edge: dec!(0.03),
                low_confidence_edge: dec!(0.08),
                cache_ttl_seconds: 300,
            },
            taker_fee_pct: dec!(0.02),
            simulated_api_cost_per_eval: dec!(0.01),
            max_evaluations_per_cycle: 10,
            skip_valuation: true,
        }
    }

    #[test]
    fn test_backtest_without_forecasts_does_not_invent_an_edge_from_outcomes() {
        let snapshots = historical::generate_synthetic(50);
        let config = test_config();

        let results = run_backtest(&snapshots, &config);

        assert_eq!(results.total_trades, 0);
        // API costs should be tracked
        assert!(results.total_api_cost > Decimal::ZERO);
        // Final balance should exist
        assert!(results.final_balance > Decimal::ZERO);
    }

    #[test]
    fn test_backtest_uses_supplied_forecast_and_executable_prices() {
        let mut snapshots = historical::generate_synthetic(10);
        for snapshot in &mut snapshots {
            snapshot.yes_price = dec!(0.50);
            snapshot.no_price = dec!(0.50);
            snapshot.spread = dec!(0.02);
            snapshot.resolved_outcome = Some(Decimal::ONE);
            snapshot.model_probability = Some(dec!(0.90));
        }

        let results = run_backtest(&snapshots, &test_config());
        assert!(results.total_trades > 0);
        assert!(results.total_pnl > Decimal::ZERO);
        assert_eq!(results.evaluated_forecasts, 10);
        assert_eq!(results.model_brier_score, Some(dec!(0.01)));
        assert_eq!(results.market_brier_score, Some(dec!(0.25)));
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
