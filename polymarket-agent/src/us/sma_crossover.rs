//! 20-Day / 50-Day Simple Moving Average (SMA) Crossover Strategy for U.S. Equities.
//!
//! Strategy Rules:
//! 1. Market Data: Evaluated on completed daily U.S. stock candles (`CandleInterval::D1`).
//! 2. Moving Average Type: Strictly Simple Moving Average (SMA) — unweighted arithmetic mean,
//!    explicitly NOT Exponential Moving Average (EMA).
//! 3. Indicator Periods: Fast SMA = 20 daily bars, Slow SMA = 50 daily bars.
//! 4. Long Entry Signal (Golden Cross): Generated when 20-day SMA crosses from at or below
//!    50-day SMA to above it:
//!    `SMA20_{t-1} <= SMA50_{t-1}` AND `SMA20_t > SMA50_t`
//! 5. Exit Signal (Death Cross): Generated when 20-day SMA crosses from at or above
//!    50-day SMA to below it:
//!    `SMA20_{t-1} >= SMA50_{t-1}` AND `SMA20_t < SMA50_t`
//! 6. Hold: Generated when no crossover occurs between bar t-1 and bar t.
//! 7. Entry confirmations (`SmaEntryFilters`): by default a golden cross only becomes an
//!    entry when ADX(14) is >= 20 and the market proxy (SPY) closes above its 200-day SMA.
//!    Volume and close-above-SMA50 confirmations exist but are off by default. A rejected
//!    cross is a Hold. Exits are never filtered.
//! 8. Stop management (`SmaTrailingStop`): the initial ATR stop defines 1R; after +1R the stop
//!    moves to breakeven and then trails 3 ATRs below the highest high since entry.
//! 9. Data Integrity & Safety:
//!    - Requires at least 51 completed daily bars (50 bars for SMA50 at bar t, plus 1 prior bar
//!      to verify crossover transition).
//!    - Rejects stale candles (> 5 calendar days old, accounting for weekends & market holidays).
//!    - Rejects non-chronological, duplicate, or malformed OHLCV data.
//!    - Rejects non-U.S.-equity instruments (strictly forbids running on prediction markets or crypto).
//!    - Strictly reports factual metrics; does not invent confidence or expected return values.

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::us::backtest::{
    UsBacktestConfig, UsBacktestResults, UsBacktestTrade, UsCandle, UsEquityPoint,
};
use crate::us::costs::UsTransactionCosts;
use crate::valuation::directional::{Direction, DirectionalView};
use crate::valuation::fair_value::DataQuality;
use crate::venue::types::{AssetClass, Candle, Instrument, Quote, Side};

/// Strategy identity constants.
pub const SMA_STRATEGY_NAME: &str = "US_EQUITY_SMA_CROSSOVER_20_50";
pub const SMA_STRATEGY_VERSION: &str = "1.1.0";
pub const SMA_MOVING_AVERAGE_TYPE: &str = "SMA";
pub const DEFAULT_FAST_SMA_PERIOD: usize = 20;
pub const DEFAULT_SLOW_SMA_PERIOD: usize = 50;
pub const MINIMUM_REQUIRED_DAILY_BARS: usize = 51;
pub const MAX_PERMISSIBLE_STALE_DAYS: i64 = 5;
/// ATR window for the trailing stop, matching live sizing.
pub const TRAILING_ATR_PERIOD: usize = 14;

/// Configuration for the 20-day / 50-day SMA Crossover Strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmaCrossoverConfig {
    /// Fast SMA window in days (default: 20).
    pub fast_sma_period: usize,
    /// Slow SMA window in days (default: 50).
    pub slow_sma_period: usize,
    /// Explicit moving average type (must be "SMA").
    pub moving_average_type: String,
    /// Minimum completed daily candles required to evaluate crossover transition.
    pub min_history_bars: usize,
    /// Maximum allowable age of the latest daily candle in calendar days.
    pub max_stale_days: i64,
    /// Confirmations a golden cross must pass before it becomes an entry.
    #[serde(default)]
    pub entry_filters: SmaEntryFilters,
}

impl Default for SmaCrossoverConfig {
    fn default() -> Self {
        Self {
            fast_sma_period: DEFAULT_FAST_SMA_PERIOD,
            slow_sma_period: DEFAULT_SLOW_SMA_PERIOD,
            moving_average_type: SMA_MOVING_AVERAGE_TYPE.to_string(),
            min_history_bars: MINIMUM_REQUIRED_DAILY_BARS,
            max_stale_days: MAX_PERMISSIBLE_STALE_DAYS,
            entry_filters: SmaEntryFilters::default(),
        }
    }
}

/// Confirmations layered on the raw golden cross.
///
/// A bare 20/50 crossover fires constantly in a sideways market and gives
/// back most of what it makes in trends. These filters only ever remove
/// entries; exits are never gated by them. A zero threshold (or period, for
/// the regime) switches that check off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmaEntryFilters {
    /// Signal-day close must be above the slow SMA.
    pub require_close_above_slow_sma: bool,
    /// Bars in the volume baseline, excluding the signal bar itself.
    pub volume_avg_period: usize,
    /// Signal-day volume must be at least this multiple of the baseline.
    pub min_volume_ratio: Decimal,
    /// Wilder ADX window.
    pub adx_period: usize,
    /// ADX must be at least this, so the cross sits inside a real trend.
    pub min_adx: Decimal,
    /// Regime: the market proxy (SPY) must close above its SMA of this many
    /// days. Evaluated by the caller, which holds the proxy's bars.
    pub regime_sma_period: usize,
}

/// Defaults keep only the confirmations that held up on five years of daily
/// data for the large-cap universe (see examples/us_sma_compare.rs): the
/// regime and ADX cut drawdown at an unchanged profit factor. Signal-day
/// volume and close-above-SMA50 removed winners as often as losers, so they
/// ship disabled.
impl Default for SmaEntryFilters {
    fn default() -> Self {
        Self {
            require_close_above_slow_sma: false,
            volume_avg_period: 20,
            min_volume_ratio: Decimal::ZERO,
            adx_period: 14,
            min_adx: dec!(20),
            regime_sma_period: 200,
        }
    }
}

impl SmaEntryFilters {
    /// The unfiltered 20/50 crossover, for baselines and mechanics tests.
    pub fn disabled() -> Self {
        Self {
            require_close_above_slow_sma: false,
            volume_avg_period: 20,
            min_volume_ratio: Decimal::ZERO,
            adx_period: 14,
            min_adx: Decimal::ZERO,
            regime_sma_period: 0,
        }
    }
}

/// Stop management after entry. The initial ATR stop defines 1R; once price
/// has run `breakeven_at_r` R in our favour the stop moves to entry, and from
/// then on trails `trail_atr_multiplier` ATRs under the highest high since
/// entry. The stop only ever ratchets up. Zero switches a step off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmaTrailingStop {
    pub breakeven_at_r: Decimal,
    pub trail_atr_multiplier: Decimal,
}

impl Default for SmaTrailingStop {
    fn default() -> Self {
        Self {
            breakeven_at_r: dec!(1.0),
            trail_atr_multiplier: dec!(3.0),
        }
    }
}

impl SmaTrailingStop {
    pub fn disabled() -> Self {
        Self {
            breakeven_at_r: Decimal::ZERO,
            trail_atr_multiplier: Decimal::ZERO,
        }
    }
}

/// Wilder's Average Directional Index over aligned high/low/close series.
///
/// Needs `2 * period + 1` bars: `period` to seed the smoothed ranges and
/// another `period` DX values to seed the ADX itself.
pub fn calculate_adx(
    highs: &[Decimal],
    lows: &[Decimal],
    closes: &[Decimal],
    period: usize,
) -> Option<Decimal> {
    let n = closes.len();
    if period == 0 || highs.len() != n || lows.len() != n || n < 2 * period + 1 {
        return None;
    }
    let p = Decimal::from(period);
    let hundred = dec!(100);

    let mut tr = Vec::with_capacity(n - 1);
    let mut plus_dm = Vec::with_capacity(n - 1);
    let mut minus_dm = Vec::with_capacity(n - 1);
    for i in 1..n {
        let up = highs[i] - highs[i - 1];
        let down = lows[i - 1] - lows[i];
        plus_dm.push(if up > down && up > Decimal::ZERO {
            up
        } else {
            Decimal::ZERO
        });
        minus_dm.push(if down > up && down > Decimal::ZERO {
            down
        } else {
            Decimal::ZERO
        });
        tr.push(
            (highs[i] - lows[i])
                .max((highs[i] - closes[i - 1]).abs())
                .max((lows[i] - closes[i - 1]).abs()),
        );
    }

    let dx = |s_tr: Decimal, s_plus: Decimal, s_minus: Decimal| -> Decimal {
        if s_tr <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        let di_plus = hundred * s_plus / s_tr;
        let di_minus = hundred * s_minus / s_tr;
        let sum = di_plus + di_minus;
        if sum <= Decimal::ZERO {
            Decimal::ZERO
        } else {
            hundred * (di_plus - di_minus).abs() / sum
        }
    };

    let mut s_tr: Decimal = tr[..period].iter().copied().sum();
    let mut s_plus: Decimal = plus_dm[..period].iter().copied().sum();
    let mut s_minus: Decimal = minus_dm[..period].iter().copied().sum();
    let mut dxs = vec![dx(s_tr, s_plus, s_minus)];
    for i in period..tr.len() {
        s_tr = s_tr - s_tr / p + tr[i];
        s_plus = s_plus - s_plus / p + plus_dm[i];
        s_minus = s_minus - s_minus / p + minus_dm[i];
        dxs.push(dx(s_tr, s_plus, s_minus));
    }

    let mut adx: Decimal = dxs[..period].iter().copied().sum::<Decimal>() / p;
    for value in &dxs[period..] {
        adx = (adx * (p - Decimal::ONE) + value) / p;
    }
    Some(adx.round_dp(4))
}

