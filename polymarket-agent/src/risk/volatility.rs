//! Volatility measurement from price bars.
//!
//! Continuous assets have no "probability" to feed a binary Kelly formula, so
//! position size is anchored to how far the price typically moves instead.
//! Average True Range is used rather than close-to-close standard deviation
//! because it accounts for gaps and intrabar range — which is what a stop
//! actually has to survive.

use rust_decimal::Decimal;

use crate::venue::types::Candle;

/// True range of one bar given the previous close: the largest of the bar's
/// own range, and the gap up or down from the prior close.
fn true_range(candle: &Candle, prev_close: Option<Decimal>) -> Decimal {
    let high_low = candle.high - candle.low;
    match prev_close {
        Some(prev) => {
            let high_gap = (candle.high - prev).abs();
            let low_gap = (candle.low - prev).abs();
            high_low.max(high_gap).max(low_gap)
        }
        None => high_low,
    }
}

/// Average True Range over the last `period` bars.
///
/// Returns `None` when there are too few bars to measure — the caller must
/// then refuse to size a position rather than guess at volatility.
pub fn atr(candles: &[Candle], period: usize) -> Option<Decimal> {
    if period == 0 || candles.len() < period + 1 {
        return None;
    }

    // Walk the most recent `period` bars, each compared to its predecessor.
    let start = candles.len() - period;
    let mut sum = Decimal::ZERO;
    for i in start..candles.len() {
        sum += true_range(&candles[i], Some(candles[i - 1].close));
    }

    let count = Decimal::from(period as i64);
    let atr = sum / count;
    if atr > Decimal::ZERO {
        Some(atr)
    } else {
        // A perfectly flat series gives no usable risk estimate.
        None
    }
}

/// ATR as a fraction of the latest close, which is the form position sizing
/// needs. `None` if volatility can't be measured or the price is degenerate.
pub fn atr_pct(candles: &[Candle], period: usize) -> Option<Decimal> {
    let atr = atr(candles, period)?;
    let last_close = candles.last()?.close;
    if last_close > Decimal::ZERO {
        Some(atr / last_close)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use rust_decimal_macros::dec;

    fn candle(open: Decimal, high: Decimal, low: Decimal, close: Decimal, i: i64) -> Candle {
        Candle {
            ts: Utc::now() + Duration::hours(i),
            open,
            high,
            low,
            close,
            volume: dec!(1000),
        }
    }

    /// Flat $1-range bars with no gaps: ATR is exactly 1.
    fn steady_series(n: i64) -> Vec<Candle> {
        (0..n)
            .map(|i| candle(dec!(100), dec!(100.5), dec!(99.5), dec!(100), i))
            .collect()
    }

    #[test]
    fn atr_of_uniform_bars_is_the_bar_range() {
        let candles = steady_series(20);
        assert_eq!(atr(&candles, 14), Some(dec!(1.0)));
    }

    #[test]
    fn atr_requires_more_bars_than_the_period() {
        // 14 bars cannot produce a 14-period ATR: each bar needs a predecessor.
        assert_eq!(atr(&steady_series(14), 14), None);
        assert!(atr(&steady_series(15), 14).is_some());
        assert_eq!(atr(&[], 14), None);
        assert_eq!(atr(&steady_series(20), 0), None);
    }

    #[test]
    fn gaps_count_toward_true_range() {
        let mut candles = steady_series(16);
        // A bar that gaps up to 110 has a true range of 10 from the prior
        // close of 100, far larger than its own 1-wide body.
        let last = candles.len() - 1;
        candles[last] = candle(dec!(109.5), dec!(110), dec!(109), dec!(109.5), last as i64);

        let with_gap = atr(&candles, 14).unwrap();
        let without_gap = atr(&steady_series(16), 14).unwrap();
        assert!(
            with_gap > without_gap,
            "gap should raise ATR: {with_gap} vs {without_gap}"
        );
    }

    #[test]
    fn flat_series_has_no_measurable_volatility() {
        // Zero-range bars would imply a zero stop distance and therefore an
        // unbounded position; refuse instead.
        let flat: Vec<Candle> = (0..20)
            .map(|i| candle(dec!(100), dec!(100), dec!(100), dec!(100), i))
            .collect();
        assert_eq!(atr(&flat, 14), None);
        assert_eq!(atr_pct(&flat, 14), None);
    }

    #[test]
    fn atr_pct_is_relative_to_the_latest_close() {
        let candles = steady_series(20);
        // ATR 1.0 on a close of 100.
        assert_eq!(atr_pct(&candles, 14), Some(dec!(0.01)));
    }
}
