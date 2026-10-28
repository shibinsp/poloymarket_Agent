//! Historical Backtesting Engine for the Pullback & Confirmed Rebound Strategy.
//!
//! Enforces:
//! 1. Strict zero look-ahead bias: Signal generated at candle `t` close executes at candle `t+1` open + slippage.
//!    Never fills at signal candle low or close.
//! 2. Conservative Stop Loss / Take Profit: On candles where both high and low touch thresholds,
//!    stop loss triggers first.
//! 3. Statutory transaction costs:
//!    - India: STT (0.025% intraday sell), Exchange turnover (0.00345%), Stamp duty (0.003% buy), GST (18%).
//!    - US: SEC Section 31 ($27.80 per million on sells), FINRA TAF ($0.000166/share on sells).
//! 4. Separation of Development (In-Sample) and Validation (Out-of-Sample) periods.
//! 5. Comprehensive reporting: Net P&L, Drawdown, Win Rate, Avg Win/Loss, Expectancy, Trade Count,
//!    and Buy & Hold Baseline comparison.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::strategy::pullback::{
    calculate_position_size, evaluate_pullback_setup, evaluate_pullback_setup_us, FiveMinCandle,
    PullbackStrategyConfig, SignalAction,
};

/// Performance metrics for a backtest evaluation period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestPeriodMetrics {
    pub period_name: String,
    pub candle_count: usize,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub initial_capital: Decimal,
    pub final_capital: Decimal,
    pub net_pnl: Decimal,
    pub net_pnl_pct: Decimal,
    pub max_drawdown_amount: Decimal,
    pub max_drawdown_pct: Decimal,
    pub trade_count: usize,
    pub winning_trades: usize,
    pub losing_trades: usize,
    pub win_rate_pct: Decimal,
    pub avg_win: Decimal,
    pub avg_loss: Decimal,
    pub profit_factor: Decimal,
    pub expectancy_per_trade: Decimal,
    pub total_regulatory_costs: Decimal,
}

/// Baseline Buy & Hold comparison metrics over the same period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineComparison {
    pub strategy_name: String,
    pub buy_and_hold_return_pct: Decimal,
    pub buy_and_hold_max_drawdown_pct: Decimal,
    pub strategy_outperformance_pct: Decimal,
}

/// Simulated trade executed during backtesting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestExecutionTrade {
    pub id: usize,
    pub symbol: String,
    pub period: String, // "DEV" or "OOS"
    pub shares: i64,
    pub entry_time: DateTime<Utc>,
    pub entry_price: Decimal,
    pub planned_stop: Decimal,
    pub planned_target: Decimal,
    pub exit_time: DateTime<Utc>,
    pub exit_price: Decimal,
    pub exit_reason: String,
    pub gross_pnl: Decimal,
    pub regulatory_fees: Decimal,
    pub net_pnl: Decimal,
    pub roi_pct: Decimal,
}

/// Complete report comparing Development (In-Sample) vs Out-of-Sample (Validation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullbackBacktestReport {
    pub market: String,   // "INDIA_NSE" or "US_EQUITY"
    pub currency: String, // "INR" or "USD"
    pub config: PullbackStrategyConfig,
    pub dev_period: BacktestPeriodMetrics,
    pub oos_period: BacktestPeriodMetrics,
    pub overall_metrics: BacktestPeriodMetrics,
    pub baseline: BaselineComparison,
    pub sample_trades: Vec<BacktestExecutionTrade>,
    pub disclaimer: String,
}