/// Why a golden cross on these bars should not become an entry, or `None`
/// when every enabled confirmation passes. Bars end at the signal bar.
pub fn entry_filter_rejection(
    highs: &[Decimal],
    lows: &[Decimal],
    closes: &[Decimal],
    volumes: &[Decimal],
    slow_sma: Decimal,
    filters: &SmaEntryFilters,
) -> Option<String> {
    let close = *closes.last()?;
    if filters.require_close_above_slow_sma && close <= slow_sma {
        return Some(format!(
            "close {close:.2} is not above the slow SMA {slow_sma:.2}"
        ));
    }

    if filters.min_volume_ratio > Decimal::ZERO {
        let period = filters.volume_avg_period;
        if period == 0 || volumes.len() < period + 1 {
            return Some("not enough bars for the volume baseline".to_string());
        }
        let last = volumes.len() - 1;
        let baseline: Decimal = volumes[last - period..last]
            .iter()
            .copied()
            .sum::<Decimal>()
            / Decimal::from(period);
        if baseline <= Decimal::ZERO {
            return Some("volume baseline is zero".to_string());
        }
        let ratio = volumes[last] / baseline;
        if ratio < filters.min_volume_ratio {
            return Some(format!(
                "volume {:.2}x its {period}-day average is below {}x",
                ratio, filters.min_volume_ratio
            ));
        }
    }

    if filters.min_adx > Decimal::ZERO {
        match calculate_adx(highs, lows, closes, filters.adx_period) {
            None => return Some("not enough bars for ADX".to_string()),
            Some(adx) if adx < filters.min_adx => {
                return Some(format!(
                    "ADX({}) {adx:.1} is below {} (no established trend)",
                    filters.adx_period, filters.min_adx
                ));
            }
            Some(_) => {}
        }
    }
    None
}

/// Whether the broad market permits new longs: the proxy's latest close must
/// be above its `period`-day SMA. Missing history blocks entries rather than
/// waving them through.
pub fn regime_rejection(proxy_closes: &[Decimal], period: usize) -> Option<String> {
    if period == 0 {
        return None;
    }
    let Some(sma) = calculate_sma(proxy_closes, period) else {
        return Some(format!(
            "market regime needs {period} daily bars of the proxy, have {}",
            proxy_closes.len()
        ));
    };
    let close = *proxy_closes.last()?;
    (close <= sma).then(|| {
        format!("market proxy close {close:.2} is not above its {period}-day SMA {sma:.2}")
    })
}

/// The stop a long should carry now, given its entry, its initial stop, the
/// highest high since entry and the current ATR. Never below `initial_stop`;
/// callers keep the max of this and the stop already in force.
pub fn managed_stop(
    entry: Decimal,
    initial_stop: Decimal,
    highest_high: Decimal,
    atr: Option<Decimal>,
    rules: &SmaTrailingStop,
) -> Decimal {
    let risk = entry - initial_stop;
    if risk <= Decimal::ZERO
        || rules.breakeven_at_r <= Decimal::ZERO
        || highest_high < entry + risk * rules.breakeven_at_r
    {
        return initial_stop;
    }
    let mut stop = initial_stop.max(entry);
    if let Some(atr) = atr.filter(|a| *a > Decimal::ZERO) {
        if rules.trail_atr_multiplier > Decimal::ZERO {
            stop = stop.max(highest_high - atr * rules.trail_atr_multiplier);
        }
    }
    stop.round_dp(2)
}

/// Action generated by the SMA crossover strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmaSignalAction {
    /// 20-day SMA crossed above 50-day SMA (Golden Cross).
    LongEntry,
    /// 20-day SMA crossed below 50-day SMA (Death Cross).
    Exit,
    /// No crossover event between bar t-1 and bar t.
    Hold,
}

/// Comprehensive signal emitted by the SMA crossover strategy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmaCrossoverSignal {
    pub instrument: String,
    pub signal_time: DateTime<Utc>,
    pub candle_timestamp: DateTime<Utc>,
    pub strategy_name: String,
    pub strategy_version: String,
    pub moving_average_type: String,
    pub fast_period: usize,
    pub slow_period: usize,
    pub fast_sma_current: Decimal,
    pub slow_sma_current: Decimal,
    pub fast_sma_previous: Decimal,
    pub slow_sma_previous: Decimal,
    pub action: SmaSignalAction,
    pub reason: String,
}

/// Validation error encountered when vetting daily candles.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SmaValidationError {
    #[error("Market routing error: {0} is not a USD-denominated U.S. cash equity on Alpaca")]
    InvalidMarketRouting(String),
    #[error(
        "Insufficient daily history for {symbol}: got {count} bars, minimum required is {required}"
    )]
    InsufficientHistory {
        symbol: String,
        count: usize,
        required: usize,
    },
    #[error("Stale daily candle data for {symbol}: latest bar is from {timestamp}, exceeding {max_days} calendar days")]
    StaleData {
        symbol: String,
        timestamp: DateTime<Utc>,
        max_days: i64,
    },
    #[error("Future daily candle for {symbol}: bar timestamp {timestamp} is later than evaluation time {now}")]
    FutureData {
        symbol: String,
        timestamp: DateTime<Utc>,
        now: DateTime<Utc>,
    },
    #[error("Invalid SMA crossover configuration: {0}")]
    InvalidConfiguration(String),
    #[error("Non-chronological or duplicate timestamps in {symbol}: bar {index} at {current} <= previous at {prev}")]
    TimestampOrderViolation {
        symbol: String,
        index: usize,
        current: DateTime<Utc>,
        prev: DateTime<Utc>,
    },
    #[error("Malformed candle at index {index} for {symbol}: {details}")]
    MalformedCandle {
        symbol: String,
        index: usize,
        details: String,
    },
}

/// Calculate the Simple Moving Average (SMA) over the last `period` closing prices.
///
/// Explicit Simple Moving Average formula:
/// SMA_k = (1 / N) * sum_{i=k-N+1}^k Close_i
///
/// Returns None if `closes.len() < period` or `period == 0`.
pub fn calculate_sma(closes: &[Decimal], period: usize) -> Option<Decimal> {
    if period == 0 || closes.len() < period {
        return None;
    }
    let slice = &closes[closes.len() - period..];
    let sum: Decimal = slice.iter().copied().sum();
    Some(sum / Decimal::from(period))
}

/// Validate that daily candles meet all chronological, geometry, freshness, and history rules.
pub fn validate_daily_candles(
    symbol: &str,
    candles: &[Candle],
    now: DateTime<Utc>,
    min_bars: usize,
    max_stale_days: i64,
) -> Result<(), SmaValidationError> {
    if candles.len() < min_bars {
        return Err(SmaValidationError::InsufficientHistory {
            symbol: symbol.to_string(),
            count: candles.len(),
            required: min_bars,
        });
    }

    for (i, c) in candles.iter().enumerate() {
        if c.ts > now {
            return Err(SmaValidationError::FutureData {
                symbol: symbol.to_string(),
                timestamp: c.ts,
                now,
            });
        }
        if i > 0 {
            let prev = candles[i - 1].ts;
            if c.ts <= prev {
                return Err(SmaValidationError::TimestampOrderViolation {
                    symbol: symbol.to_string(),
                    index: i,
                    current: c.ts,
                    prev,
                });
            }
        }

        if c.open <= Decimal::ZERO
            || c.high <= Decimal::ZERO
            || c.low <= Decimal::ZERO
            || c.close <= Decimal::ZERO
        {
            return Err(SmaValidationError::MalformedCandle {
                symbol: symbol.to_string(),
                index: i,
                details: format!(
                    "Non-positive price (open={}, high={}, low={}, close={})",
                    c.open, c.high, c.low, c.close
                ),
            });
        }

        if c.high < c.low
            || c.high < c.open
            || c.high < c.close
            || c.low > c.open
            || c.low > c.close
        {
            return Err(SmaValidationError::MalformedCandle {
                symbol: symbol.to_string(),
                index: i,
                details: format!(
                    "Invalid candle geometry (open={}, high={}, low={}, close={})",
                    c.open, c.high, c.low, c.close
                ),
            });
        }

        if c.volume < Decimal::ZERO {
            return Err(SmaValidationError::MalformedCandle {
                symbol: symbol.to_string(),
                index: i,
                details: format!("Negative volume ({})", c.volume),
            });
        }
    }

    // Freshness check: latest completed candle must not be excessively stale
    let latest_ts = candles.last().unwrap().ts;
    if now > latest_ts && (now - latest_ts) > Duration::days(max_stale_days) {
        return Err(SmaValidationError::StaleData {
            symbol: symbol.to_string(),
            timestamp: latest_ts,
            max_days: max_stale_days,
        });
    }

    Ok(())
}

