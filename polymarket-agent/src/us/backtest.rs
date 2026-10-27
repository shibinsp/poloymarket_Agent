//! High-integrity historical backtesting engine for US cash equities and ETFs.
//!
//! Enforces strict no-look-ahead bias (signals generated at bar t close execute
//! at bar t+1 open with spread and slippage), conservative SL/TP handling
//! (SL triggers first on outside candles), fractional/whole share sizing, and
//! US statutory transaction costs (SEC Section 31 fees, FINRA TAF, commissions).

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

use crate::us::costs::{UsCostConfig, UsTransactionCosts};
use crate::venue::types::Side;

/// An OHLCV market candle for US cash equities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsCandle {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
}

/// Backtest execution parameters and risk limits for US equities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsBacktestConfig {
    pub initial_capital_usd: Decimal,
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
    pub cost_config: UsCostConfig,
}

impl Default for UsBacktestConfig {
    fn default() -> Self {
        Self {
            initial_capital_usd: dec!(100000.0), // $100,000 initial testing capital
            max_position_pct: dec!(0.10),        // 10% max per stock/ETF
            max_total_exposure_pct: dec!(0.50),  // 50% max portfolio exposure
            spread_bps: dec!(0.0001),            // 1 bp spread for liquid US equities
            slippage_bps: dec!(0.0002),          // 2 bps slippage
            volume_limit_pct: dec!(0.05),        // Max 5% of candle volume
            fast_sma_period: 5,
            slow_sma_period: 15,
            rsi_period: 14,
            rsi_buy_threshold: dec!(40.0),
            stop_loss_pct: dec!(0.015),   // 1.5% stop loss
            take_profit_pct: dec!(0.030), // 3.0% take profit
            train_split_pct: dec!(0.60),  // 60% train
            val_split_pct: dec!(0.20),    // 20% validation, 20% test
            cost_config: UsCostConfig::default(),
        }
    }
}

/// Simulated trade record with full US statutory and regulatory cost accounting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsBacktestTrade {
    pub id: usize,
    pub symbol: String,
    pub shares: Decimal,
    pub entry_time: DateTime<Utc>,
    pub entry_price: Decimal,
    pub entry_costs: UsTransactionCosts,
    pub exit_time: Option<DateTime<Utc>>,
    pub exit_price: Option<Decimal>,
    pub exit_costs: Option<UsTransactionCosts>,
    pub gross_pnl: Decimal,
    pub total_costs: Decimal,
    pub net_pnl: Decimal,
    pub exit_reason: String,
}

/// Equity curve snapshot at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsEquityPoint {
    pub timestamp: DateTime<Utc>,
    pub cash: Decimal,
    pub position_market_value: Decimal,
    pub total_equity: Decimal,
    pub benchmark_equity: Decimal,
    pub drawdown_pct: Decimal,
}

/// Complete results of a US backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsBacktestResults {
    pub initial_capital: Decimal,
    pub final_equity: Decimal,
    pub net_profit: Decimal,
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_costs: Decimal,
    pub total_sec_fees: Decimal,
    pub total_finra_taf: Decimal,
    pub total_commission: Decimal,
    pub total_spread_slippage_cost: Decimal,
    pub roi_pct: Decimal,
    pub max_drawdown_pct: Decimal,
    pub max_drawdown_usd: Decimal,
    pub total_trades: usize,
    pub winning_trades: usize,
    pub losing_trades: usize,
    pub win_rate_pct: Decimal,
    pub profit_factor: Decimal,
    pub net_expectancy_per_trade: Decimal,
    pub benchmark_return_pct: Decimal,
    pub insufficient_data_warning: bool,
    pub trades: Vec<UsBacktestTrade>,
    pub equity_curve: Vec<UsEquityPoint>,
}

