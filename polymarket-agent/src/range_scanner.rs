//! Range scanner (4 Nov 2026): how high and how low each stock is likely to
//! trade for the rest of today. It says nothing about which way; nothing
//! tested this week could.
//!
//! A stock's typical daily range is the mean of (high - low) / open over its
//! last 14 sessions. Each half hour of the session has a table (`range_model.json`),
//! fitted on May-Oct 2026 1-minute bars of the NIFTY 50 and 91 U.S. stocks
//! (scratchpad `range_scanner_test.py` and `range_model_export.py`): by how
//! close the price is to the day's high (low) so far, the chance of a new high
//! (low) later and the 80th percentile of how far past it the day goes, in
//! typical ranges. Scored on days the fit never saw, the odds matched (66%
//! predicted, 65% seen; 37% vs 36%) and the 80% limits held 80-88% of the time.
//!
//! Display only: no strategy reads it.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::{America::New_York, Asia::Kolkata, Tz};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::india::desk::NIFTY50;
use crate::india::orb::UpstoxBars;
use crate::us::orb::{AlpacaBars, Bar};

const MODEL: &str = include_str!("range_model.json");
/// Sessions averaged for the typical daily range, and the fewest accepted.
const RANGE_SESSIONS: usize = 14;
const MIN_SESSIONS: usize = 10;

#[derive(Debug, Deserialize)]
struct Group {
    p_new_high: f64,
    up80: f64,
    p_new_low: f64,
    down80: f64,
}

#[derive(Debug, Deserialize)]
struct Table {
    /// Minutes after the open this table applies from.
    minute: u32,
    at_it: Group,
    near: Group,
    far: Group,
}

fn model() -> &'static HashMap<String, Vec<Table>> {
    static M: OnceLock<HashMap<String, Vec<Table>>> = OnceLock::new();
    M.get_or_init(|| serde_json::from_str(MODEL).expect("range_model.json is valid"))
}

/// The rest of today, projected from what has traded so far.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    /// 80% of days the high stays at or below this.
    pub likely_high: f64,
    /// 80% of days the low stays at or above this.
    pub likely_low: f64,
    pub p_new_high: f64,
    pub p_new_low: f64,
    /// Today's range so far over the typical daily range.
    pub range_used: f64,
}

fn group(t: &Table, distance: f64) -> &Group {
    if distance < 0.1 {
        &t.at_it
    } else if distance < 0.3 {
        &t.near
    } else {
        &t.far
    }
}

/// `market` "india" or "us"; `minute` after the open; `range` the typical
/// daily range as a fraction. None before the first table or without a range.
pub fn project(
    market: &str,
    minute: u32,
    price: f64,
    high_so_far: f64,
    low_so_far: f64,
    range: f64,
) -> Option<Projection> {
    let table = model()
        .get(market)?
        .iter()
        .rev()
        .find(|t| t.minute <= minute)?;
    let unit = range * price;
    if unit <= 0.0 {
        return None;
    }
    let up = group(table, (high_so_far - price) / unit);
    let down = group(table, (price - low_so_far) / unit);
    Some(Projection {
        likely_high: high_so_far + up.up80 * unit,
        likely_low: low_so_far - down.down80 * unit,
        p_new_high: up.p_new_high,
        p_new_low: down.p_new_low,
        range_used: (high_so_far - low_so_far) / unit,
    })
}

/// Mean (high - low) / open of the last 14 sessions, from daily bars oldest
/// first; None with fewer than 10.
pub fn typical_range(daily: &[(f64, f64, f64)]) -> Option<f64> {
    let recent: Vec<f64> = daily
        .iter()
        .filter(|(o, _, _)| *o > 0.0)
        .map(|(o, h, l)| (h - l) / o)
        .collect();
    let take = &recent[recent.len().saturating_sub(RANGE_SESSIONS)..];
    (take.len() >= MIN_SESSIONS).then(|| take.iter().sum::<f64>() / take.len() as f64)
}

#[derive(Debug, Clone, Serialize)]
pub struct RangeRow {
    pub symbol: String,
    pub price: f64,
    pub high_so_far: f64,
    pub low_so_far: f64,
    pub likely_high: f64,
    pub likely_low: f64,
    pub p_new_high: f64,
    pub p_new_low: f64,
    pub typical_range_pct: f64,
    pub range_used_pct: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MarketScan {
    /// Time of the data the rows were built from.
    pub as_of: Option<DateTime<Utc>>,
    pub note: String,
    pub rows: Vec<RangeRow>,
}

/// The latest scan per market ("india", "us"), read by the dashboard.
pub type RangeScans = Arc<RwLock<HashMap<&'static str, MarketScan>>>;

struct Session {
    tz: Tz,
    open: u32,
    close: u32,
}

const INDIA: Session = Session {
    tz: Kolkata,
    open: 9 * 60 + 15,
    close: 15 * 60 + 30,
};
const US: Session = Session {
    tz: New_York,
    open: 9 * 60 + 30,
    close: 16 * 60,
};

impl Session {
    /// The local date and minutes since the open, on a weekday in session.
    fn minute(&self, t: DateTime<Utc>) -> Option<(NaiveDate, u32)> {
        let local = t.with_timezone(&self.tz);
        if matches!(local.weekday(), Weekday::Sat | Weekday::Sun) {
            return None;
        }
        let m = local.hour() * 60 + local.minute();
        (self.open..self.close)
            .contains(&m)
            .then(|| (local.date_naive(), m - self.open))
    }