/// Evaluate SMA-20 / SMA-50 Crossover Signal from completed daily candles.
pub fn evaluate_sma_crossover(
    instrument: &Instrument,
    candles: &[Candle],
    now: DateTime<Utc>,
    config: &SmaCrossoverConfig,
) -> Result<SmaCrossoverSignal, SmaValidationError> {
    if !config
        .moving_average_type
        .eq_ignore_ascii_case(SMA_MOVING_AVERAGE_TYPE)
    {
        return Err(SmaValidationError::InvalidConfiguration(
            "moving_average_type must be SMA".to_string(),
        ));
    }
    if config.fast_sma_period == 0
        || config.slow_sma_period == 0
        || config.fast_sma_period >= config.slow_sma_period
        || config.min_history_bars < config.slow_sma_period.saturating_add(1)
        || config.max_stale_days <= 0
        || config.entry_filters.min_volume_ratio < Decimal::ZERO
        || config.entry_filters.min_adx < Decimal::ZERO
        || (config.entry_filters.min_volume_ratio > Decimal::ZERO
            && config.entry_filters.volume_avg_period == 0)
        || (config.entry_filters.min_adx > Decimal::ZERO && config.entry_filters.adx_period == 0)
    {
        return Err(SmaValidationError::InvalidConfiguration(
            "periods must be positive with fast < slow, history must include slow + 1 bars, and max_stale_days must be positive".to_string(),
        ));
    }
    // 1. Strict Market Routing Check: U.S. cash equities on Alpaca only
    let is_us_equity = instrument.asset_class == AssetClass::Equity
        && instrument.quote_ccy.eq_ignore_ascii_case("USD")
        && instrument.venue().as_str() == "alpaca";

    if !is_us_equity {
        return Err(SmaValidationError::InvalidMarketRouting(format!(
            "venue={}, symbol={}, asset_class={:?}, quote_ccy={}",
            instrument.venue(),
            instrument.symbol(),
            instrument.asset_class,
            instrument.quote_ccy
        )));
    }

    // 2. Validate candle stream integrity
    validate_daily_candles(
        instrument.symbol(),
        candles,
        now,
        config.min_history_bars,
        config.max_stale_days,
    )?;

    let n = candles.len();
    let closes: Vec<Decimal> = candles.iter().map(|c| c.close).collect();

    // 3. Compute SMA-20 and SMA-50 at latest completed bar t (index n-1)
    let fast_sma_current =
        calculate_sma(&closes[..n], config.fast_sma_period).ok_or_else(|| {
            SmaValidationError::InsufficientHistory {
                symbol: instrument.symbol().to_string(),
                count: n,
                required: config.fast_sma_period,
            }
        })?;

    let slow_sma_current =
        calculate_sma(&closes[..n], config.slow_sma_period).ok_or_else(|| {
            SmaValidationError::InsufficientHistory {
                symbol: instrument.symbol().to_string(),
                count: n,
                required: config.slow_sma_period,
            }
        })?;

    // 4. Compute SMA-20 and SMA-50 at previous completed bar t-1 (index n-2)
    let fast_sma_previous =
        calculate_sma(&closes[..n - 1], config.fast_sma_period).ok_or_else(|| {
            SmaValidationError::InsufficientHistory {
                symbol: instrument.symbol().to_string(),
                count: n - 1,
                required: config.fast_sma_period,
            }
        })?;

    let slow_sma_previous =
        calculate_sma(&closes[..n - 1], config.slow_sma_period).ok_or_else(|| {
            SmaValidationError::InsufficientHistory {
                symbol: instrument.symbol().to_string(),
                count: n - 1,
                required: config.slow_sma_period,
            }
        })?;

    let latest_candle = &candles[n - 1];

    // 5. Crossover Evaluation
    let is_golden_cross =
        fast_sma_previous <= slow_sma_previous && fast_sma_current > slow_sma_current;
    let is_death_cross =
        fast_sma_previous >= slow_sma_previous && fast_sma_current < slow_sma_current;

    let filter_rejection = if is_golden_cross {
        let highs: Vec<Decimal> = candles.iter().map(|c| c.high).collect();
        let lows: Vec<Decimal> = candles.iter().map(|c| c.low).collect();
        let volumes: Vec<Decimal> = candles.iter().map(|c| c.volume).collect();
        entry_filter_rejection(
            &highs,
            &lows,
            &closes,
            &volumes,
            slow_sma_current,
            &config.entry_filters,
        )
    } else {
        None
    };

    let (action, reason) = if let Some(rejection) = filter_rejection {
        (
            SmaSignalAction::Hold,
            format!("Golden cross rejected by entry filters: {rejection}"),
        )
    } else if is_golden_cross {
        (
            SmaSignalAction::LongEntry,
            format!(
                "Bullish golden cross: {}-day SMA ({:.2}) crossed above {}-day SMA ({:.2}) (previous: {:.2} <= {:.2})",
                config.fast_sma_period,
                fast_sma_current,
                config.slow_sma_period,
                slow_sma_current,
                fast_sma_previous,
                slow_sma_previous
            ),
        )
    } else if is_death_cross {
        (
            SmaSignalAction::Exit,
            format!(
                "Bearish death cross: {}-day SMA ({:.2}) crossed below {}-day SMA ({:.2}) (previous: {:.2} >= {:.2})",
                config.fast_sma_period,
                fast_sma_current,
                config.slow_sma_period,
                slow_sma_current,
                fast_sma_previous,
                slow_sma_previous
            ),
        )
    } else {
        let status = if fast_sma_current > slow_sma_current {
            "above (bullish trend, no crossover)"
        } else if fast_sma_current < slow_sma_current {
            "below (bearish trend, no crossover)"
        } else {
            "equal to"
        };
        (
            SmaSignalAction::Hold,
            format!(
                "Hold: {}-day SMA ({:.2}) is {} {}-day SMA ({:.2}) with no crossover event",
                config.fast_sma_period,
                fast_sma_current,
                status,
                config.slow_sma_period,
                slow_sma_current
            ),
        )
    };

    Ok(SmaCrossoverSignal {
        instrument: instrument.symbol().to_string(),
        signal_time: now,
        candle_timestamp: latest_candle.ts,
        strategy_name: SMA_STRATEGY_NAME.to_string(),
        strategy_version: SMA_STRATEGY_VERSION.to_string(),
        moving_average_type: config.moving_average_type.clone(),
        fast_period: config.fast_sma_period,
        slow_period: config.slow_sma_period,
        fast_sma_current,
        slow_sma_current,
        fast_sma_previous,
        slow_sma_previous,
        action,
        reason,
    })
}

/// Convert an evaluated SMA crossover signal into an agent `DirectionalView`.
///
/// Adheres strictly to the rule: "Do not invent confidence or expected-return values."
/// Reports neutral baseline probability and records factual SMA metrics in key factors.
pub fn generate_sma_crossover_view(
    instrument: &Instrument,
    _quote: &Quote,
    candles: &[Candle],
    now: DateTime<Utc>,
    config: &SmaCrossoverConfig,
) -> DirectionalView {
    let flat = |reason: String, factor: &str| DirectionalView {
        direction: Direction::Flat,
        p_up: dec!(0.50),
        expected_return_pct: Decimal::ZERO,
        horizon_hours: 24 * 30, // 30-day swing horizon for daily strategy
        confidence: dec!(0.50),
        invalidation_price: None,
        target_price: None,
        reasoning_summary: reason,
        key_factors: vec![factor.to_string()],
        data_quality: DataQuality::Medium,
    };

    let signal = match evaluate_sma_crossover(instrument, candles, now, config) {
        Ok(sig) => sig,
        Err(e) => {
            let factor = match &e {
                SmaValidationError::InvalidMarketRouting(_) => "invalid_market_routing",
                SmaValidationError::InvalidConfiguration(_) => "invalid_configuration",
                SmaValidationError::FutureData { .. } => "future_data",
                SmaValidationError::InsufficientHistory { .. } => "insufficient_history",
                SmaValidationError::StaleData { .. } => "stale_data",
                SmaValidationError::TimestampOrderViolation { .. } => "timestamp_order_violation",
                SmaValidationError::MalformedCandle { .. } => "malformed_candle",
            };
            return flat(format!("Skipping {}: {}", instrument.symbol(), e), factor);
        }
    };

    match signal.action {
        SmaSignalAction::LongEntry => {
            DirectionalView {
                direction: Direction::Long,
                // The crossover is a deterministic rule signal, not a forecast.
                // Keep model-only fields neutral; sizing uses 0.50 confidence as a
                // conservative risk multiplier for this non-model strategy.
                p_up: dec!(0.50),
                expected_return_pct: Decimal::ZERO,
                horizon_hours: 24 * 30,
                confidence: dec!(0.50),
                invalidation_price: None,
                target_price: None,
                reasoning_summary: signal.reason,
                key_factors: vec![
                    format!("strategy:{}", signal.strategy_name),
                    format!("type:{}", signal.moving_average_type),
                    format!("sma{}_curr:{}", signal.fast_period, signal.fast_sma_current),
                    format!("sma{}_curr:{}", signal.slow_period, signal.slow_sma_current),
                    format!(
                        "sma{}_prev:{}",
                        signal.fast_period, signal.fast_sma_previous
                    ),
                    format!(
                        "sma{}_prev:{}",
                        signal.slow_period, signal.slow_sma_previous
                    ),
                    "action:LONG_ENTRY_GOLDEN_CROSS".to_string(),
                ],
                data_quality: DataQuality::High,
            }
        }
        SmaSignalAction::Exit => flat(signal.reason, "sma_death_cross_exit"),
        SmaSignalAction::Hold => flat(signal.reason, "sma_hold_no_crossover"),
    }
}

/// Simulated trade tracking for SMA Crossover backtesting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmaBacktestTrade {
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
    pub fast_sma_at_entry: Decimal,
    pub slow_sma_at_entry: Decimal,
    pub fast_sma_at_exit: Option<Decimal>,
    pub slow_sma_at_exit: Option<Decimal>,
}

/// Run an SMA-20 / SMA-50 Crossover backtest with strict zero-lookahead bias,
/// realistic transaction costs, and buy-and-hold baseline comparison.
#[derive(Debug, Clone)]
struct SmaBacktestPosition {
    shares: Decimal,
    entry_price: Decimal,
    stop_price: Decimal,
    /// The stop set at entry; its distance from entry is 1R.
    initial_stop: Decimal,
    highest_high: Decimal,
    entry_time: DateTime<Utc>,
    entry_costs: UsTransactionCosts,
    fast_sma_entry: Decimal,
    slow_sma_entry: Decimal,
}