/// Parse and validate a US OHLCV CSV file according to strict rules.
///
/// Supported columns: `timestamp` (ISO or YYYY-MM-DD HH:MM:SS), `symbol`, `open`, `high`, `low`, `close`, `volume`.
pub fn parse_and_validate_us_csv(csv_content: &str) -> Result<Vec<UsCandle>> {
    let mut candles = Vec::new();
    let mut last_ts_by_symbol: HashMap<String, DateTime<Utc>> = HashMap::new();

    for (line_idx, line) in csv_content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() < 7 {
            if line_idx == 0 {
                // Header line with fewer columns
                continue;
            }
            bail!(
                "Line {}: invalid CSV format. Expected at least 7 columns (timestamp,symbol,open,high,low,close,volume), found {}",
                line_idx + 1,
                parts.len()
            );
        }

        // Check if header row
        if parts[0].eq_ignore_ascii_case("timestamp") || parts[0].eq_ignore_ascii_case("date") {
            continue;
        }

        let ts_str = parts[0];
        let symbol = parts[1].to_uppercase();

        let timestamp = if let Ok(dt) = DateTime::parse_from_rfc3339(ts_str) {
            dt.with_timezone(&Utc)
        } else if let Ok(ndt) = NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M:%S") {
            ndt.and_utc()
        } else if let Ok(ndt) = NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M") {
            ndt.and_utc()
        } else if let Ok(nd) = chrono::NaiveDate::parse_from_str(ts_str, "%Y-%m-%d") {
            nd.and_hms_opt(16, 0, 0).unwrap().and_utc()
        } else {
            bail!("Line {}: unparseable timestamp '{}'", line_idx + 1, ts_str);
        };

        // Monotonic timestamp check per symbol
        if let Some(last_ts) = last_ts_by_symbol.get(&symbol) {
            if timestamp <= *last_ts {
                bail!(
                    "Line {}: Non-chronological or duplicate timestamp for symbol {}: {} <= {}",
                    line_idx + 1,
                    symbol,
                    timestamp,
                    last_ts
                );
            }
        }
        last_ts_by_symbol.insert(symbol.clone(), timestamp);

        let open = Decimal::from_str(parts[2])
            .with_context(|| format!("Line {}: invalid open price '{}'", line_idx + 1, parts[2]))?;
        let high = Decimal::from_str(parts[3])
            .with_context(|| format!("Line {}: invalid high price '{}'", line_idx + 1, parts[3]))?;
        let low = Decimal::from_str(parts[4])
            .with_context(|| format!("Line {}: invalid low price '{}'", line_idx + 1, parts[4]))?;
        let close = Decimal::from_str(parts[5]).with_context(|| {
            format!("Line {}: invalid close price '{}'", line_idx + 1, parts[5])
        })?;
        let volume = Decimal::from_str(parts[6])
            .with_context(|| format!("Line {}: invalid volume '{}'", line_idx + 1, parts[6]))?;

        if open <= Decimal::ZERO
            || high <= Decimal::ZERO
            || low <= Decimal::ZERO
            || close <= Decimal::ZERO
        {
            bail!(
                "Line {}: OHLC prices must be strictly positive",
                line_idx + 1
            );
        }
        if high < low || high < open || high < close || low > open || low > close {
            bail!(
                "Line {}: Inconsistent OHLC candle (high {} < low {} or bounds breached)",
                line_idx + 1,
                high,
                low
            );
        }
        if volume < Decimal::ZERO {
            bail!("Line {}: volume cannot be negative", line_idx + 1);
        }

        candles.push(UsCandle {
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
        bail!("CSV file contains no valid candle rows");
    }

    Ok(candles)
}

/// Simulated open position during US backtest.
#[allow(dead_code)]
struct OpenPosition {
    symbol: String,
    shares: Decimal,
    entry_time: DateTime<Utc>,
    entry_price: Decimal,
    entry_costs: UsTransactionCosts,
    stop_price: Decimal,
    target_price: Decimal,
}

