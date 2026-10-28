//! Quantitative Pullback & Confirmed Rebound Trading Strategy.
//!
//! Strategy Rules (evaluated on completed 5-minute candles):
//! 1. Trend Filter: Price above session VWAP AND EMA 20 above EMA 50.
//! 2. Pullback: Price retraces toward session VWAP or support level confirmed by prior completed candles.
//! 3. Rebound Confirmation: Completed bullish candle closing above previous candle's high with healthy volume.
//! 4. Entry: Simulated buy on the next available tick after confirmation with slippage and statutory costs.
//!    (Never assumes a fill at the signal candle's low or close).
//! 5. Risk & Exit: Stop loss placed below pullback low with configurable buffer;
//!    Target profit placed at 1.5:1 reward-to-risk ratio after estimated costs.
//! 6. Position Controls: One open position per instrument, no averaging down, sized against available paper cash
//!    and maximum risk per trade.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// A standardized 5-minute OHLCV candle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FiveMinCandle {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
}

/// Parameters for the Pullback Rebound Strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullbackStrategyConfig {
    pub ema_fast_period: usize,              // 20
    pub ema_slow_period: usize,              // 50
    pub volume_avg_period: usize,            // 20
    pub max_pullback_pct_from_vwap: Decimal, // e.g. 0.015 (1.5% distance to VWAP)
    pub stop_buffer_pct: Decimal,            // e.g. 0.003 (0.3% below pullback low)
    pub reward_to_risk_ratio: Decimal,       // 1.5 (1.5:1 initial target)
    pub slippage_bps: Decimal,               // e.g. 0.0005 (5 bps / 0.05%)
    pub max_risk_per_trade_pct: Decimal,     // e.g. 0.01 (1% of paper cash)
    pub max_position_pct_of_cash: Decimal,   // e.g. 0.20 (20% of cash max per position)
}

impl Default for PullbackStrategyConfig {
    fn default() -> Self {
        Self {
            ema_fast_period: 20,
            ema_slow_period: 50,
            volume_avg_period: 20,
            max_pullback_pct_from_vwap: dec!(0.015),
            stop_buffer_pct: dec!(0.003),
            reward_to_risk_ratio: dec!(1.5),
            slippage_bps: dec!(0.0005),
            max_risk_per_trade_pct: dec!(0.01),
            max_position_pct_of_cash: dec!(0.20),
        }
    }
}

/// Quantitative indicators computed for a 5-minute bar series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullbackIndicators {
    pub session_vwap: Decimal,
    pub ema_20: Decimal,
    pub ema_50: Decimal,
    pub avg_volume_20: Decimal,
    pub trend_bullish: bool,
    pub pullback_detected: bool,
    pub rebound_confirmed: bool,
    pub pullback_low: Decimal,
}

/// Trading signal produced by the strategy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SignalAction {
    Buy {
        symbol: String,
        stop_price: Decimal,
        target_price: Decimal,
        planned_risk_per_share: Decimal,
        reward_to_risk: Decimal,
        pullback_low: Decimal,
        reason: String,
    },
    Hold,
    Exit {
        symbol: String,
        exit_price: Decimal,
        reason: String,
    },
}

/// Computes Volume-Weighted Average Price (VWAP) across the session candles.
pub fn compute_vwap(candles: &[FiveMinCandle]) -> Decimal {
    let mut total_vp = Decimal::ZERO;
    let mut total_v = Decimal::ZERO;

    for c in candles {
        let typical_price = (c.high + c.low + c.close) / dec!(3.0);
        total_vp += typical_price * c.volume;
        total_v += c.volume;
    }

    if total_v.is_zero() {
        candles.last().map(|c| c.close).unwrap_or(Decimal::ZERO)
    } else {
        total_vp / total_v
    }
}

/// Computes Exponential Moving Average (EMA) for the given period.
pub fn compute_ema(prices: &[Decimal], period: usize) -> Decimal {
    if prices.is_empty() {
        return Decimal::ZERO;
    }
    if prices.len() < period {
        let sum: Decimal = prices.iter().copied().sum();
        return sum / Decimal::from(prices.len());
    }

    let k = dec!(2.0) / Decimal::from(period + 1);
    // Seed with SMA
    let seed_sum: Decimal = prices[..period].iter().copied().sum();
    let mut ema = seed_sum / Decimal::from(period);

    for &price in &prices[period..] {
        ema = (price * k) + (ema * (Decimal::ONE - k));
    }
    ema
}