#[derive(Debug, Clone)]
struct SmaPendingOrder {
    symbol: String,
    side: Side,
    target_cash: Decimal,
    signal_time: DateTime<Utc>,
    reason: String,
    fast_sma: Decimal,
    slow_sma: Decimal,
    stop_pct: Decimal,
}

/// ATR percentage using the same true-range calculation as live position
/// sizing. Inputs are the bars available at the signal close, so this cannot
/// look ahead into the bar used to fill the next-session order.
fn us_atr_pct(candles: &[&UsCandle], period: usize) -> Option<Decimal> {
    if period == 0 || candles.len() < period + 1 {
        return None;
    }
    let start = candles.len() - period;
    let mut total = Decimal::ZERO;
    for index in start..candles.len() {
        let candle = candles[index];
        let previous_close = candles[index - 1].close;
        let range = (candle.high - candle.low)
            .max((candle.high - previous_close).abs())
            .max((candle.low - previous_close).abs());
        total += range;
    }
    let last_close = candles.last()?.close;
    if last_close <= Decimal::ZERO {
        return None;
    }
    let atr = total / Decimal::from(period);
    (atr > Decimal::ZERO).then_some(atr / last_close)
}

pub fn run_us_sma_crossover_backtest(
    candles: &[UsCandle],
    config: &UsBacktestConfig,
) -> anyhow::Result<UsBacktestResults> {
    run_us_sma_crossover_backtest_with_regime(candles, None, config)
}

