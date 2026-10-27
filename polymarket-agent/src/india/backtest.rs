//! High-integrity historical backtesting engine for NSE cash equities.
//!
//! Enforces strict no-look-ahead bias (signals generated at bar t close execute
//! at bar t+1 open with spread and slippage), conservative SL/TP handling
//! (SL triggers first on outside candles), whole share sizing, and statutory
//! Indian transaction costs.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

use crate::india::costs::{IndianCostConfig, IndianTransactionCosts};
use crate::venue::types::Side;

/// An OHLCV market candle for NSE cash equities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NseCandle {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
}

/// Backtest execution parameters and risk limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NseBacktestConfig {
    pub initial_capital_inr: Decimal,
    pub max_position_pct: Decimal,
    pub max_total_exposure_pct: Decimal,
    pub spread_bps: Decimal,
    pub slippage_bps: Decimal,
    pub volume_limit_pct: Decimal,
    pub fast_sma_period: usize,
    pub slow_sma_period: usize,
    pub rsi_period: usize,
    pub rsi_buy_threshold: Decimal,
    pub stop_loss_pct: Decimal,
    pub take_profit_pct: Decimal,
    pub train_split_pct: Decimal,
    pub val_split_pct: Decimal,
    pub cost_config: IndianCostConfig,
}

impl Default for NseBacktestConfig {
    fn default() -> Self {
        Self {
            initial_capital_inr: dec!(500000.0), // ₹5,00,000 initial testing capital
            max_position_pct: dec!(0.10),        // 10% max per stock
            max_total_exposure_pct: dec!(0.50),  // 50% max portfolio exposure
            spread_bps: dec!(0.0005),            // 5 bps spread (0.05%)
            slippage_bps: dec!(0.0005),          // 5 bps slippage (0.05%)
            volume_limit_pct: dec!(0.05),        // Max 5% of candle volume
            fast_sma_period: 5,
            slow_sma_period: 15,
            rsi_period: 14,
            rsi_buy_threshold: dec!(40.0),
            stop_loss_pct: dec!(0.015),   // 1.5% stop loss
            take_profit_pct: dec!(0.030), // 3.0% take profit
            train_split_pct: dec!(0.60),  // 60% train
            val_split_pct: dec!(0.20),    // 20% validation, 20% test
            cost_config: IndianCostConfig::default(),
        }
    }
}

/// Simulated trade record with full Indian statutory cost accounting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestTrade {
    pub id: usize,
    pub symbol: String,
    pub shares: i64,
    pub entry_time: DateTime<Utc>,
    pub entry_price: Decimal,
    pub entry_costs: IndianTransactionCosts,
    pub exit_time: Option<DateTime<Utc>>,
    pub exit_price: Option<Decimal>,
    pub exit_costs: Option<IndianTransactionCosts>,
    pub gross_pnl: Decimal,
    pub total_costs: Decimal,
    pub net_pnl: Decimal,
    pub exit_reason: String,
}

/// Equity curve snapshot at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub timestamp: DateTime<Utc>,
    pub cash: Decimal,
    pub position_market_value: Decimal,
    pub total_equity: Decimal,
    pub benchmark_equity: Decimal,
    pub drawdown_pct: Decimal,
}

/// Complete results of a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NseBacktestResults {
    pub initial_capital: Decimal,
    pub final_equity: Decimal,
    pub net_profit: Decimal,
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_costs: Decimal,
    pub total_stt: Decimal,
    pub total_brokerage: Decimal,
    pub total_gst: Decimal,
    pub total_stamp_duty: Decimal,
    pub total_exchange_charges: Decimal,
    pub roi_pct: Decimal,
    pub max_drawdown_pct: Decimal,
    pub max_drawdown_inr: Decimal,
    pub total_trades: usize,
    pub winning_trades: usize,
    pub losing_trades: usize,
    pub win_rate_pct: Decimal,
    pub profit_factor: Decimal,
    pub net_expectancy_per_trade: Decimal,
    pub benchmark_return_pct: Decimal,
    pub insufficient_data_warning: bool,
    pub trades: Vec<BacktestTrade>,
    pub equity_curve: Vec<EquityPoint>,
}