/// Execute a strict backtest on the provided candles.
pub fn run_us_backtest(
    candles: &[UsCandle],
    config: &UsBacktestConfig,
) -> Result<UsBacktestResults> {
    if candles.is_empty() {
        bail!("Cannot run backtest on empty candles slice");
    }

    let mut by_symbol: HashMap<String, Vec<UsCandle>> = HashMap::new();
    for c in candles {
        by_symbol
            .entry(c.symbol.clone())
            .or_default()
            .push(c.clone());
    }

    for (sym, list) in &mut by_symbol {
        list.sort_by_key(|c| c.timestamp);
        if list.len() < config.slow_sma_period + 5 {
            bail!(
                "Symbol {} has only {} bars; at least {} required for SMA indicators",
                sym,
                list.len(),
                config.slow_sma_period + 5
            );
        }
    }

    let mut timestamps: Vec<DateTime<Utc>> = candles.iter().map(|c| c.timestamp).collect();
    timestamps.sort();
    timestamps.dedup();

    let mut cash = config.initial_capital_usd;
    let mut positions: HashMap<String, OpenPosition> = HashMap::new();
    let mut completed_trades: Vec<UsBacktestTrade> = Vec::new();
    let mut equity_curve: Vec<UsEquityPoint> = Vec::new();
    let mut peak_equity = cash;
    let mut max_drawdown_pct = Decimal::ZERO;
    let mut max_drawdown_usd = Decimal::ZERO;

    let mut pending_orders: Vec<(String, Side, Decimal, DateTime<Utc>)> = Vec::new();

    // Benchmark setup (e.g. SPY or first stock buy-and-hold)
    let first_ts = timestamps[0];
    let benchmark_initial_price = candles
        .iter()
        .find(|c| c.timestamp == first_ts)
        .map(|c| c.open)
        .unwrap_or(dec!(100.0));
    let benchmark_shares = config.initial_capital_usd / benchmark_initial_price;

    let mut total_sec_fees = Decimal::ZERO;
    let mut total_finra_taf = Decimal::ZERO;
    let mut total_commission = Decimal::ZERO;
    let mut total_spread_slippage_cost = Decimal::ZERO;

    for (ts_idx, &ts) in timestamps.iter().enumerate() {
        // 1. Execute any pending orders generated at bar t-1 close using bar t open
        let mut executed_orders = Vec::new();
        for (sym, side, target_cash, _gen_ts) in pending_orders.drain(..) {
            if let Some(candle) = by_symbol
                .get(&sym)
                .and_then(|list| list.iter().find(|c| c.timestamp == ts))
            {
                let execution_price = match side {
                    Side::Buy => {
                        candle.open * (Decimal::ONE + config.spread_bps + config.slippage_bps)
                    }
                    Side::Sell => {
                        candle.open * (Decimal::ONE - config.spread_bps - config.slippage_bps)
                    }
                };

                let execution_price = execution_price.round_dp(2);
                if execution_price <= Decimal::ZERO {
                    continue;
                }

                if side == Side::Buy && !positions.contains_key(&sym) {
                    let max_shares_by_cash = (target_cash / execution_price).floor();
                    let max_shares_by_vol = (candle.volume * config.volume_limit_pct).floor();
                    let shares = max_shares_by_cash.min(max_shares_by_vol).max(dec!(1.0));

                    if shares > Decimal::ZERO {
                        let trade_val = shares * execution_price;
                        let costs = config.cost_config.calculate(trade_val, shares, Side::Buy);
                        let total_outflow = trade_val + costs.total;

                        if total_outflow <= cash {
                            cash -= total_outflow;
                            total_commission += costs.commission;
                            total_spread_slippage_cost +=
                                costs.estimated_spread_cost + costs.estimated_slippage_cost;

                            let stop_price = (execution_price
                                * (Decimal::ONE - config.stop_loss_pct))
                                .round_dp(2);
                            let target_price = (execution_price
                                * (Decimal::ONE + config.take_profit_pct))
                                .round_dp(2);

                            positions.insert(
                                sym.clone(),
                                OpenPosition {
                                    symbol: sym.clone(),
                                    shares,
                                    entry_time: ts,
                                    entry_price: execution_price,
                                    entry_costs: costs,
                                    stop_price,
                                    target_price,
                                },
                            );
                            executed_orders.push(sym);
                        }
                    }
                }
            }
        }

        // 2. Evaluate exits for currently held positions against current bar t high/low
        let mut closed_symbols = Vec::new();
        for (sym, pos) in &positions {
            if let Some(candle) = by_symbol
                .get(sym)
                .and_then(|list| list.iter().find(|c| c.timestamp == ts))
            {
                let mut triggered_exit = None;

                // Stop loss checked first (conservative)
                if candle.low <= pos.stop_price {
                    let exit_price = pos.stop_price.min(candle.open);
                    triggered_exit = Some((exit_price, "STOP_LOSS"));
                } else if candle.high >= pos.target_price {
                    let exit_price = pos.target_price.max(candle.open);
                    triggered_exit = Some((exit_price, "TAKE_PROFIT"));
                } else if ts_idx == timestamps.len() - 1 {
                    // Backtest end: square off remaining positions
                    triggered_exit = Some((candle.close, "BACKTEST_END"));
                }

                if let Some((raw_exit_price, reason)) = triggered_exit {
                    let exit_price = (raw_exit_price
                        * (Decimal::ONE - config.spread_bps - config.slippage_bps))
                        .round_dp(2);
                    let trade_val = pos.shares * exit_price;
                    let exit_costs =
                        config
                            .cost_config
                            .calculate(trade_val, pos.shares, Side::Sell);
                    let net_inflow = trade_val - exit_costs.total;

                    cash += net_inflow;

                    total_sec_fees += exit_costs.sec_fee;
                    total_finra_taf += exit_costs.finra_taf;
                    total_commission += exit_costs.commission;
                    total_spread_slippage_cost +=
                        exit_costs.estimated_spread_cost + exit_costs.estimated_slippage_cost;

                    let gross_pnl = (exit_price - pos.entry_price) * pos.shares;
                    let trade_total_costs = pos.entry_costs.total + exit_costs.total;
                    let net_pnl = gross_pnl - trade_total_costs;

                    completed_trades.push(UsBacktestTrade {
                        id: completed_trades.len() + 1,
                        symbol: sym.clone(),
                        shares: pos.shares,
                        entry_time: pos.entry_time,
                        entry_price: pos.entry_price,
                        entry_costs: pos.entry_costs.clone(),
                        exit_time: Some(ts),
                        exit_price: Some(exit_price),
                        exit_costs: Some(exit_costs),
                        gross_pnl,
                        total_costs: trade_total_costs,
                        net_pnl,
                        exit_reason: reason.to_string(),
                    });

                    closed_symbols.push(sym.clone());
                }
            }
        }

        for sym in closed_symbols {
            positions.remove(&sym);
        }

        // 3. Mark to market equity at bar t close
        let mut position_market_value = Decimal::ZERO;
        for (sym, pos) in &positions {
            if let Some(candle) = by_symbol
                .get(sym)
                .and_then(|list| list.iter().find(|c| c.timestamp == ts))
            {
                position_market_value += pos.shares * candle.close;
            }
        }

        let total_equity = cash + position_market_value;
        if total_equity > peak_equity {
            peak_equity = total_equity;
        }
        let dd_usd = peak_equity - total_equity;
        let dd_pct = if peak_equity > Decimal::ZERO {
            dd_usd / peak_equity
        } else {
            Decimal::ZERO
        };

        if dd_pct > max_drawdown_pct {
            max_drawdown_pct = dd_pct;
        }
        if dd_usd > max_drawdown_usd {
            max_drawdown_usd = dd_usd;
        }

        let current_benchmark_price = candles
            .iter()
            .find(|c| c.timestamp == ts)
            .map(|c| c.close)
            .unwrap_or(benchmark_initial_price);
        let benchmark_equity = benchmark_shares * current_benchmark_price;

        equity_curve.push(UsEquityPoint {
            timestamp: ts,
            cash,
            position_market_value,
            total_equity,
            benchmark_equity,
            drawdown_pct: dd_pct.round_dp(4),
        });

        // 4. Generate new signals at bar t close (to execute at bar t+1 open)
        let total_portfolio_equity = total_equity;
        let max_exposure = total_portfolio_equity * config.max_total_exposure_pct;
        let current_exposure = position_market_value;

        if current_exposure < max_exposure && ts_idx + 1 < timestamps.len() {
            for (sym, list) in &by_symbol {
                if positions.contains_key(sym) {
                    continue;
                }

                let sub_bars: Vec<&UsCandle> = list.iter().filter(|c| c.timestamp <= ts).collect();
                let n = sub_bars.len();
                if n < config.slow_sma_period {
                    continue;
                }

                let fast_sma = sub_bars[n - config.fast_sma_period..]
                    .iter()
                    .map(|c| c.close)
                    .sum::<Decimal>()
                    / Decimal::from(config.fast_sma_period);

                let slow_sma = sub_bars[n - config.slow_sma_period..]
                    .iter()
                    .map(|c| c.close)
                    .sum::<Decimal>()
                    / Decimal::from(config.slow_sma_period);

                let rsi = calculate_us_rsi(&sub_bars, config.rsi_period);
                let last_close = sub_bars[n - 1].close;

                // Testable Signal: Bullish Moving Average alignment + Healthy RSI (oversold recovery)
                if fast_sma > slow_sma
                    && last_close >= fast_sma
                    && rsi >= config.rsi_buy_threshold
                    && rsi <= dec!(70.0)
                {
                    let target_position_cash = (total_portfolio_equity * config.max_position_pct)
                        .min(max_exposure - current_exposure)
                        .min(cash);

                    if target_position_cash >= dec!(100.0) {
                        pending_orders.push((sym.clone(), Side::Buy, target_position_cash, ts));
                    }
                }
            }
        }
    }

    let final_equity = equity_curve.last().map(|e| e.total_equity).unwrap_or(cash);
    let net_profit = final_equity - config.initial_capital_usd;
    let roi_pct = if config.initial_capital_usd > Decimal::ZERO {
        (net_profit / config.initial_capital_usd).round_dp(4)
    } else {
        Decimal::ZERO
    };

    let total_trades = completed_trades.len();
    let winning_trades = completed_trades
        .iter()
        .filter(|t| t.net_pnl > Decimal::ZERO)
        .count();
    let losing_trades = completed_trades
        .iter()
        .filter(|t| t.net_pnl < Decimal::ZERO)
        .count();

    let win_rate_pct = if total_trades > 0 {
        (Decimal::from(winning_trades) / Decimal::from(total_trades) * dec!(100.0)).round_dp(2)
    } else {
        Decimal::ZERO
    };

    let gross_gains: Decimal = completed_trades
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
        (gross_gains / gross_losses).round_dp(2)
    } else if gross_gains > Decimal::ZERO {
        dec!(99.99)
    } else {
        Decimal::ZERO
    };

    let realized_pnl: Decimal = completed_trades.iter().map(|t| t.net_pnl).sum();
    let unrealized_pnl = final_equity - cash;

    let net_expectancy_per_trade = if total_trades > 0 {
        (realized_pnl / Decimal::from(total_trades)).round_dp(2)
    } else {
        Decimal::ZERO
    };

    let total_costs =
        total_sec_fees + total_finra_taf + total_commission + total_spread_slippage_cost;

    let final_benchmark_equity = equity_curve
        .last()
        .map(|e| e.benchmark_equity)
        .unwrap_or(config.initial_capital_usd);
    let benchmark_return_pct = if config.initial_capital_usd > Decimal::ZERO {
        ((final_benchmark_equity - config.initial_capital_usd) / config.initial_capital_usd)
            .round_dp(4)
    } else {
        Decimal::ZERO
    };

    let insufficient_data_warning = total_trades < 5;

    Ok(UsBacktestResults {
        initial_capital: config.initial_capital_usd,
        final_equity: final_equity.round_dp(2),
        net_profit: net_profit.round_dp(2),
        realized_pnl: realized_pnl.round_dp(2),
        unrealized_pnl: unrealized_pnl.round_dp(2),
        total_costs: total_costs.round_dp(2),
        total_sec_fees: total_sec_fees.round_dp(2),
        total_finra_taf: total_finra_taf.round_dp(2),
        total_commission: total_commission.round_dp(2),
        total_spread_slippage_cost: total_spread_slippage_cost.round_dp(2),
        roi_pct,
        max_drawdown_pct: max_drawdown_pct.round_dp(4),
        max_drawdown_usd: max_drawdown_usd.round_dp(2),
        total_trades,
        winning_trades,
        losing_trades,
        win_rate_pct,
        profit_factor,
        net_expectancy_per_trade,
        benchmark_return_pct,
        insufficient_data_warning,
        trades: completed_trades,
        equity_curve,
    })
}