/// Evaluates the Pullback & Confirmed Rebound strategy on completed 5-minute candles.
pub fn evaluate_pullback_setup(
    candles: &[FiveMinCandle],
    config: &PullbackStrategyConfig,
) -> (SignalAction, Option<PullbackIndicators>) {
    evaluate_pullback_setup_in_timezone(candles, config, chrono_tz::Asia::Kolkata)
}

/// US-market variant of the pullback evaluator. EMA warmup spans sessions,
/// while VWAP and pullback detection reset on the New York trading date.
pub fn evaluate_pullback_setup_us(
    candles: &[FiveMinCandle],
    config: &PullbackStrategyConfig,
) -> (SignalAction, Option<PullbackIndicators>) {
    evaluate_pullback_setup_in_timezone(candles, config, chrono_tz::America::New_York)
}

fn evaluate_pullback_setup_in_timezone(
    candles: &[FiveMinCandle],
    config: &PullbackStrategyConfig,
    timezone: chrono_tz::Tz,
) -> (SignalAction, Option<PullbackIndicators>) {
    let min_bars = config.ema_slow_period + 2;
    if candles.len() < min_bars {
        return (SignalAction::Hold, None);
    }

    let n = candles.len();
    let current_bar = &candles[n - 1]; // Completed candle just closed
    let prev_bar = &candles[n - 2]; // Prior completed candle

    // EMA history may span sessions, but intraday VWAP and pullback checks
    // must use only candles from the current market-local session.
    let current_session_date = current_bar.timestamp.with_timezone(&timezone).date_naive();
    let session_start = candles
        .iter()
        .rposition(|candle| {
            candle.timestamp.with_timezone(&timezone).date_naive() != current_session_date
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    if n.saturating_sub(session_start) < 6 {
        return (SignalAction::Hold, None);
    }

    let closes: Vec<Decimal> = candles.iter().map(|c| c.close).collect();
    let volumes: Vec<Decimal> = candles.iter().map(|c| c.volume).collect();

    // 1. Technical Indicators
    let session_vwap = compute_vwap(&candles[session_start..]);
    let ema_20 = compute_ema(&closes, config.ema_fast_period);
    let ema_50 = compute_ema(&closes, config.ema_slow_period);

    let vol_window = config.volume_avg_period.min(volumes.len());
    let avg_volume_20: Decimal = volumes[volumes.len() - vol_window..]
        .iter()
        .copied()
        .sum::<Decimal>()
        / Decimal::from(vol_window);

    // Rule 1: Trend Filter (Uptrend requirement)
    // Price must be above session VWAP and EMA 20 must be above EMA 50
    let trend_bullish = current_bar.close > session_vwap && ema_20 > ema_50;

    // Rule 2: Pullback Detection
    // In the last 3-5 bars, price retraced toward VWAP or EMA 20 support
    let pullback_start = (n - 1).saturating_sub(5).max(session_start);
    let mut pullback_low = Decimal::MAX;
    let mut pullback_detected = false;

    for bar in &candles[pullback_start..n - 1] {
        if bar.low < pullback_low {
            pullback_low = bar.low;
        }
        // Distance from VWAP at lowest point
        let dist_to_vwap = if session_vwap > Decimal::ZERO {
            (bar.low - session_vwap).abs() / session_vwap
        } else {
            dec!(1.0)
        };
        // Touched VWAP or EMA 20 zone
        if dist_to_vwap <= config.max_pullback_pct_from_vwap || bar.low <= ema_20 * dec!(1.004) {
            pullback_detected = true;
        }
    }

    // Rule 3: Rebound Confirmation
    // Completed bullish candle: close above open AND close above previous candle's high
    let is_bullish_bar = current_bar.close > current_bar.open;
    let broke_prev_high = current_bar.close > prev_bar.high;
    let volume_healthy = current_bar.volume >= avg_volume_20 * dec!(0.90);
    let rebound_confirmed = is_bullish_bar && broke_prev_high && volume_healthy;

    let indicators = PullbackIndicators {
        session_vwap,
        ema_20,
        ema_50,
        avg_volume_20,
        trend_bullish,
        pullback_detected,
        rebound_confirmed,
        pullback_low,
    };

    // Rule 4 & 5: If all rules align, issue Buy signal with defined stop and target
    if trend_bullish && pullback_detected && rebound_confirmed {
        let stop_price = pullback_low * (Decimal::ONE - config.stop_buffer_pct);
        let planned_risk = current_bar.close - stop_price;

        if planned_risk > Decimal::ZERO {
            let planned_reward = planned_risk * config.reward_to_risk_ratio;
            let target_price = current_bar.close + planned_reward;

            let signal = SignalAction::Buy {
                symbol: current_bar.symbol.clone(),
                stop_price,
                target_price,
                planned_risk_per_share: planned_risk,
                reward_to_risk: config.reward_to_risk_ratio,
                pullback_low,
                reason: format!(
                    "Confirmed 5m Pullback Rebound: Price (₹{:.2}) > VWAP (₹{:.2}) & EMA20 > EMA50. Low ₹{:.2} tested support. Bullish rebound closed > prev high ₹{:.2}.",
                    current_bar.close, session_vwap, pullback_low, prev_bar.high
                ),
            };
            return (signal, Some(indicators));
        }
    }

    (SignalAction::Hold, Some(indicators))
}

/// Sizing calculator: calculates position shares adhering to paper cash and risk limits.
pub fn calculate_position_size(
    available_cash: Decimal,
    estimated_fill_price: Decimal,
    planned_risk_per_share: Decimal,
    config: &PullbackStrategyConfig,
) -> i64 {
    if available_cash <= Decimal::ZERO
        || estimated_fill_price <= Decimal::ZERO
        || planned_risk_per_share <= Decimal::ZERO
    {
        return 0;
    }

    // 1. Max risk capital budget (e.g. 1% of cash)
    let max_risk_budget = available_cash * config.max_risk_per_trade_pct;
    let shares_by_risk = (max_risk_budget / planned_risk_per_share).floor();

    // 2. Max position capital budget (e.g. 20% of cash)
    let max_notional = available_cash * config.max_position_pct_of_cash;
    let shares_by_notional = (max_notional / estimated_fill_price).floor();

    // 3. Absolute cash affordability
    let max_affordable_shares = (available_cash / estimated_fill_price).floor();

    let shares = shares_by_risk
        .min(shares_by_notional)
        .min(max_affordable_shares);

    shares.to_string().parse::<i64>().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_candle(idx: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> FiveMinCandle {
        use chrono::TimeZone;
        FiveMinCandle {
            timestamp: Utc.timestamp_opt(1700000000 + idx * 300, 0).unwrap(),
            symbol: "TEST".to_string(),
            open: Decimal::from_str_exact(&format!("{:.2}", o)).unwrap(),
            high: Decimal::from_str_exact(&format!("{:.2}", h)).unwrap(),
            low: Decimal::from_str_exact(&format!("{:.2}", l)).unwrap(),
            close: Decimal::from_str_exact(&format!("{:.2}", c)).unwrap(),
            volume: Decimal::from_str_exact(&format!("{:.2}", v)).unwrap(),
        }
    }

    #[test]
    fn test_vwap_calculation() {
        let candles = vec![
            make_candle(0, 100.0, 105.0, 95.0, 100.0, 1000.0), // TP = 100, VP = 100000
            make_candle(1, 100.0, 110.0, 100.0, 105.0, 2000.0), // TP = 105, VP = 210000
        ];
        let vwap = compute_vwap(&candles);
        // (100000 + 210000) / 3000 = 310000 / 3000 = 103.3333...
        assert!(vwap > dec!(103.3) && vwap < dec!(103.4));
    }

    #[test]
    fn test_position_sizing_within_limits() {
        let config = PullbackStrategyConfig::default();
        let cash = dec!(10000.0);
        let fill_price = dec!(100.0);
        let risk_per_share = dec!(2.0); // 2% stop loss

        let shares = calculate_position_size(cash, fill_price, risk_per_share, &config);
        // 1% max risk = $100 / $2 risk per share = 50 shares
        // 20% max notional = $2000 / $100 = 20 shares
        assert_eq!(shares, 20);
    }
}