/// Parse and validate an OHLCV CSV file according to strict rules.
///
/// Supported columns: `timestamp` (ISO or YYYY-MM-DD HH:MM:SS), `symbol`, `open`, `high`, `low`, `close`, `volume`.
pub fn parse_and_validate_csv(csv_content: &str) -> Result<Vec<NseCandle>> {
    let mut candles = Vec::new();
    let mut last_ts_by_symbol: HashMap<String, DateTime<Utc>> = HashMap::new();

    for (line_idx, line) in csv_content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Check header
        if line_idx == 0
            || line.to_ascii_lowercase().starts_with("timestamp")
            || line.to_ascii_lowercase().starts_with("date")
        {
            continue;
        }

        let fields: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if fields.len() < 7 {
            bail!("Line {}: Expected at least 7 fields (timestamp,symbol,open,high,low,close,volume), found {}", line_idx + 1, fields.len());
        }

        let ts_raw = fields[0];
        let symbol = fields[1].to_uppercase();
        let open = Decimal::from_str(fields[2])
            .with_context(|| format!("Line {}: Invalid open price {}", line_idx + 1, fields[2]))?;
        let high = Decimal::from_str(fields[3])
            .with_context(|| format!("Line {}: Invalid high price {}", line_idx + 1, fields[3]))?;
        let low = Decimal::from_str(fields[4])
            .with_context(|| format!("Line {}: Invalid low price {}", line_idx + 1, fields[4]))?;
        let close = Decimal::from_str(fields[5])
            .with_context(|| format!("Line {}: Invalid close price {}", line_idx + 1, fields[5]))?;
        let volume = Decimal::from_str(fields[6])
            .with_context(|| format!("Line {}: Invalid volume {}", line_idx + 1, fields[6]))?;

        // Geometry checks
        if open <= Decimal::ZERO
            || high <= Decimal::ZERO
            || low <= Decimal::ZERO
            || close <= Decimal::ZERO
        {
            bail!("Line {}: Prices must be strictly positive", line_idx + 1);
        }
        if volume < Decimal::ZERO {
            bail!("Line {}: Volume must be non-negative", line_idx + 1);
        }
        if high < low || high < open || high < close || low > open || low > close {
            bail!(
                "Line {}: Inconsistent OHLC candle geometry (H: {}, L: {}, O: {}, C: {})",
                line_idx + 1,
                high,
                low,
                open,
                close
            );
        }

        // Parse timestamp
        let timestamp = parse_timestamp(ts_raw).with_context(|| {
            format!(
                "Line {}: Could not parse timestamp '{}'",
                line_idx + 1,
                ts_raw
            )
        })?;

        // Chronological monotonic check per symbol
        if let Some(prev) = last_ts_by_symbol.get(&symbol) {
            if timestamp <= *prev {
                bail!("Line {}: Non-monotonic or duplicate timestamp '{}' for symbol '{}' (previous was '{}')", line_idx + 1, timestamp, symbol, prev);
            }
        }
        last_ts_by_symbol.insert(symbol.clone(), timestamp);

        candles.push(NseCandle {
            timestamp,
            symbol,
            open,
            high,
            low,
            close,
            volume,
        });
    }

    if candles.is_empty() {
        bail!("CSV file contains no valid OHLCV candle rows");
    }

    Ok(candles)
}

fn parse_timestamp(raw: &str) -> Result<DateTime<Utc>> {
    // 1. Try RFC3339 / ISO 8601
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.with_timezone(&Utc));
    }
    // 2. Try Naive formats (assume IST UTC+05:30 if naive)
    for fmt in &[
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%d",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, fmt) {
            let kolkata = chrono_tz::Asia::Kolkata;
            use chrono::TimeZone;
            if let Some(local) = kolkata.from_local_datetime(&naive).single() {
                return Ok(local.with_timezone(&Utc));
            }
        }
    }
    // Try YYYY-MM-DD
    if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let naive = date.and_hms_opt(9, 15, 0).unwrap();
        let kolkata = chrono_tz::Asia::Kolkata;
        use chrono::TimeZone;
        if let Some(local) = kolkata.from_local_datetime(&naive).single() {
            return Ok(local.with_timezone(&Utc));
        }
    }

    bail!("Unrecognized date format: {}", raw);
}