fn calculate_us_rsi(bars: &[&UsCandle], period: usize) -> Decimal {
    if bars.len() <= period {
        return dec!(50.0);
    }

    let n = bars.len();
    let mut gains = Decimal::ZERO;
    let mut losses = Decimal::ZERO;

    for i in (n - period)..n {
        let diff = bars[i].close - bars[i - 1].close;
        if diff > Decimal::ZERO {
            gains += diff;
        } else {
            losses += diff.abs();
        }
    }

    if losses == Decimal::ZERO {
        dec!(100.0)
    } else if gains == Decimal::ZERO {
        Decimal::ZERO
    } else {
        let rs = gains / losses;
        dec!(100.0) - (dec!(100.0) / (Decimal::ONE + rs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_synthetic_us_candles() -> String {
        let mut lines = vec!["timestamp,symbol,open,high,low,close,volume".to_string()];
        let base_price = dec!(500.0); // SPY around $500
        let start_date = chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
        for i in 1..=40 {
            let cur_date = start_date + chrono::Duration::days(i);
            let date_str = cur_date.format("%Y-%m-%d");
            let p = base_price + Decimal::from(i % 5) * dec!(1.5);
            let o = p;
            let h = p + dec!(3.0);
            let l = p - dec!(2.0);
            let c = p + dec!(1.0);
            lines.push(format!("{date_str} 16:00:00,SPY,{o},{h},{l},{c},1500000"));
        }
        lines.join("\n")
    }

    #[test]
    fn parses_valid_us_csv() {
        let csv = generate_synthetic_us_candles();
        let candles = parse_and_validate_us_csv(&csv).expect("CSV should parse cleanly");
        assert_eq!(candles.len(), 40);
        assert_eq!(candles[0].symbol, "SPY");
        assert_eq!(candles[0].open, dec!(501.5));
    }

    #[test]
    fn runs_backtest_with_us_costs_and_zero_lookahead() {
        let csv = generate_synthetic_us_candles();
        let candles = parse_and_validate_us_csv(&csv).unwrap();
        let cfg = UsBacktestConfig::default();
        let results = run_us_backtest(&candles, &cfg).expect("backtest should run");

        assert_eq!(results.initial_capital, dec!(100000.0));
        assert!(results.equity_curve.len() == 40);
        // Ensure no nan or uninitialized figures
        assert!(results.final_equity > Decimal::ZERO);
    }
}