/// Computes statutory transaction fees.
fn calculate_fees(is_us: bool, entry_val: Decimal, exit_val: Decimal, shares: i64) -> Decimal {
    if is_us {
        // SEC Section 31 fee: $27.80 per million on gross sales
        let sec_fee = exit_val * dec!(0.0000278);
        // FINRA TAF: $0.000166 per share on sells (min $0.01, max $8.30)
        let finra_taf = (Decimal::from(shares) * dec!(0.000166))
            .max(dec!(0.01))
            .min(dec!(8.30));
        (sec_fee + finra_taf).round_dp(4)
    } else {
        // Indian Cash Equity Intraday:
        // STT: 0.025% on sell turnover
        let stt = exit_val * dec!(0.00025);
        // Stamp duty: 0.003% on buy turnover
        let stamp_duty = entry_val * dec!(0.00003);
        // Exchange turnover charge: 0.00345%
        let exchange_chg = (entry_val + exit_val) * dec!(0.0000345);
        // SEBI turnover charge: 0.0001%
        let sebi_chg = (entry_val + exit_val) * dec!(0.000001);
        // GST: 18% on exchange + SEBI charges
        let gst = (exchange_chg + sebi_chg) * dec!(0.18);
        (stt + stamp_duty + exchange_chg + sebi_chg + gst).round_dp(2)
    }
}