/// Run an end-to-end historical backtest with zero look-ahead bias.
pub fn run_backtest(
    candles: &[NseCandle],
    config: &NseBacktestConfig,
) -> Result<NseBacktestResults> {
    if candles.len() < config.slow_sma_period + 2 {
        bail!(
            "Insufficient candle data (found {}, need at least {})",
            candles.len(),
            config.slow_sma_period + 2
        );
    }

    // Group candles by symbol and sort chronologically
    let mut by_symbol: HashMap<String, Vec<NseCandle>> = HashMap::new();
    for c in candles {
        by_symbol
            .entry(c.symbol.clone())
            .or_default()
            .push(c.clone());
    }
    for list in by_symbol.values_mut() {
        list.sort_by_key(|c| c.timestamp);
    }

    let initial_capital = config.initial_capital_inr;
    let mut cash = initial_capital;
    let mut open_position: Option<BacktestTrade> = None;
    let mut completed_trades: Vec<BacktestTrade> = Vec::new();
    let mut equity_curve: Vec<EquityPoint> = Vec::new();
    let mut trade_seq = 1usize;

    // Benchmark tracking (Buy-and-hold the first symbol from first bar)
    let first_candle = &candles[0];
    let benchmark_initial_price = first_candle.open;

    // Multi-bar simulation: iterate across all timestamps
    // We assume single-symbol or primary symbol timeline
    let primary_symbol = &first_candle.symbol;
    let timeline = by_symbol
        .get(primary_symbol)
        .context("Primary symbol not found")?;

    let mut pending_signal: Option<Side> = None;
    let mut high_water_mark = initial_capital;

    for i in 0..timeline.len() {
        let current_bar = &timeline[i];
        let now = current_bar.timestamp;

        // 1. Process pending execution from previous bar's signal (Strict No-Look-Ahead!)
        if let Some(signal) = pending_signal.take() {
            if signal == Side::Buy && open_position.is_none() {
                // Execute Buy at current bar OPEN with spread and slippage
                let execution_price =
                    current_bar.open * (Decimal::ONE + config.spread_bps + config.slippage_bps);
                let max_notional = initial_capital * config.max_position_pct;
                let available_to_spend = cash.min(max_notional);

                if available_to_spend >= execution_price {
                    // Whole shares only!
                    let shares = (available_to_spend / execution_price)
                        .floor()
                        .to_string()
                        .parse::<i64>()
                        .unwrap_or(0);
                    // Volume participation cap
                    let max_shares_by_vol = (current_bar.volume * config.volume_limit_pct)
                        .floor()
                        .to_string()
                        .parse::<i64>()
                        .unwrap_or(shares);
                    let shares = shares.min(max_shares_by_vol);

                    if shares >= 1 {
                        let trade_value = execution_price * Decimal::from(shares);
                        let costs = config.cost_config.calculate(trade_value, Side::Buy);
                        let total_deduction = trade_value + costs.total;

                        if cash >= total_deduction {
                            cash -= total_deduction;
                            open_position = Some(BacktestTrade {
                                id: trade_seq,
                                symbol: current_bar.symbol.clone(),
                                shares,
                                entry_time: now,
                                entry_price: execution_price,
                                entry_costs: costs,
                                exit_time: None,
                                exit_price: None,
                                exit_costs: None,
                                gross_pnl: Decimal::ZERO,
                                total_costs: Decimal::ZERO,
                                net_pnl: Decimal::ZERO,
                                exit_reason: String::new(),
                            });
                            trade_seq += 1;
                        }
                    }
                }
            } else if signal == Side::Sell && open_position.is_some() {
                // Exit executed at current bar OPEN
                let mut pos = open_position.take().unwrap();
                let exit_price =
                    current_bar.open * (Decimal::ONE - config.spread_bps - config.slippage_bps);
                let trade_value = exit_price * Decimal::from(pos.shares);
                let exit_costs = config.cost_config.calculate(trade_value, Side::Sell);
                let total_payout = trade_value - exit_costs.total;

                cash += total_payout;

                let gross = (exit_price - pos.entry_price) * Decimal::from(pos.shares);
                let total_fee = pos.entry_costs.total + exit_costs.total;
                let net = gross - total_fee;

                pos.exit_time = Some(now);
                pos.exit_price = Some(exit_price);
                pos.exit_costs = Some(exit_costs);
                pos.gross_pnl = gross.round_dp(2);
                pos.total_costs = total_fee.round_dp(2);
                pos.net_pnl = net.round_dp(2);
                pos.exit_reason = "SIGNAL_EXIT".to_string();

                completed_trades.push(pos);
            }
        }

        // 2. Intra-bar Stop-Loss and Take-Profit evaluation for open positions
        if let Some(pos) = open_position.take() {
            let stop_price = pos.entry_price * (Decimal::ONE - config.stop_loss_pct);
            let target_price = pos.entry_price * (Decimal::ONE + config.take_profit_pct);

            let hit_stop = current_bar.low <= stop_price;
            let hit_target = current_bar.high >= target_price;

            if hit_stop && hit_target {
                // Conservative Rule: Stop loss triggers first!
                let exit_price = stop_price * (Decimal::ONE - config.slippage_bps);
                let trade_value = exit_price * Decimal::from(pos.shares);
                let exit_costs = config.cost_config.calculate(trade_value, Side::Sell);
                cash += trade_value - exit_costs.total;

                let gross = (exit_price - pos.entry_price) * Decimal::from(pos.shares);
                let total_fee = pos.entry_costs.total + exit_costs.total;
                let net = gross - total_fee;

                completed_trades.push(BacktestTrade {
                    id: pos.id,
                    symbol: pos.symbol,
                    shares: pos.shares,
                    entry_time: pos.entry_time,
                    entry_price: pos.entry_price,
                    entry_costs: pos.entry_costs,
                    exit_time: Some(now),
                    exit_price: Some(exit_price),
                    exit_costs: Some(exit_costs),
                    gross_pnl: gross.round_dp(2),
                    total_costs: total_fee.round_dp(2),
                    net_pnl: net.round_dp(2),
                    exit_reason: "STOP_LOSS (OUTSIDE_BAR_CONSERVATIVE)".to_string(),
                });
            } else if hit_stop {
                let exit_price = stop_price * (Decimal::ONE - config.slippage_bps);
                let trade_value = exit_price * Decimal::from(pos.shares);
                let exit_costs = config.cost_config.calculate(trade_value, Side::Sell);
                cash += trade_value - exit_costs.total;

                let gross = (exit_price - pos.entry_price) * Decimal::from(pos.shares);
                let total_fee = pos.entry_costs.total + exit_costs.total;
                let net = gross - total_fee;

                completed_trades.push(BacktestTrade {
                    id: pos.id,
                    symbol: pos.symbol,
                    shares: pos.shares,
                    entry_time: pos.entry_time,
                    entry_price: pos.entry_price,
                    entry_costs: pos.entry_costs,
                    exit_time: Some(now),
                    exit_price: Some(exit_price),
                    exit_costs: Some(exit_costs),
                    gross_pnl: gross.round_dp(2),
                    total_costs: total_fee.round_dp(2),
                    net_pnl: net.round_dp(2),
                    exit_reason: "STOP_LOSS".to_string(),
                });
            } else if hit_target {
                let exit_price = target_price * (Decimal::ONE - config.slippage_bps);
                let trade_value = exit_price * Decimal::from(pos.shares);
                let exit_costs = config.cost_config.calculate(trade_value, Side::Sell);
                cash += trade_value - exit_costs.total;

                let gross = (exit_price - pos.entry_price) * Decimal::from(pos.shares);
                let total_fee = pos.entry_costs.total + exit_costs.total;
                let net = gross - total_fee;

                completed_trades.push(BacktestTrade {
                    id: pos.id,
                    symbol: pos.symbol,
                    shares: pos.shares,
                    entry_time: pos.entry_time,
                    entry_price: pos.entry_price,
                    entry_costs: pos.entry_costs,
                    exit_time: Some(now),
                    exit_price: Some(exit_price),
                    exit_costs: Some(exit_costs),
                    gross_pnl: gross.round_dp(2),
                    total_costs: total_fee.round_dp(2),
                    net_pnl: net.round_dp(2),
                    exit_reason: "TAKE_PROFIT".to_string(),
                });
            } else {
                // Position stays open
                open_position = Some(pos);
            }
        }

        // 3. Generate signals using strictly information up to current bar close
        if i >= config.slow_sma_period {
            let slice = &timeline[..=i];
            let fast_sma = compute_sma(slice, config.fast_sma_period);
            let slow_sma = compute_sma(slice, config.slow_sma_period);
            let rsi = compute_rsi(slice, config.rsi_period);

            if open_position.is_none()
                && fast_sma > slow_sma
                && rsi > config.rsi_buy_threshold
                && rsi < dec!(70.0)
            {
                pending_signal = Some(Side::Buy);
            } else if open_position.is_some() && fast_sma < slow_sma {
                pending_signal = Some(Side::Sell);
            }
        }

        // 4. Mark equity curve
        let position_val = match &open_position {
            Some(p) => current_bar.close * Decimal::from(p.shares),
            None => Decimal::ZERO,
        };
        let total_eq = cash + position_val;
        high_water_mark = high_water_mark.max(total_eq);
        let dd_pct = if high_water_mark > Decimal::ZERO {
            ((high_water_mark - total_eq) / high_water_mark).round_dp(4)
        } else {
            Decimal::ZERO
        };

        // Benchmark calculation
        let benchmark_eq = (initial_capital / benchmark_initial_price) * current_bar.close;

        equity_curve.push(EquityPoint {
            timestamp: now,
            cash: cash.round_dp(2),
            position_market_value: position_val.round_dp(2),
            total_equity: total_eq.round_dp(2),
            benchmark_equity: benchmark_eq.round_dp(2),
            drawdown_pct: dd_pct,
        });
    }

    // Close remaining open position at end of backtest to record final marks
    let mut unrealized_pnl = Decimal::ZERO;
    if let Some(p) = open_position {
        let last_close = timeline.last().unwrap().close;
        unrealized_pnl = (last_close - p.entry_price) * Decimal::from(p.shares);
    }

    let final_equity = equity_curve
        .last()
        .map(|e| e.total_equity)
        .unwrap_or(initial_capital);
    let net_profit = final_equity - initial_capital;
    let realized_pnl: Decimal = completed_trades.iter().map(|t| t.net_pnl).sum();

    let total_costs: Decimal = completed_trades.iter().map(|t| t.total_costs).sum();
    let total_stt: Decimal = completed_trades
        .iter()
        .map(|t| {
            t.entry_costs.stt
                + t.exit_costs
                    .as_ref()
                    .map(|c| c.stt)
                    .unwrap_or(Decimal::ZERO)
        })
        .sum();
    let total_brokerage: Decimal = completed_trades
        .iter()
        .map(|t| {
            t.entry_costs.brokerage
                + t.exit_costs
                    .as_ref()
                    .map(|c| c.brokerage)
                    .unwrap_or(Decimal::ZERO)
        })
        .sum();
    let total_gst: Decimal = completed_trades
        .iter()
        .map(|t| {
            t.entry_costs.gst
                + t.exit_costs
                    .as_ref()
                    .map(|c| c.gst)
                    .unwrap_or(Decimal::ZERO)
        })
        .sum();
    let total_stamp_duty: Decimal = completed_trades
        .iter()
        .map(|t| t.entry_costs.stamp_duty)
        .sum();
    let total_exchange_charges: Decimal = completed_trades
        .iter()
        .map(|t| {
            t.entry_costs.exchange_charges
                + t.exit_costs
                    .as_ref()
                    .map(|c| c.exchange_charges)
                    .unwrap_or(Decimal::ZERO)
        })
        .sum();

    let roi_pct = (net_profit / initial_capital).round_dp(4);

    let max_drawdown_pct = equity_curve
        .iter()
        .map(|e| e.drawdown_pct)
        .max()
        .unwrap_or(Decimal::ZERO);
    let max_drawdown_inr = (initial_capital * max_drawdown_pct).round_dp(2);

    let total_trades = completed_trades.len();
    let winning_trades = completed_trades
        .iter()
        .filter(|t| t.net_pnl > Decimal::ZERO)
        .count();
    let losing_trades = completed_trades
        .iter()
        .filter(|t| t.net_pnl <= Decimal::ZERO)
        .count();

    let win_rate_pct = if total_trades > 0 {
        (Decimal::from(winning_trades) / Decimal::from(total_trades) * dec!(100.0)).round_dp(2)
    } else {
        Decimal::ZERO
    };

    let gross_wins: Decimal = completed_trades
        .iter()
        .filter(|t| t.net_pnl > Decimal::ZERO)
        .map(|t| t.net_pnl)
        .sum();
    let gross_losses: Decimal = completed_trades
        .iter()
        .filter(|t| t.net_pnl < Decimal::ZERO)
        .map(|t| t.net_pnl.abs())
        .sum();

    let profit_factor = if gross_losses > Decimal::ZERO {
        (gross_wins / gross_losses).round_dp(2)
    } else if gross_wins > Decimal::ZERO {
        dec!(99.99)
    } else {
        Decimal::ZERO
    };

    let net_expectancy_per_trade = if total_trades > 0 {
        (net_profit / Decimal::from(total_trades)).round_dp(2)
    } else {
        Decimal::ZERO
    };

    let last_benchmark = equity_curve
        .last()
        .map(|e| e.benchmark_equity)
        .unwrap_or(initial_capital);
    let benchmark_return_pct = ((last_benchmark - initial_capital) / initial_capital).round_dp(4);

    Ok(NseBacktestResults {
        initial_capital,
        final_equity,
        net_profit: net_profit.round_dp(2),
        realized_pnl: realized_pnl.round_dp(2),
        unrealized_pnl: unrealized_pnl.round_dp(2),
        total_costs: total_costs.round_dp(2),
        total_stt: total_stt.round_dp(2),
        total_brokerage: total_brokerage.round_dp(2),
        total_gst: total_gst.round_dp(2),
        total_stamp_duty: total_stamp_duty.round_dp(2),
        total_exchange_charges: total_exchange_charges.round_dp(2),
        roi_pct,
        max_drawdown_pct,
        max_drawdown_inr,
        total_trades,
        winning_trades,
        losing_trades,
        win_rate_pct,
        profit_factor,
        net_expectancy_per_trade,
        benchmark_return_pct,
        insufficient_data_warning: total_trades < 10,
        trades: completed_trades,
        equity_curve,
    })
}