    fn open_utc(&self, day: NaiveDate) -> Option<DateTime<Utc>> {
        let t = day.and_hms_opt(self.open / 60, self.open % 60, 0)?;
        Some(
            self.tz
                .from_local_datetime(&t)
                .earliest()?
                .with_timezone(&Utc),
        )
    }
}

fn f(d: rust_decimal::Decimal) -> f64 {
    d.to_f64().unwrap_or(0.0)
}

/// Today's high, low, last price and data time (end of the last bar) from
/// one session's 5-minute bars.
fn so_far(bars: &[Bar], s: &Session, day: NaiveDate) -> Option<(f64, f64, f64, DateTime<Utc>)> {
    let today: Vec<&Bar> = bars
        .iter()
        .filter(|b| s.minute(b.ts).is_some_and(|(d, _)| d == day))
        .collect();
    let last = today.last()?;
    Some((
        today.iter().map(|b| f(b.high)).fold(f64::MIN, f64::max),
        today.iter().map(|b| f(b.low)).fold(f64::MAX, f64::min),
        f(last.close),
        last.ts + Duration::minutes(5),
    ))
}

fn row(
    symbol: &str,
    market: &str,
    s: &Session,
    range: f64,
    hlp: (f64, f64, f64, DateTime<Utc>),
) -> Option<RangeRow> {
    let (high, low, price, at) = hlp;
    let (_, minute) = s.minute(at - Duration::seconds(1))?;
    let p = project(market, minute + 1, price, high, low, range)?;
    let r2 = |x: f64| (x * 100.0).round() / 100.0;
    Some(RangeRow {
        symbol: symbol.to_string(),
        price: r2(price),
        high_so_far: r2(high),
        low_so_far: r2(low),
        likely_high: r2(p.likely_high),
        likely_low: r2(p.likely_low),
        p_new_high: (p.p_new_high * 100.0).round(),
        p_new_low: (p.p_new_low * 100.0).round(),
        typical_range_pct: r2(range * 100.0),
        range_used_pct: (p.range_used * 100.0).round(),
    })
}

/// U.S. bars: `daily` (SIP, full market) for the usual range, `live` for
/// today. On 4 Nov 2026 the user chose IEX for `live`: real time on the free
/// plan, though IEX trades alone can make the high and low a little narrower.
pub struct UsBars {
    pub daily: AlpacaBars,
    pub live: AlpacaBars,
    pub live_note: String,
}

/// Scan both markets every five minutes while each is open; a market's last
/// scan stays up after its close.
pub fn spawn_range_scanner(
    upstox_token: Arc<RwLock<Option<String>>>,
    alpaca: Option<UsBars>,
    us_symbols: Vec<String>,
) -> (RangeScans, JoinHandle<()>) {
    let scans: RangeScans = Arc::default();
    let out = scans.clone();
    let handle = tokio::spawn(async move {
        let upstox = UpstoxBars::new(upstox_token);
        let mut ranges: HashMap<String, (NaiveDate, f64)> = HashMap::new();
        info!(
            us = us_symbols.len(),
            india = NIFTY50.len(),
            "Range scanner started (display only)"
        );
        loop {
            let now = Utc::now();
            if let Some((day, _)) = INDIA.minute(now) {
                let scan = scan_india(&upstox, &mut ranges, day).await;
                out.write().await.insert("india", scan);
            }
            if let (Some(alpaca), Some((day, _))) = (alpaca.as_ref(), US.minute(now)) {
                match scan_us(alpaca, &us_symbols, &mut ranges, day).await {
                    Ok(scan) => {
                        out.write().await.insert("us", scan);
                    }
                    Err(e) => {
                        warn!(error = %format!("{e:#}"), "Range scanner: U.S. bars unavailable")
                    }
                }
            }
            tokio::time::sleep(StdDuration::from_secs(300)).await;
        }
    });
    (scans, handle)
}

async fn scan_india(
    upstox: &UpstoxBars,
    ranges: &mut HashMap<String, (NaiveDate, f64)>,
    day: NaiveDate,
) -> MarketScan {
    let mut rows = Vec::new();
    let mut as_of = None;
    let mut failed = 0;
    for (symbol, key) in NIFTY50 {
        let cache = format!("in:{symbol}");
        if ranges.get(&cache).map_or(true, |(d, _)| *d != day) {
            match upstox
                .daily(key, day - Duration::days(40), day - Duration::days(1))
                .await
            {
                Ok(bars) => {
                    let daily: Vec<_> = bars
                        .iter()
                        .filter(|b| b.ts.with_timezone(&Kolkata).date_naive() < day)
                        .map(|b| (f(b.open), f(b.high), f(b.low)))
                        .collect();
                    if let Some(r) = typical_range(&daily) {
                        ranges.insert(cache.clone(), (day, r));
                    }
                }
                Err(_) => failed += 1,
            }
        }
        let Some(&(_, range)) = ranges.get(&cache) else {
            continue;
        };
        match upstox.today(key, 5).await {
            Ok(bars) => {
                if let Some(hlp) = so_far(&bars, &INDIA, day) {
                    as_of = as_of.max(Some(hlp.3));
                    rows.extend(row(symbol, "india", &INDIA, range, hlp));
                }
            }
            Err(_) => failed += 1,
        }
    }
    let note = if failed > 0 {
        format!("{failed} Upstox requests failed this pass")
    } else {
        String::new()
    };
    MarketScan { as_of, note, rows }
}

async fn scan_us(
    alpaca: &UsBars,
    symbols: &[String],
    ranges: &mut HashMap<String, (NaiveDate, f64)>,
    day: NaiveDate,
) -> anyhow::Result<MarketScan> {
    let missing: Vec<String> = symbols
        .iter()
        .filter(|s| {
            ranges
                .get(&format!("us:{s}"))
                .map_or(true, |(d, _)| *d != day)
        })
        .cloned()
        .collect();
    if !missing.is_empty() {
        let since = US.open_utc(day).unwrap_or_else(Utc::now) - Duration::days(40);
        for (symbol, bars) in alpaca.daily.daily_bars(&missing, since).await? {
            let daily: Vec<_> = bars
                .iter()
                .filter(|b| b.ts.with_timezone(&New_York).date_naive() < day)
                .map(|b| (f(b.open), f(b.high), f(b.low)))
                .collect();
            if let Some(r) = typical_range(&daily) {
                ranges.insert(format!("us:{symbol}"), (day, r));
            }
        }
    }
    let open = US.open_utc(day).unwrap_or_else(Utc::now);
    let bars = alpaca.live.bars(symbols, open).await?;
    let mut rows = Vec::new();
    let mut as_of = None;
    for symbol in symbols {
        let (Some(&(_, range)), Some(list)) =
            (ranges.get(&format!("us:{symbol}")), bars.get(symbol))
        else {
            continue;
        };
        if let Some(hlp) = so_far(list, &US, day) {
            as_of = as_of.max(Some(hlp.3));
            rows.extend(row(symbol, "us", &US, range, hlp));
        }
    }
    Ok(MarketScan {
        as_of,
        note: alpaca.live_note.clone(),
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_has_both_markets_from_the_first_half_hour() {
        assert_eq!(model()["india"][0].minute, 15);
        assert_eq!(model()["us"][0].minute, 15);
        assert!(
            project("india", 10, 100.0, 101.0, 99.0, 0.02).is_none(),
            "no table before 9:30"
        );
    }

    #[test]
    fn projects_past_the_high_and_low_so_far() {
        // 10:30 IST, price at the day's high, 2% typical range.
        let p = project("india", 75, 100.0, 100.0, 98.5, 0.02).unwrap();
        assert!(p.likely_high > 100.0 && p.likely_low < 98.5);
        assert!(p.p_new_high > 0.7, "at the high, a new high is likely");
        assert!(
            p.p_new_low < p.p_new_high,
            "far above the low, a new low is less likely"
        );
        assert!((p.range_used - 0.75).abs() < 1e-9);
        // Late in the day the band is narrower.
        let late = project("india", 345, 100.0, 100.0, 98.5, 0.02).unwrap();
        assert!(late.likely_high - 100.0 < p.likely_high - 100.0);
    }

    #[test]
    fn typical_range_needs_ten_sessions_and_uses_the_last_fourteen() {
        let days: Vec<(f64, f64, f64)> = (0..9).map(|_| (100.0, 101.0, 99.0)).collect();
        assert_eq!(typical_range(&days), None);
        let mut days: Vec<(f64, f64, f64)> = (0..10).map(|_| (100.0, 110.0, 90.0)).collect();
        days.extend((0..14).map(|_| (100.0, 101.0, 99.0)));
        assert!((typical_range(&days).unwrap() - 0.02).abs() < 1e-12);
    }

    #[test]
    fn session_minutes_follow_each_market() {
        // 10:00 IST on Wed 4 Nov 2026 = 04:30 UTC.
        let t = Utc.with_ymd_and_hms(2026, 11, 4, 4, 30, 0).unwrap();
        assert_eq!(INDIA.minute(t).map(|x| x.1), Some(45));
        assert_eq!(US.minute(t), None);
        // 10:00 New York = 15:00 UTC in November.
        let t = Utc.with_ymd_and_hms(2026, 11, 4, 15, 0, 0).unwrap();
        assert_eq!(US.minute(t).map(|x| x.1), Some(30));
    }
}