/// As [`run_us_sma_crossover_backtest`], with the market-regime proxy's daily
/// bars supplied separately so the proxy need not be traded. Without them the
/// regime is read from SPY in `candles`.
pub fn run_us_sma_crossover_backtest_with_regime(
    candles: &[UsCandle],
    regime_candles: Option<&[UsCandle]>,
    config: &UsBacktestConfig,
) -> anyhow::Result<UsBacktestResults> {
    if candles.is_empty() {
        anyhow::bail!("Cannot run SMA crossover backtest on empty candles slice");
    }
    if config.atr_period == 0
        || config.atr_multiplier <= Decimal::ZERO
        || config.min_stop_pct <= Decimal::ZERO
        || config.max_stop_pct < config.min_stop_pct
        || config.fast_sma_period == 0
        || config.slow_sma_period == 0
        || config.fast_sma_period >= config.slow_sma_period
    {
        anyhow::bail!("Invalid SMA period or ATR stop configuration for backtest");
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
        if list.len() < MINIMUM_REQUIRED_DAILY_BARS {
            anyhow::bail!(
                "Symbol {} has only {} daily bars; at least {} required for SMA-20/SMA-50 crossover backtest",
                sym,
                list.len(),
                MINIMUM_REQUIRED_DAILY_BARS
            );
        }
    }

    let filters = &config.sma_entry_filters;
    let mut regime_bars: Vec<UsCandle> = match regime_candles {
        Some(bars) => bars.to_vec(),
        None => by_symbol.get("SPY").cloned().unwrap_or_default(),
    };
    if filters.regime_sma_period > 0 && regime_bars.is_empty() {
        anyhow::bail!(
            "The market-regime filter needs SPY daily bars; include SPY or set regime_sma_period = 0"
        );
    }
    regime_bars.sort_by_key(|c| c.timestamp);
    let regime_closes: Vec<Decimal> = regime_bars.iter().map(|c| c.close).collect();

    let mut timestamps: Vec<DateTime<Utc>> = candles.iter().map(|c| c.timestamp).collect();
    timestamps.sort();
    timestamps.dedup();

    let mut cash = config.initial_capital_usd;
    let mut positions: HashMap<String, SmaBacktestPosition> = HashMap::new();
    let mut completed_trades: Vec<SmaBacktestTrade> = Vec::new();
    let mut equity_curve: Vec<UsEquityPoint> = Vec::new();
    let mut peak_equity = cash;
    let mut max_drawdown_pct = Decimal::ZERO;
    let mut max_drawdown_usd = Decimal::ZERO;

    // Orders generated at bar t close to be executed at bar t+1 open
    let mut pending_orders: Vec<SmaPendingOrder> = Vec::new();

    let benchmark_symbol = if by_symbol.contains_key("SPY") {
        "SPY"
    } else {
        by_symbol
            .keys()
            .min()
            .map(String::as_str)
            .expect("non-empty candles were grouped above")
    };
    let benchmark_candles = &by_symbol[benchmark_symbol];
    let benchmark_initial_price = benchmark_candles
        .first()
        .map(|c| c.open)
        .unwrap_or(dec!(100.0));
    let benchmark_shares = config.initial_capital_usd / benchmark_initial_price;

    let mut total_sec_fees = Decimal::ZERO;
    let mut total_finra_taf = Decimal::ZERO;
    let mut total_commission = Decimal::ZERO;
    let mut total_spread_slippage_cost = Decimal::ZERO;

    for (ts_idx, &ts) in timestamps.iter().enumerate() {
        // 1. Execute pending orders generated at bar t-1 close using bar t open
        let mut waiting_orders = Vec::new();
        for pending in pending_orders.drain(..) {
            let Some(candle) = by_symbol
                .get(&pending.symbol)
                .and_then(|list| list.iter().find(|c| c.timestamp == ts))
            else {
                // A global timestamp may belong to a different symbol. Keep
                // this order until its own next bar instead of silently
                // dropping a valid signal.
                waiting_orders.push(pending);
                continue;
            };

            let SmaPendingOrder {
                symbol: sym,
                side,
                target_cash,
                signal_time: _gen_ts,
                reason,
                fast_sma,
                slow_sma,
                stop_pct,
            } = pending;
            let execution_price = match side {
                Side::Buy => candle.open * (Decimal::ONE + config.spread_bps + config.slippage_bps),
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
                let shares = max_shares_by_cash.min(max_shares_by_vol);

                if shares > Decimal::ZERO {
                    let trade_val = shares * execution_price;
                    let costs = config.cost_config.calculate(trade_val, shares, Side::Buy);
                    let total_outflow = trade_val + costs.total;

                    if total_outflow <= cash {
                        cash -= total_outflow;
                        total_commission += costs.commission;
                        total_spread_slippage_cost +=
                            costs.estimated_spread_cost + costs.estimated_slippage_cost;

                        let stop_price = (execution_price * (Decimal::ONE - stop_pct)).round_dp(2);
                        positions.insert(
                            sym.clone(),
                            SmaBacktestPosition {
                                shares,
                                entry_price: execution_price,
                                stop_price,
                                initial_stop: stop_price,
                                highest_high: candle.high,
                                entry_time: ts,
                                entry_costs: costs,
                                fast_sma_entry: fast_sma,
                                slow_sma_entry: slow_sma,
                            },
                        );
                    }
                }
            } else if side == Side::Sell && positions.contains_key(&sym) {
                let pos = positions.remove(&sym).unwrap();
                let trade_val = pos.shares * execution_price;
                let exit_costs = config
                    .cost_config
                    .calculate(trade_val, pos.shares, Side::Sell);
                let net_inflow = trade_val - exit_costs.total;

                cash += net_inflow;
                total_sec_fees += exit_costs.sec_fee;
                total_finra_taf += exit_costs.finra_taf;
                total_commission += exit_costs.commission;
                total_spread_slippage_cost +=
                    exit_costs.estimated_spread_cost + exit_costs.estimated_slippage_cost;

                let gross_pnl = (execution_price - pos.entry_price) * pos.shares;
                let trade_total_costs = pos.entry_costs.total + exit_costs.total;
                let net_pnl = gross_pnl - trade_total_costs;

                completed_trades.push(SmaBacktestTrade {
                    id: completed_trades.len() + 1,
                    symbol: sym.clone(),
                    shares: pos.shares,
                    entry_time: pos.entry_time,
                    entry_price: pos.entry_price,
                    entry_costs: pos.entry_costs,
                    exit_time: Some(ts),
                    exit_price: Some(execution_price),
                    exit_costs: Some(exit_costs),
                    gross_pnl,
                    total_costs: trade_total_costs,
                    net_pnl,
                    exit_reason: reason,
                    fast_sma_at_entry: pos.fast_sma_entry,
                    slow_sma_at_entry: pos.slow_sma_entry,
                    fast_sma_at_exit: Some(fast_sma),
                    slow_sma_at_exit: Some(slow_sma),
                });
            }
        }
        pending_orders = waiting_orders;

        // Apply the same ATR protective stop used by live sizing. If the bar
        // opens through the stop, assume the worse open; otherwise fill at
        // the stop, with spread and slippage applied to either execution.
        let stopped_symbols: Vec<String> = positions
            .iter()
            .filter_map(|(symbol, position)| {
                by_symbol
                    .get(symbol)
                    .and_then(|list| list.iter().find(|c| c.timestamp == ts))
                    .filter(|candle| candle.low <= position.stop_price)
                    .map(|_| symbol.clone())
            })
            .collect();
        for symbol in stopped_symbols {
            let Some(candle) = by_symbol
                .get(&symbol)
                .and_then(|list| list.iter().find(|c| c.timestamp == ts))
            else {
                continue;
            };
            let Some(position) = positions.remove(&symbol) else {
                continue;
            };
            let stop_fill = candle.open.min(position.stop_price)
                * (Decimal::ONE - config.spread_bps - config.slippage_bps);
            let exit_price = stop_fill.round_dp(2);
            let trade_value = position.shares * exit_price;
            let exit_costs = config
                .cost_config
                .calculate(trade_value, position.shares, Side::Sell);
            cash += trade_value - exit_costs.total;
            total_sec_fees += exit_costs.sec_fee;
            total_finra_taf += exit_costs.finra_taf;
            total_commission += exit_costs.commission;
            total_spread_slippage_cost +=
                exit_costs.estimated_spread_cost + exit_costs.estimated_slippage_cost;

            let gross_pnl = (exit_price - position.entry_price) * position.shares;
            let trade_total_costs = position.entry_costs.total + exit_costs.total;
            completed_trades.push(SmaBacktestTrade {
                id: completed_trades.len() + 1,
                symbol: symbol.clone(),
                shares: position.shares,
                entry_time: position.entry_time,
                entry_price: position.entry_price,
                entry_costs: position.entry_costs,
                exit_time: Some(ts),
                exit_price: Some(exit_price),
                exit_costs: Some(exit_costs),
                gross_pnl,
                total_costs: trade_total_costs,
                net_pnl: gross_pnl - trade_total_costs,
                exit_reason: if position.stop_price > position.initial_stop {
                    "TRAILING_STOP".to_string()
                } else {
                    "ATR_STOP".to_string()
                },
                fast_sma_at_entry: position.fast_sma_entry,
                slow_sma_at_entry: position.slow_sma_entry,
                fast_sma_at_exit: None,
                slow_sma_at_exit: None,
            });
        }

        // Ratchet stops at bar t close from bars up to and including t; the
        // new level first applies to bar t+1, so this cannot look ahead.
        for (symbol, position) in positions.iter_mut() {
            let Some(list) = by_symbol.get(symbol) else {
                continue;
            };
            let end = list.partition_point(|c| c.timestamp <= ts);
            if end == 0 || list[end - 1].timestamp != ts {
                continue;
            }
            position.highest_high = position.highest_high.max(list[end - 1].high);
            let history: Vec<&UsCandle> = list[..end].iter().collect();
            let atr = us_atr_pct(&history, config.atr_period).map(|pct| pct * list[end - 1].close);
            let managed = managed_stop(
                position.entry_price,
                position.initial_stop,
                position.highest_high,
                atr,
                &config.sma_trailing_stop,
            );
            position.stop_price = position.stop_price.max(managed);
        }

        // 2. Mark to market equity at bar t close
        let mut position_market_value = Decimal::ZERO;
        for (sym, pos) in &positions {
            if let Some(list) = by_symbol.get(sym) {
                if let Some(index) = list.partition_point(|c| c.timestamp <= ts).checked_sub(1) {
                    position_market_value += pos.shares * list[index].close;
                }
            }
        }

        let total_equity = cash + position_market_value;
        if total_equity > peak_equity {
            peak_equity = total_equity;
        }
        let dd_usd = peak_equity - total_equity;
        let dd_pct = if peak_equity > Decimal::ZERO {
            (dd_usd / peak_equity).round_dp(4)
        } else {
            Decimal::ZERO
        };
        if dd_pct > max_drawdown_pct {
            max_drawdown_pct = dd_pct;
            max_drawdown_usd = dd_usd;
        }

        let current_benchmark_price = benchmark_candles
            .partition_point(|c| c.timestamp <= ts)
            .checked_sub(1)
            .map(|index| benchmark_candles[index].close)
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

        // 3. Generate new SMA crossover signals at bar t close (executing at bar t+1 open)
        let total_portfolio_equity = total_equity;
        let max_exposure = total_portfolio_equity * config.max_total_exposure_pct;
        let current_exposure = position_market_value;

        if ts_idx + 1 < timestamps.len() {
            let regime_block = regime_rejection(
                &regime_closes[..regime_bars.partition_point(|c| c.timestamp <= ts)],
                filters.regime_sma_period,
            );
            let mut symbols: Vec<String> = by_symbol.keys().cloned().collect();
            symbols.sort();
            let reserved_buy_cash: Decimal = pending_orders
                .iter()
                .filter(|order| order.side == Side::Buy)
                .map(|order| order.target_cash)
                .sum();
            let mut available_exposure =
                (max_exposure - current_exposure - reserved_buy_cash).max(Decimal::ZERO);
            let mut available_cash = (cash - reserved_buy_cash).max(Decimal::ZERO);

            for sym in symbols {
                let list = &by_symbol[&sym];
                if list
                    .binary_search_by_key(&ts, |candle| candle.timestamp)
                    .is_err()
                {
                    continue;
                }
                let sub_bars: Vec<&UsCandle> = list.iter().filter(|c| c.timestamp <= ts).collect();
                let n = sub_bars.len();
                if n < config.slow_sma_period.saturating_add(1) {
                    continue;
                }

                let closes: Vec<Decimal> = sub_bars.iter().map(|c| c.close).collect();
                let fast_sma_curr = calculate_sma(&closes[..n], config.fast_sma_period).unwrap();
                let slow_sma_curr = calculate_sma(&closes[..n], config.slow_sma_period).unwrap();
                let fast_sma_prev =
                    calculate_sma(&closes[..n - 1], config.fast_sma_period).unwrap();
                let slow_sma_prev =
                    calculate_sma(&closes[..n - 1], config.slow_sma_period).unwrap();
                let Some(atr_pct) = us_atr_pct(&sub_bars, config.atr_period) else {
                    continue;
                };
                let stop_pct = (atr_pct * config.atr_multiplier)
                    .clamp(config.min_stop_pct, config.max_stop_pct);

                let is_golden_cross =
                    fast_sma_prev <= slow_sma_prev && fast_sma_curr > slow_sma_curr;
                let is_death_cross =
                    fast_sma_prev >= slow_sma_prev && fast_sma_curr < slow_sma_curr;

                let is_confirmed_entry = is_golden_cross
                    && regime_block.is_none()
                    && entry_filter_rejection(
                        &sub_bars.iter().map(|c| c.high).collect::<Vec<_>>(),
                        &sub_bars.iter().map(|c| c.low).collect::<Vec<_>>(),
                        &closes,
                        &sub_bars.iter().map(|c| c.volume).collect::<Vec<_>>(),
                        slow_sma_curr,
                        filters,
                    )
                    .is_none();

                if is_confirmed_entry
                    && !positions.contains_key(&sym)
                    && available_exposure > Decimal::ZERO
                    && available_cash > Decimal::ZERO
                {
                    let target_position_cash = (total_portfolio_equity * config.max_position_pct)
                        .min(available_exposure)
                        .min(available_cash);

                    if target_position_cash >= dec!(100.0) {
                        pending_orders.push(SmaPendingOrder {
                            symbol: sym.clone(),
                            side: Side::Buy,
                            target_cash: target_position_cash,
                            signal_time: ts,
                            reason: "SMA_GOLDEN_CROSS".to_string(),
                            fast_sma: fast_sma_curr,
                            slow_sma: slow_sma_curr,
                            stop_pct,
                        });
                        available_exposure -= target_position_cash;
                        available_cash -= target_position_cash;
                    }
                } else if is_death_cross && positions.contains_key(&sym) {
                    pending_orders.push(SmaPendingOrder {
                        symbol: sym,
                        side: Side::Sell,
                        target_cash: Decimal::ZERO,
                        signal_time: ts,
                        reason: "SMA_DEATH_CROSS".to_string(),
                        fast_sma: fast_sma_curr,
                        slow_sma: slow_sma_curr,
                        stop_pct: Decimal::ZERO,
                    });
                }
            }
        }
    }

    // 4. Square off remaining positions at end of backtest
    let final_ts = *timestamps.last().unwrap();
    for (sym, pos) in positions {
        if let Some(candle) = by_symbol
            .get(&sym)
            .and_then(|list| list.iter().rev().find(|c| c.timestamp <= final_ts))
        {
            let exit_price =
                candle.close * (Decimal::ONE - config.spread_bps - config.slippage_bps);
            let exit_price = exit_price.round_dp(2);
            let trade_val = pos.shares * exit_price;
            let exit_costs = config
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

            completed_trades.push(SmaBacktestTrade {
                id: completed_trades.len() + 1,
                symbol: sym.clone(),
                shares: pos.shares,
                entry_time: pos.entry_time,
                entry_price: pos.entry_price,
                entry_costs: pos.entry_costs,
                exit_time: Some(final_ts),
                exit_price: Some(exit_price),
                exit_costs: Some(exit_costs),
                gross_pnl,
                total_costs: trade_total_costs,
                net_pnl,
                exit_reason: "BACKTEST_END_SQUARE_OFF".to_string(),
                fast_sma_at_entry: pos.fast_sma_entry,
                slow_sma_at_entry: pos.slow_sma_entry,
                fast_sma_at_exit: None,
                slow_sma_at_exit: None,
            });
        }
    }

    // Remaining positions were liquidated at their last available close
    // above. Report cash after exit costs, not the pre-liquidation mark-to-
    // market value that omitted those costs.
    let final_equity = cash;
    let final_drawdown_usd = (peak_equity - final_equity).max(Decimal::ZERO);
    let final_drawdown_pct = if peak_equity > Decimal::ZERO {
        (final_drawdown_usd / peak_equity).round_dp(4)
    } else {
        Decimal::ZERO
    };
    if final_drawdown_pct > max_drawdown_pct {
        max_drawdown_pct = final_drawdown_pct;
        max_drawdown_usd = final_drawdown_usd;
    }
    if let Some(last) = equity_curve.last_mut() {
        last.cash = final_equity;
        last.position_market_value = Decimal::ZERO;
        last.total_equity = final_equity;
        last.drawdown_pct = final_drawdown_pct;
    }
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
        dec!(999.99)
    } else {
        Decimal::ZERO
    };

    let net_expectancy_per_trade = if total_trades > 0 {
        (net_profit / Decimal::from(total_trades)).round_dp(2)
    } else {
        Decimal::ZERO
    };

    let benchmark_final_price = benchmark_candles
        .iter()
        .rev()
        .find(|c| c.timestamp <= final_ts)
        .map(|c| c.close)
        .unwrap_or(benchmark_initial_price);
    let benchmark_return_pct = if benchmark_initial_price > Decimal::ZERO {
        ((benchmark_final_price - benchmark_initial_price) / benchmark_initial_price).round_dp(4)
    } else {
        Decimal::ZERO
    };

    let legacy_trades: Vec<UsBacktestTrade> = completed_trades
        .into_iter()
        .map(|t| UsBacktestTrade {
            id: t.id,
            symbol: t.symbol,
            shares: t.shares,
            entry_time: t.entry_time,
            entry_price: t.entry_price,
            entry_costs: t.entry_costs,
            exit_time: t.exit_time,
            exit_price: t.exit_price,
            exit_costs: t.exit_costs,
            gross_pnl: t.gross_pnl,
            total_costs: t.total_costs,
            net_pnl: t.net_pnl,
            exit_reason: t.exit_reason,
        })
        .collect();

    let total_costs =
        total_sec_fees + total_finra_taf + total_commission + total_spread_slippage_cost;

    Ok(UsBacktestResults {
        initial_capital: config.initial_capital_usd,
        final_equity,
        net_profit,
        realized_pnl: net_profit,
        unrealized_pnl: Decimal::ZERO,
        total_costs,
        total_sec_fees,
        total_finra_taf,
        total_commission,
        total_spread_slippage_cost,
        roi_pct,
        max_drawdown_pct,
        max_drawdown_usd,
        total_trades,
        winning_trades,
        losing_trades,
        win_rate_pct,
        profit_factor,
        net_expectancy_per_trade,
        benchmark_return_pct,
        insufficient_data_warning: total_trades < 30,
        trades: legacy_trades,
        equity_curve,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::us::backtest::parse_and_validate_us_csv;
    use crate::venue::types::{InstrumentId, InstrumentMeta, VenueId};
    use chrono::TimeZone;

    /// The bare crossover, for tests of signal and fill mechanics.
    fn sma_backtest_config() -> UsBacktestConfig {
        UsBacktestConfig {
            fast_sma_period: DEFAULT_FAST_SMA_PERIOD,
            slow_sma_period: DEFAULT_SLOW_SMA_PERIOD,
            sma_entry_filters: SmaEntryFilters::disabled(),
            sma_trailing_stop: SmaTrailingStop::disabled(),
            ..UsBacktestConfig::default()
        }
    }

    fn unfiltered_config() -> SmaCrossoverConfig {
        SmaCrossoverConfig {
            entry_filters: SmaEntryFilters::disabled(),
            ..SmaCrossoverConfig::default()
        }
    }

    fn test_equity(symbol: &str) -> Instrument {
        Instrument {
            id: InstrumentId::new(VenueId::new("alpaca"), symbol),
            asset_class: AssetClass::Equity,
            display_name: format!("{symbol} Equity"),
            quote_ccy: "USD".to_string(),
            tick_size: Some(dec!(0.01)),
            lot_size: Some(dec!(1.0)),
            min_notional: Some(dec!(1.0)),
            min_qty: Some(dec!(0.001)),
            fractional: true,
            meta: InstrumentMeta::Equity {
                exchange: "NASDAQ".to_string(),
            },
        }
    }

    fn test_crypto(symbol: &str) -> Instrument {
        Instrument {
            id: InstrumentId::new(VenueId::new("alpaca"), symbol),
            asset_class: AssetClass::CryptoSpot,
            display_name: format!("{symbol} Spot"),
            quote_ccy: "USD".to_string(),
            tick_size: Some(dec!(0.01)),
            lot_size: Some(dec!(0.0001)),
            min_notional: Some(dec!(1.0)),
            min_qty: Some(dec!(0.0001)),
            fractional: true,
            meta: InstrumentMeta::Spot {
                base: "BTC".to_string(),
            },
        }
    }

    fn sample_daily_candles(count: usize, base_price: Decimal, trend: Decimal) -> Vec<Candle> {
        let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 16, 0, 0).unwrap();
        (0..count)
            .map(|i| {
                let close = base_price + Decimal::from(i) * trend;
                Candle {
                    ts: base_time + Duration::days(i as i64),
                    open: close - dec!(0.50),
                    high: close + dec!(1.00),
                    low: close - dec!(1.00),
                    close,
                    volume: dec!(1000000),
                }
            })
            .collect()
    }

    #[test]
    fn test_sma_calculation_accuracy() {
        let prices = vec![dec!(10.0), dec!(12.0), dec!(14.0), dec!(16.0), dec!(18.0)];
        // 5-period SMA: (10 + 12 + 14 + 16 + 18) / 5 = 70 / 5 = 14.0
        let sma5 = calculate_sma(&prices, 5).unwrap();
        assert_eq!(sma5, dec!(14.0));

        // 3-period SMA: (14 + 16 + 18) / 3 = 48 / 3 = 16.0
        let sma3 = calculate_sma(&prices, 3).unwrap();
        assert_eq!(sma3, dec!(16.0));

        // Insufficient prices returns None
        assert_eq!(calculate_sma(&prices, 6), None);
        // Period 0 returns None
        assert_eq!(calculate_sma(&prices, 0), None);
    }

    #[test]
    fn test_golden_cross_generates_long_entry() {
        // Construct 55 daily candles:
        // First 54 bars: 100.0 (SMA20 = 100.0, SMA50 = 100.0, so SMA20 <= SMA50 at bar t-1)
        // Bar 55 (index 54): price surges to 150.0 (SMA20 = 102.5 > SMA50 = 101.0 at bar t -> Golden Cross!)
        let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 16, 0, 0).unwrap();
        let mut candles = Vec::new();
        for i in 0..55 {
            let close = if i < 54 { dec!(100.0) } else { dec!(150.0) };

            candles.push(Candle {
                ts: base_time + Duration::days(i as i64),
                open: close - dec!(0.50),
                high: close + dec!(1.00),
                low: close - dec!(1.00),
                close,
                volume: dec!(2000000),
            });
        }

        let instrument = test_equity("SPY");
        let config = unfiltered_config();
        let now = candles.last().unwrap().ts + Duration::hours(2);

        let signal = evaluate_sma_crossover(&instrument, &candles, now, &config)
            .expect("evaluation should succeed");

        assert_eq!(signal.action, SmaSignalAction::LongEntry);
        assert_eq!(signal.moving_average_type, "SMA");
        assert_eq!(signal.strategy_name, SMA_STRATEGY_NAME);
        assert_eq!(signal.fast_sma_previous, dec!(100.0));
        assert_eq!(signal.slow_sma_previous, dec!(100.0));
        assert!(signal.fast_sma_previous <= signal.slow_sma_previous);
        assert!(signal.fast_sma_current > signal.slow_sma_current);
        assert!(signal.reason.contains("Bullish golden cross"));
    }

    #[test]
    fn test_death_cross_generates_exit() {
        // Construct 55 daily candles:
        // First 54 bars: 100.0 (SMA20 = 100.0, SMA50 = 100.0, so SMA20 >= SMA50 at bar t-1)
        // Bar 55 (index 54): price plunges to 50.0 (SMA20 = 97.5 < SMA50 = 99.0 at bar t -> Death Cross!)
        let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 16, 0, 0).unwrap();
        let mut candles = Vec::new();
        for i in 0..55 {
            let close = if i < 54 { dec!(100.0) } else { dec!(50.0) };

            candles.push(Candle {
                ts: base_time + Duration::days(i as i64),
                open: close - dec!(0.50),
                high: close + dec!(1.00),
                low: close - dec!(1.00),
                close,
                volume: dec!(2000000),
            });
        }

        let instrument = test_equity("AAPL");
        let config = SmaCrossoverConfig::default();
        let now = candles.last().unwrap().ts + Duration::hours(2);

        let signal = evaluate_sma_crossover(&instrument, &candles, now, &config)
            .expect("evaluation should succeed");

        assert_eq!(signal.action, SmaSignalAction::Exit);
        assert_eq!(signal.moving_average_type, "SMA");
        assert_eq!(signal.fast_sma_previous, dec!(100.0));
        assert_eq!(signal.slow_sma_previous, dec!(100.0));
        assert!(signal.fast_sma_previous >= signal.slow_sma_previous);
        assert!(signal.fast_sma_current < signal.slow_sma_current);
        assert!(signal.reason.contains("Bearish death cross"));
    }

    #[test]
    fn test_no_crossover_generates_hold() {
        // Steady upward drift: SMA20 is above SMA50 at both bar t-1 and bar t
        let candles = sample_daily_candles(55, dec!(100.0), dec!(1.0));
        let instrument = test_equity("NVDA");
        let config = SmaCrossoverConfig::default();
        let now = candles.last().unwrap().ts + Duration::hours(2);

        let signal = evaluate_sma_crossover(&instrument, &candles, now, &config)
            .expect("evaluation should succeed");

        assert_eq!(signal.action, SmaSignalAction::Hold);
        assert!(signal.reason.contains("Hold"));
    }

    #[test]
    fn test_insufficient_history_rejected() {
        // Only 50 candles provided, whereas 51 are strictly required
        let candles = sample_daily_candles(50, dec!(100.0), dec!(0.5));
        let instrument = test_equity("SPY");
        let config = SmaCrossoverConfig::default();
        let now = candles.last().unwrap().ts + Duration::hours(2);

        let err = evaluate_sma_crossover(&instrument, &candles, now, &config).unwrap_err();
        match err {
            SmaValidationError::InsufficientHistory {
                count, required, ..
            } => {
                assert_eq!(count, 50);
                assert_eq!(required, 51);
            }
            other => panic!("Expected InsufficientHistory, got {other:?}"),
        }
    }

    #[test]
    fn test_stale_candles_rejected() {
        let candles = sample_daily_candles(55, dec!(100.0), dec!(0.5));
        let instrument = test_equity("SPY");
        let config = SmaCrossoverConfig::default();
        // Set evaluation time to 10 days after the latest daily candle
        let now = candles.last().unwrap().ts + Duration::days(10);

        let err = evaluate_sma_crossover(&instrument, &candles, now, &config).unwrap_err();
        match err {
            SmaValidationError::StaleData { max_days, .. } => {
                assert_eq!(max_days, 5);
            }
            other => panic!("Expected StaleData, got {other:?}"),
        }
    }

    #[test]
    fn test_future_candles_are_rejected() {
        let candles = sample_daily_candles(55, dec!(100.0), dec!(0.5));
        let instrument = test_equity("SPY");
        let config = SmaCrossoverConfig::default();
        let now = candles.last().unwrap().ts - Duration::hours(1);

        assert!(matches!(
            evaluate_sma_crossover(&instrument, &candles, now, &config),
            Err(SmaValidationError::FutureData { .. })
        ));
    }

    #[test]
    fn test_non_sma_or_invalid_period_configuration_is_rejected() {
        let candles = sample_daily_candles(55, dec!(100.0), dec!(0.5));
        let instrument = test_equity("SPY");
        let now = candles.last().unwrap().ts + Duration::hours(2);
        let mut config = SmaCrossoverConfig::default();
        config.moving_average_type = "EMA".to_string();
        assert!(matches!(
            evaluate_sma_crossover(&instrument, &candles, now, &config),
            Err(SmaValidationError::InvalidConfiguration(_))
        ));

        config = SmaCrossoverConfig::default();
        config.fast_sma_period = config.slow_sma_period;
        assert!(matches!(
            evaluate_sma_crossover(&instrument, &candles, now, &config),
            Err(SmaValidationError::InvalidConfiguration(_))
        ));
    }

    #[test]
    fn test_non_chronological_or_duplicate_timestamps_rejected() {
        let mut candles = sample_daily_candles(55, dec!(100.0), dec!(0.5));
        // Duplicate timestamp on candle 30
        candles[30].ts = candles[29].ts;

        let instrument = test_equity("SPY");
        let config = SmaCrossoverConfig::default();
        let now = candles.last().unwrap().ts + Duration::hours(2);

        let err = evaluate_sma_crossover(&instrument, &candles, now, &config).unwrap_err();
        match err {
            SmaValidationError::TimestampOrderViolation { index, .. } => {
                assert_eq!(index, 30);
            }
            other => panic!("Expected TimestampOrderViolation, got {other:?}"),
        }
    }

    #[test]
    fn test_malformed_candles_rejected() {
        let config = SmaCrossoverConfig::default();
        let instrument = test_equity("SPY");
        let now = Utc::now();

        // 1. Negative price
        let mut bad_price = sample_daily_candles(55, dec!(100.0), dec!(0.5));
        bad_price[20].close = dec!(-5.0);
        assert!(matches!(
            evaluate_sma_crossover(&instrument, &bad_price, now, &config),
            Err(SmaValidationError::MalformedCandle { .. })
        ));

        // 2. High < Low
        let mut bad_geom = sample_daily_candles(55, dec!(100.0), dec!(0.5));
        bad_geom[20].high = dec!(50.0);
        bad_geom[20].low = dec!(150.0);
        assert!(matches!(
            evaluate_sma_crossover(&instrument, &bad_geom, now, &config),
            Err(SmaValidationError::MalformedCandle { .. })
        ));

        // 3. Negative volume
        let mut bad_vol = sample_daily_candles(55, dec!(100.0), dec!(0.5));
        bad_vol[20].volume = dec!(-100.0);
        assert!(matches!(
            evaluate_sma_crossover(&instrument, &bad_vol, now, &config),
            Err(SmaValidationError::MalformedCandle { .. })
        ));
    }

    #[test]
    fn test_market_routing_safety_rejects_non_us_equity() {
        let candles = sample_daily_candles(55, dec!(50000.0), dec!(10.0));
        let crypto = test_crypto("BTC/USD");
        let config = SmaCrossoverConfig::default();
        let now = candles.last().unwrap().ts + Duration::hours(2);

        let err = evaluate_sma_crossover(&crypto, &candles, now, &config).unwrap_err();
        assert!(matches!(err, SmaValidationError::InvalidMarketRouting(_)));
    }

    #[test]
    fn test_directional_view_generation_and_integrity() {
        let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 16, 0, 0).unwrap();
        let mut candles = Vec::new();
        for i in 0..55 {
            let close = if i < 40 {
                dec!(100.0)
            } else if i < 53 {
                dec!(90.0)
            } else if i == 53 {
                dec!(91.0)
            } else {
                dec!(250.0)
            };
            candles.push(Candle {
                ts: base_time + Duration::days(i as i64),
                open: close - dec!(0.50),
                high: close + dec!(1.00),
                low: close - dec!(1.00),
                close,
                volume: dec!(2000000),
            });
        }

        let instrument = test_equity("SPY");
        let quote = Quote {
            instrument: instrument.id.clone(),
            bid: dec!(249.95),
            ask: dec!(250.05),
            mid: dec!(250.00),
            last: Some(dec!(250.00)),
            ts: Utc::now(),
            book: None,
        };
        let config = unfiltered_config();
        let now = candles.last().unwrap().ts + Duration::hours(2);

        let view = generate_sma_crossover_view(&instrument, &quote, &candles, now, &config);

        assert_eq!(view.direction, Direction::Long);
        assert_eq!(view.data_quality, DataQuality::High);
        assert!(view.key_factors.iter().any(|f| f == "type:SMA"));
        assert!(view
            .key_factors
            .iter()
            .any(|f| f.starts_with("strategy:US_EQUITY_SMA_CROSSOVER_20_50")));
        assert_eq!(view.p_up, dec!(0.50));
        assert_eq!(view.expected_return_pct, Decimal::ZERO);
        assert_eq!(view.confidence, dec!(0.50));
        assert_eq!(view.target_price, None);
        assert_eq!(view.invalidation_price, None);
    }

    #[test]
    fn test_sma_backtest_with_realistic_costs_and_benchmark() {
        let csv_content = std::fs::read_to_string("data/us_daily_sample_data.csv")
            .expect("us_daily_sample_data.csv must exist");
        let candles = parse_and_validate_us_csv(&csv_content)
            .expect("daily CSV must parse and validate cleanly");

        assert!(candles.len() >= 80, "Expected at least 80 daily candles");

        let config = sma_backtest_config();
        let results = run_us_sma_crossover_backtest(&candles, &config)
            .expect("SMA crossover backtest should succeed");

        assert_eq!(results.initial_capital, dec!(100000.0));
        assert!(results.final_equity > Decimal::ZERO);
        assert_eq!(
            results.final_equity,
            results.equity_curve.last().unwrap().total_equity
        );
        assert!(results.equity_curve.len() >= 80);
        assert_eq!(
            results.total_trades,
            results.winning_trades + results.losing_trades
        );
        // Total costs should account for spread and regulatory fees
        assert!(results.total_costs >= Decimal::ZERO);
        // Compare against benchmark baseline
        assert_ne!(results.benchmark_return_pct, Decimal::ZERO);
    }

    #[test]
    fn sma_backtest_does_not_force_a_share_above_the_volume_limit() {
        let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 16, 0, 0).unwrap();
        let candles = (0..56)
            .map(|i| {
                let close = if i < 54 { dec!(100.0) } else { dec!(150.0) };
                UsCandle {
                    timestamp: base_time + Duration::days(i),
                    symbol: "SPY".to_string(),
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: if i == 55 {
                        Decimal::ZERO
                    } else {
                        dec!(1000000)
                    },
                }
            })
            .collect::<Vec<_>>();

        let results = run_us_sma_crossover_backtest(&candles, &sma_backtest_config())
            .expect("backtest should accept a zero-volume bar");
        assert_eq!(results.total_trades, 0);
        assert_eq!(results.final_equity, results.initial_capital);
    }

    #[test]
    fn sma_backtest_reserves_exposure_for_simultaneous_signals() {
        let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 16, 0, 0).unwrap();
        let symbols = (0..10).map(|i| format!("TICKER{i:02}")).collect::<Vec<_>>();
        let candles = symbols
            .iter()
            .flat_map(|symbol| {
                (0..56).map(move |i| {
                    let close = if i < 54 { dec!(100.0) } else { dec!(150.0) };
                    UsCandle {
                        timestamp: base_time + Duration::days(i),
                        symbol: symbol.clone(),
                        open: close,
                        high: close,
                        low: close,
                        close,
                        volume: dec!(1000000),
                    }
                })
            })
            .collect::<Vec<_>>();

        let results = run_us_sma_crossover_backtest(&candles, &sma_backtest_config())
            .expect("backtest should enforce the portfolio exposure limit");
        assert_eq!(results.total_trades, 5);
        let mut traded_symbols = results
            .trades
            .iter()
            .map(|trade| trade.symbol.clone())
            .collect::<Vec<_>>();
        traded_symbols.sort();
        assert_eq!(
            traded_symbols,
            symbols.into_iter().take(5).collect::<Vec<_>>()
        );
    }

    #[test]
    fn sma_backtest_applies_the_live_atr_protective_stop() {
        let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 16, 0, 0).unwrap();
        let candles = (0..56)
            .map(|i| {
                let close = if i < 54 { dec!(100.0) } else { dec!(150.0) };
                UsCandle {
                    timestamp: base_time + Duration::days(i),
                    symbol: "SPY".to_string(),
                    open: close,
                    high: close,
                    low: if i == 55 { dec!(100.0) } else { close },
                    close,
                    volume: dec!(1000000),
                }
            })
            .collect::<Vec<_>>();

        let results = run_us_sma_crossover_backtest(&candles, &sma_backtest_config())
            .expect("backtest should run");
        assert_eq!(results.total_trades, 1);
        assert_eq!(results.trades[0].exit_reason, "ATR_STOP");
        assert!(results.trades[0].net_pnl < Decimal::ZERO);
    }

    fn us_bar(day: i64, symbol: &str, close: Decimal, volume: Decimal) -> UsCandle {
        let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 16, 0, 0).unwrap();
        UsCandle {
            timestamp: base_time + Duration::days(day),
            symbol: symbol.to_string(),
            open: close,
            high: close + dec!(1.0),
            low: close - dec!(1.0),
            close,
            volume,
        }
    }

    /// 54 flat bars then a surge: the last bar is a golden cross.
    fn golden_cross_candles(volume_on_cross: Decimal) -> Vec<Candle> {
        let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 16, 0, 0).unwrap();
        (0..55)
            .map(|i| {
                let close = if i < 54 { dec!(100.0) } else { dec!(150.0) };
                Candle {
                    ts: base_time + Duration::days(i),
                    open: close - dec!(0.50),
                    high: close + dec!(1.00),
                    low: close - dec!(1.00),
                    close,
                    volume: if i == 54 {
                        volume_on_cross
                    } else {
                        dec!(2000000)
                    },
                }
            })
            .collect()
    }

    #[test]
    fn adx_is_high_in_a_steady_trend_and_low_in_chop() {
        let closes: Vec<Decimal> = (0..40).map(|i| dec!(100) + Decimal::from(i)).collect();
        let highs: Vec<Decimal> = closes.iter().map(|c| c + dec!(1)).collect();
        let lows: Vec<Decimal> = closes.iter().map(|c| c - dec!(1)).collect();
        let trending = calculate_adx(&highs, &lows, &closes, 14).unwrap();
        assert!(trending > dec!(90), "trend ADX was {trending}");

        let closes: Vec<Decimal> = (0..40)
            .map(|i| if i % 2 == 0 { dec!(100) } else { dec!(102) })
            .collect();
        let highs: Vec<Decimal> = closes.iter().map(|c| c + dec!(1)).collect();
        let lows: Vec<Decimal> = closes.iter().map(|c| c - dec!(1)).collect();
        let choppy = calculate_adx(&highs, &lows, &closes, 14).unwrap();
        assert!(choppy < dec!(20), "chop ADX was {choppy}");

        assert_eq!(
            calculate_adx(&highs[..28], &lows[..28], &closes[..28], 14),
            None
        );
    }

    #[test]
    fn golden_cross_needs_volume_and_trend_confirmation() {
        let instrument = test_equity("AAPL");
        let mut config = SmaCrossoverConfig::default();
        config.entry_filters.min_volume_ratio = dec!(1.2);

        let quiet = golden_cross_candles(dec!(2000000));
        let now = quiet.last().unwrap().ts + Duration::hours(2);
        let signal = evaluate_sma_crossover(&instrument, &quiet, now, &config).unwrap();
        assert_eq!(signal.action, SmaSignalAction::Hold);
        assert!(signal.reason.contains("Golden cross rejected"));
        assert!(signal.reason.contains("volume"), "{}", signal.reason);

        // Volume confirms, but 54 flat bars are no trend.
        let loud = golden_cross_candles(dec!(5000000));
        let signal = evaluate_sma_crossover(&instrument, &loud, now, &config).unwrap();
        assert_eq!(signal.action, SmaSignalAction::Hold);
        assert!(signal.reason.contains("ADX"), "{}", signal.reason);

        let mut no_adx = config.clone();
        no_adx.entry_filters.min_adx = Decimal::ZERO;
        let signal = evaluate_sma_crossover(&instrument, &loud, now, &no_adx).unwrap();
        assert_eq!(signal.action, SmaSignalAction::LongEntry);

        let quote = Quote {
            instrument: instrument.id.clone(),
            bid: dec!(149.95),
            ask: dec!(150.05),
            mid: dec!(150.00),
            last: Some(dec!(150.00)),
            ts: now,
            book: None,
        };
        let view = generate_sma_crossover_view(&instrument, &quote, &quiet, now, &config);
        assert_eq!(view.direction, Direction::Flat);
    }

    #[test]
    fn regime_blocks_longs_below_the_long_average_and_on_missing_history() {
        let rising: Vec<Decimal> = (0..210).map(|i| dec!(100) + Decimal::from(i)).collect();
        let falling: Vec<Decimal> = (0..210).map(|i| dec!(400) - Decimal::from(i)).collect();
        assert_eq!(regime_rejection(&rising, 200), None);
        assert!(regime_rejection(&falling, 200).is_some());
        assert!(regime_rejection(&rising[..150], 200)
            .unwrap()
            .contains("needs 200"));
        assert_eq!(regime_rejection(&falling, 0), None);
    }

    #[test]
    fn managed_stop_moves_to_breakeven_then_trails_and_never_loosens() {
        let rules = SmaTrailingStop::default();
        let stop = |high: Decimal, atr: Option<Decimal>| {
            managed_stop(dec!(100), dec!(95), high, atr, &rules)
        };
        // Entry 100, initial stop 95: 1R is 5.
        assert_eq!(stop(dec!(104.99), Some(dec!(1))), dec!(95));
        // +1R reached: at least breakeven, and the 3-ATR trail when higher.
        assert_eq!(stop(dec!(105), Some(dec!(3))), dec!(100));
        assert_eq!(stop(dec!(105), Some(dec!(1))), dec!(102));
        assert_eq!(stop(dec!(130), Some(dec!(2))), dec!(124));
        assert_eq!(stop(dec!(105), None), dec!(100));
        assert_eq!(
            managed_stop(
                dec!(100),
                dec!(95),
                dec!(130),
                Some(dec!(2)),
                &SmaTrailingStop::disabled()
            ),
            dec!(95)
        );
        // A stop at or above entry has no defined R and is left alone.
        assert_eq!(
            managed_stop(dec!(100), dec!(100), dec!(130), Some(dec!(2)), &rules),
            dec!(100)
        );
    }

    /// AAPL is flat, golden-crosses on day 255, then holds the new level.
    fn late_cross_candles() -> Vec<UsCandle> {
        (0..262)
            .map(|day| {
                let close = if day < 255 { dec!(100) } else { dec!(150) };
                us_bar(day, "AAPL", close, dec!(1000000))
            })
            .collect()
    }

    #[test]
    fn sma_backtest_only_enters_while_the_regime_is_bullish() {
        let mut config = sma_backtest_config();
        config.sma_entry_filters = SmaEntryFilters {
            regime_sma_period: 200,
            ..SmaEntryFilters::disabled()
        };
        let candles = late_cross_candles();

        let bull: Vec<UsCandle> = (0..262)
            .map(|day| us_bar(day, "SPY", dec!(300) + Decimal::from(day), dec!(1000000)))
            .collect();
        let results =
            run_us_sma_crossover_backtest_with_regime(&candles, Some(&bull), &config).unwrap();
        assert_eq!(results.total_trades, 1);
        assert!(results.trades.iter().all(|t| t.symbol == "AAPL"));

        let bear: Vec<UsCandle> = (0..262)
            .map(|day| us_bar(day, "SPY", dec!(600) - Decimal::from(day), dec!(1000000)))
            .collect();
        let results =
            run_us_sma_crossover_backtest_with_regime(&candles, Some(&bear), &config).unwrap();
        assert_eq!(results.total_trades, 0);
    }

    #[test]
    fn sma_backtest_refuses_a_regime_filter_without_spy() {
        let mut config = sma_backtest_config();
        config.sma_entry_filters = SmaEntryFilters::default();
        let err = run_us_sma_crossover_backtest(&late_cross_candles(), &config).unwrap_err();
        assert!(err.to_string().contains("SPY"), "{err}");
    }

    #[test]
    fn sma_backtest_trailing_stop_locks_in_a_winner() {
        // Cross on day 54, fill day 55 at 150, run to 200 by day 80, then
        // fall 5 a day. The bare strategy rides the fall; the trail exits.
        let candles: Vec<UsCandle> = (0..100)
            .map(|day| {
                let close = match day {
                    0..=53 => dec!(100),
                    54..=55 => dec!(150),
                    56..=80 => dec!(150) + Decimal::from(day - 55) * dec!(2),
                    _ => dec!(200) - Decimal::from(day - 80) * dec!(5),
                };
                us_bar(day, "AAPL", close, dec!(1000000))
            })
            .collect();

        let mut config = sma_backtest_config();
        config.sma_trailing_stop = SmaTrailingStop::default();
        let trailed = run_us_sma_crossover_backtest(&candles, &config).unwrap();
        assert_eq!(trailed.total_trades, 1);
        assert_eq!(trailed.trades[0].exit_reason, "TRAILING_STOP");
        let trailed_pnl = trailed.trades[0].net_pnl;
        assert!(trailed_pnl > Decimal::ZERO);

        let bare = run_us_sma_crossover_backtest(&candles, &sma_backtest_config()).unwrap();
        assert!(trailed_pnl > bare.trades[0].net_pnl);
    }
}