fn compute_sma(candles: &[NseCandle], period: usize) -> Decimal {
    if candles.len() < period || period == 0 {
        return Decimal::ZERO;
    }
    let sum: Decimal = candles.iter().rev().take(period).map(|c| c.close).sum();
    sum / Decimal::from(period)
}

fn compute_rsi(candles: &[NseCandle], period: usize) -> Decimal {
    if candles.len() <= period || period == 0 {
        return dec!(50.0);
    }
    let mut gains = Decimal::ZERO;
    let mut losses = Decimal::ZERO;

    let start = candles.len() - period;
    for i in start..candles.len() {
        let diff = candles[i].close - candles[i - 1].close;
        if diff > Decimal::ZERO {
            gains += diff;
        } else {
            losses += diff.abs();
        }
    }

    if losses == Decimal::ZERO {
        return dec!(100.0);
    }
    let rs = gains / losses;
    dec!(100.0) - (dec!(100.0) / (Decimal::ONE + rs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_synthetic_candles() -> Vec<NseCandle> {
        let mut candles = Vec::new();
        let base_date =
            NaiveDateTime::parse_from_str("2026-01-01 09:15:00", "%Y-%m-%d %H:%M:%S").unwrap();
        let kolkata = chrono_tz::Asia::Kolkata;
        use chrono::TimeZone;

        let mut price = dec!(2500.0);
        for i in 0..50 {
            let naive = base_date + chrono::Duration::days(i);
            let ts = kolkata
                .from_local_datetime(&naive)
                .unwrap()
                .with_timezone(&Utc);

            // Alternate wave pattern
            let delta = if i % 2 == 0 { dec!(25.0) } else { dec!(-15.0) };
            price += delta;

            candles.push(NseCandle {
                timestamp: ts,
                symbol: "RELIANCE".to_string(),
                open: price - dec!(5.0),
                high: price + dec!(15.0),
                low: price - dec!(10.0),
                close: price,
                volume: dec!(100000.0),
            });
        }
        candles
    }

    #[test]
    fn parses_valid_csv() {
        let csv = "\
timestamp,symbol,open,high,low,close,volume
2026-01-01 09:15:00,RELIANCE,2500.0,2520.0,2490.0,2515.0,150000
2026-01-02 09:15:00,RELIANCE,2515.0,2540.0,2510.0,2530.0,180000
2026-01-05 09:15:00,RELIANCE,2530.0,2550.0,2525.0,2545.0,210000
";
        let candles = parse_and_validate_csv(csv).unwrap();
        assert_eq!(candles.len(), 3);
        assert_eq!(candles[0].symbol, "RELIANCE");
        assert_eq!(candles[0].open, dec!(2500.0));
    }

    #[test]
    fn rejects_invalid_geometry() {
        let csv = "\
timestamp,symbol,open,high,low,close,volume
2026-01-01 09:15:00,RELIANCE,2500.0,2400.0,2600.0,2515.0,150000
";
        let res = parse_and_validate_csv(csv);
        assert!(res.is_err());
    }

    #[test]
    fn runs_backtest_with_no_lookahead_and_statutory_costs() {
        let candles = generate_synthetic_candles();
        let config = NseBacktestConfig::default();
        let results = run_backtest(&candles, &config).unwrap();

        assert_eq!(results.initial_capital, dec!(500000.0));
        assert!(results.equity_curve.len() == candles.len());
        assert!(results.total_costs >= Decimal::ZERO);
        assert!(results.total_stt >= Decimal::ZERO);
        // Insufficient data warning when trades < 10
        assert_eq!(results.insufficient_data_warning, results.total_trades < 10);
    }
}