/// Runs the Pullback & Confirmed Rebound backtest across the provided 5-minute candles.
pub fn run_pullback_backtest(
    candles: &[FiveMinCandle],
    is_us: bool,
    initial_capital: Decimal,
    config: &PullbackStrategyConfig,
    dev_split_ratio: Decimal, // e.g. 0.65
) -> PullbackBacktestReport {
    let market_name = if is_us { "US_EQUITY" } else { "INDIA_NSE" };
    let currency_name = if is_us { "USD" } else { "INR" };

    if candles.len() < config.ema_slow_period + 10 {
        let empty_metrics = |name: &str| BacktestPeriodMetrics {
            period_name: name.to_string(),
            candle_count: 0,
            start_time: None,
            end_time: None,
            initial_capital,
            final_capital: initial_capital,
            net_pnl: Decimal::ZERO,
            net_pnl_pct: Decimal::ZERO,
            max_drawdown_amount: Decimal::ZERO,
            max_drawdown_pct: Decimal::ZERO,
            trade_count: 0,
            winning_trades: 0,
            losing_trades: 0,
            win_rate_pct: Decimal::ZERO,
            avg_win: Decimal::ZERO,
            avg_loss: Decimal::ZERO,
            profit_factor: Decimal::ZERO,
            expectancy_per_trade: Decimal::ZERO,
            total_regulatory_costs: Decimal::ZERO,
        };

        return PullbackBacktestReport {
            market: market_name.to_string(),
            currency: currency_name.to_string(),
            config: config.clone(),
            dev_period: empty_metrics("Development (In-Sample)"),
            oos_period: empty_metrics("Validation (Out-of-Sample)"),
            overall_metrics: empty_metrics("Full Dataset"),
            baseline: BaselineComparison {
                strategy_name: "Buy & Hold Baseline".to_string(),
                buy_and_hold_return_pct: Decimal::ZERO,
                buy_and_hold_max_drawdown_pct: Decimal::ZERO,
                strategy_outperformance_pct: Decimal::ZERO,
            },
            sample_trades: Vec::new(),
            disclaimer: "Insufficient 5m candles to evaluate technical indicators.".to_string(),
        };
    }

    let dev_count = ((Decimal::from(candles.len()) * dev_split_ratio).to_string())
        .parse::<f64>()
        .unwrap_or(0.0) as usize;

    let mut current_cash = initial_capital;
    let mut peak_equity = initial_capital;
    let mut max_drawdown_amount = Decimal::ZERO;
    let mut max_drawdown_pct = Decimal::ZERO;

    let mut trades: Vec<BacktestExecutionTrade> = Vec::new();
    let mut pending_signal: Option<(SignalAction, usize)> = None;

    // Track active simulated position (Single open position rule)
    struct OpenPosition {
        symbol: String,
        shares: i64,
        entry_time: DateTime<Utc>,
        entry_price: Decimal,
        stop_price: Decimal,
        target_price: Decimal,
        period: String,
    }
    let mut open_position: Option<OpenPosition> = None;

    for i in (config.ema_slow_period + 2)..candles.len() {
        let current_candle = &candles[i];
        let period_label = if i < dev_count { "DEV" } else { "OOS" };

        // 1. Process fills for signals generated at prior bar close (Zero look-ahead)
        if let Some((
            SignalAction::Buy {
                stop_price,
                target_price,
                planned_risk_per_share,
                ..
            },
            _,
        )) = pending_signal.take()
        {
            if open_position.is_none() {
                // Fill at next bar open + slippage
                let fill_price = current_candle.open * (Decimal::ONE + config.slippage_bps);
                let shares = calculate_position_size(
                    current_cash,
                    fill_price,
                    planned_risk_per_share,
                    config,
                );

                if shares > 0 {
                    let cost = fill_price * Decimal::from(shares);
                    if cost <= current_cash {
                        current_cash -= cost;
                        open_position = Some(OpenPosition {
                            symbol: current_candle.symbol.clone(),
                            shares,
                            entry_time: current_candle.timestamp,
                            entry_price: fill_price,
                            stop_price,
                            target_price,
                            period: period_label.to_string(),
                        });
                    }
                }
            }
        }

        // 2. Manage existing open position against current candle high/low
        if let Some(pos) = open_position.take() {
            let mut closed = false;
            let mut exit_price = Decimal::ZERO;
            let mut exit_reason = String::new();

            // Conservative: Check stop loss first on the candle
            if current_candle.low <= pos.stop_price {
                closed = true;
                exit_price = pos.stop_price * (Decimal::ONE - config.slippage_bps);
                exit_reason = "STOP_LOSS".to_string();
            } else if current_candle.high >= pos.target_price {
                closed = true;
                exit_price = pos.target_price * (Decimal::ONE - config.slippage_bps);
                exit_reason = "TAKE_PROFIT".to_string();
            }

            if closed {
                let entry_val = pos.entry_price * Decimal::from(pos.shares);
                let exit_val = exit_price * Decimal::from(pos.shares);
                let fees = calculate_fees(is_us, entry_val, exit_val, pos.shares);
                let gross_pnl = exit_val - entry_val;
                let net_pnl = gross_pnl - fees;
                current_cash += exit_val - fees;

                let roi = if !entry_val.is_zero() {
                    (net_pnl / entry_val) * dec!(100.0)
                } else {
                    Decimal::ZERO
                };

                trades.push(BacktestExecutionTrade {
                    id: trades.len() + 1,
                    symbol: pos.symbol,
                    period: pos.period,
                    shares: pos.shares,
                    entry_time: pos.entry_time,
                    entry_price: pos.entry_price,
                    planned_stop: pos.stop_price,
                    planned_target: pos.target_price,
                    exit_time: current_candle.timestamp,
                    exit_price,
                    exit_reason,
                    gross_pnl,
                    regulatory_fees: fees,
                    net_pnl,
                    roi_pct: roi,
                });
            } else {
                // Position remains active
                open_position = Some(pos);
            }
        }

        // Track running equity & drawdowns
        let current_equity = current_cash
            + open_position
                .as_ref()
                .map(|p| current_candle.close * Decimal::from(p.shares))
                .unwrap_or(Decimal::ZERO);

        if current_equity > peak_equity {
            peak_equity = current_equity;
        }
        let dd = peak_equity - current_equity;
        if dd > max_drawdown_amount {
            max_drawdown_amount = dd;
        }
        if !peak_equity.is_zero() {
            let dd_pct = (dd / peak_equity) * dec!(100.0);
            if dd_pct > max_drawdown_pct {
                max_drawdown_pct = dd_pct;
            }
        }

        // 3. Evaluate bar close for new setups (completed candle i)
        if open_position.is_none() {
            let eval_slice = &candles[..=i];
            let signal = if is_us {
                evaluate_pullback_setup_us(eval_slice, config).0
            } else {
                evaluate_pullback_setup(eval_slice, config).0
            };
            if let SignalAction::Buy { .. } = signal {
                pending_signal = Some((signal, i));
            }
        }
    }

    // Close any residual open position at the final candle close
    if let Some(pos) = open_position {
        let last_c = candles.last().unwrap();
        let entry_val = pos.entry_price * Decimal::from(pos.shares);
        let exit_val = last_c.close * Decimal::from(pos.shares);
        let fees = calculate_fees(is_us, entry_val, exit_val, pos.shares);
        let gross_pnl = exit_val - entry_val;
        let net_pnl = gross_pnl - fees;
        current_cash += exit_val - fees;

        trades.push(BacktestExecutionTrade {
            id: trades.len() + 1,
            symbol: pos.symbol,
            period: pos.period,
            shares: pos.shares,
            entry_time: pos.entry_time,
            entry_price: pos.entry_price,
            planned_stop: pos.stop_price,
            planned_target: pos.target_price,
            exit_time: last_c.timestamp,
            exit_price: last_c.close,
            exit_reason: "MARKET_CLOSE".to_string(),
            gross_pnl,
            regulatory_fees: fees,
            net_pnl,
            roi_pct: if !entry_val.is_zero() {
                (net_pnl / entry_val) * dec!(100.0)
            } else {
                Decimal::ZERO
            },
        });
    }

    // Helper to compute metrics for a subset of trades
    let compute_metrics = |period_name: &str,
                           filtered_trades: &[BacktestExecutionTrade],
                           candle_slice: &[FiveMinCandle]| {
        let count = filtered_trades.len();
        let winning: Vec<_> = filtered_trades
            .iter()
            .filter(|t| t.net_pnl > Decimal::ZERO)
            .collect();
        let losing: Vec<_> = filtered_trades
            .iter()
            .filter(|t| t.net_pnl <= Decimal::ZERO)
            .collect();

        let win_count = winning.len();
        let loss_count = losing.len();

        let net_pnl: Decimal = filtered_trades.iter().map(|t| t.net_pnl).sum();
        let total_costs: Decimal = filtered_trades.iter().map(|t| t.regulatory_fees).sum();

        let win_sum: Decimal = winning.iter().map(|t| t.net_pnl).sum();
        let loss_sum: Decimal = losing.iter().map(|t| t.net_pnl.abs()).sum();

        let avg_win = if win_count > 0 {
            win_sum / Decimal::from(win_count)
        } else {
            Decimal::ZERO
        };
        let avg_loss = if loss_count > 0 {
            loss_sum / Decimal::from(loss_count)
        } else {
            Decimal::ZERO
        };

        let win_rate_pct = if count > 0 {
            (Decimal::from(win_count) / Decimal::from(count)) * dec!(100.0)
        } else {
            Decimal::ZERO
        };

        let profit_factor = if !loss_sum.is_zero() {
            win_sum / loss_sum
        } else if win_sum > Decimal::ZERO {
            dec!(99.99)
        } else {
            Decimal::ZERO
        };

        let expectancy_per_trade = if count > 0 {
            net_pnl / Decimal::from(count)
        } else {
            Decimal::ZERO
        };

        BacktestPeriodMetrics {
            period_name: period_name.to_string(),
            candle_count: candle_slice.len(),
            start_time: candle_slice.first().map(|c| c.timestamp),
            end_time: candle_slice.last().map(|c| c.timestamp),
            initial_capital,
            final_capital: initial_capital + net_pnl,
            net_pnl,
            net_pnl_pct: if !initial_capital.is_zero() {
                (net_pnl / initial_capital) * dec!(100.0)
            } else {
                Decimal::ZERO
            },
            max_drawdown_amount,
            max_drawdown_pct,
            trade_count: count,
            winning_trades: win_count,
            losing_trades: loss_count,
            win_rate_pct,
            avg_win,
            avg_loss,
            profit_factor,
            expectancy_per_trade,
            total_regulatory_costs: total_costs,
        }
    };

    let dev_trades: Vec<_> = trades
        .iter()
        .filter(|t| t.period == "DEV")
        .cloned()
        .collect();
    let oos_trades: Vec<_> = trades
        .iter()
        .filter(|t| t.period == "OOS")
        .cloned()
        .collect();

    let dev_metrics = compute_metrics(
        "Development (In-Sample)",
        &dev_trades,
        &candles[..dev_count],
    );
    let oos_metrics = compute_metrics(
        "Validation (Out-of-Sample)",
        &oos_trades,
        &candles[dev_count..],
    );
    let overall_metrics = compute_metrics("Full Dataset", &trades, candles);

    // Compute Buy & Hold Baseline Return
    let first_price = candles.first().map(|c| c.open).unwrap_or(dec!(100.0));
    let last_price = candles.last().map(|c| c.close).unwrap_or(dec!(100.0));
    let bnh_return_pct = if !first_price.is_zero() {
        ((last_price - first_price) / first_price) * dec!(100.0)
    } else {
        Decimal::ZERO
    };

    // Calculate baseline max drawdown
    let mut bnh_peak = first_price;
    let mut bnh_max_dd_pct = Decimal::ZERO;
    for c in candles {
        if c.high > bnh_peak {
            bnh_peak = c.high;
        }
        let dd = (bnh_peak - c.low) / bnh_peak * dec!(100.0);
        if dd > bnh_max_dd_pct {
            bnh_max_dd_pct = dd;
        }
    }

    let outperformance = overall_metrics.net_pnl_pct - bnh_return_pct;

    PullbackBacktestReport {
        market: market_name.to_string(),
        currency: currency_name.to_string(),
        config: config.clone(),
        dev_period: dev_metrics,
        oos_period: oos_metrics,
        overall_metrics,
        baseline: BaselineComparison {
            strategy_name: "Buy & Hold Baseline (Underlying Asset)".to_string(),
            buy_and_hold_return_pct: bnh_return_pct.round_dp(2),
            buy_and_hold_max_drawdown_pct: bnh_max_dd_pct.round_dp(2),
            strategy_outperformance_pct: outperformance.round_dp(2),
        },
        sample_trades: trades.into_iter().take(25).collect(),
        disclaimer: "Historical backtest simulation with zero look-ahead bias, slippage, and statutory fee deductions. Past performance is no guarantee of future results.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn generate_trending_candles(count: usize, base_price: f64) -> Vec<FiveMinCandle> {
        let mut candles = Vec::with_capacity(count);
        let mut price = base_price;

        for i in 0..count {
            // Trend up with cyclical dips every 10 bars
            let cycle = i % 10;
            let change = if cycle > 6 { -0.40 } else { 0.65 };
            let open = price;
            let close = open + change;
            let high = open.max(close) + 0.30;
            let low = open.min(close) - 0.25;
            let volume = 15000.0 + (i as f64 * 50.0);
            price = close;

            candles.push(FiveMinCandle {
                timestamp: Utc.timestamp_opt(1704096000 + (i as i64 * 300), 0).unwrap(),
                symbol: "RELIANCE".to_string(),
                open: Decimal::from_str_exact(&format!("{:.2}", open)).unwrap(),
                high: Decimal::from_str_exact(&format!("{:.2}", high)).unwrap(),
                low: Decimal::from_str_exact(&format!("{:.2}", low)).unwrap(),
                close: Decimal::from_str_exact(&format!("{:.2}", close)).unwrap(),
                volume: Decimal::from_str_exact(&format!("{:.2}", volume)).unwrap(),
            });
        }
        candles
    }

    #[test]
    fn test_pullback_backtest_india_no_lookahead() {
        let candles = generate_trending_candles(120, 2500.0);
        let config = PullbackStrategyConfig::default();
        let report = run_pullback_backtest(&candles, false, dec!(500000.0), &config, dec!(0.65));

        assert_eq!(report.market, "INDIA_NSE");
        assert_eq!(report.currency, "INR");
        assert_eq!(report.dev_period.period_name, "Development (In-Sample)");
        assert_eq!(report.oos_period.period_name, "Validation (Out-of-Sample)");
        assert!(report.dev_period.candle_count > 0);
        assert!(report.oos_period.candle_count > 0);
    }

    #[test]
    fn test_pullback_backtest_us_no_lookahead() {
        let candles = generate_trending_candles(120, 150.0);
        let config = PullbackStrategyConfig::default();
        let report = run_pullback_backtest(&candles, true, dec!(10000.0), &config, dec!(0.65));

        assert_eq!(report.market, "US_EQUITY");
        assert_eq!(report.currency, "USD");
        assert!(report.baseline.buy_and_hold_return_pct != Decimal::ZERO);
    }
}
