//! US "buy a weak close" (IBS) paper book, added 9 Nov 2026 at the user's yes.
//!
//! IBS = (close - low) / (high - low) of the day. At 15:55 New York, for QQQ,
//! SPY and IWM: if a fund sits in the bottom 25% of today's range, buy it (or
//! keep holding it); a fund held from an earlier day whose IBS is not low any
//! more is sold. So each buy is held to the next day's close, rolled while the
//! close stays weak. Overnight holds only: no day trades.
//!
//! The money: $1,000, a third per fund, fractional shares. Losses come off it,
//! profits are set aside (the same as the other books). Fills are simulated at
//! the live IEX price with 2 bp slippage a side; no Alpaca orders are sent, so
//! the shared paper account and its reconciler never see this book.
//!
//! Evidence (scratchpad `us_new25.py`, 25 years of daily bars, Nov 2001 - Nov
//! 2026, 2 bp a side): QQQ +10.1% a year in the market 19% of days, max
//! drawdown -17%, 19 of 24 years up, every nearby threshold positive,
//! walk-forward and 2x costs positive. The three funds a third each: +8.2% a
//! year, drawdown -14%, 20 of 24 years up. Untested: deciding at 15:55 rather
//! than on the exact close (5 minutes early), and early-close days (13:00),
//! when no 15:55 bar exists and the book simply holds.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::America::New_York;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::db::store::Store;
use crate::us::orb::{AlpacaBars, Bar};
use crate::us::session::us_market_holidays;

pub const FUNDS: [&str; 3] = ["QQQ", "SPY", "IWM"];
pub const CAPITAL_USD: Decimal = dec!(1000);
/// Buy below this IBS. 0.25 from 10 Nov 2026 (scratchpad `desk_replace.py`,
/// `thr_check.py`): better than 0.2 in both halves (+$2,158 vs +$1,974 per
/// $1,000 over 24 years), same -14% drawdown, still positive at 3x costs;
/// every setting 0.25-0.5 beats 0.2. Mood filters (SPY trend, VIX), event
/// filters (Fridays, shock days) and volatility sizing all made it worse.
pub const THRESHOLD: Decimal = dec!(0.25);
const SLIPPAGE: Decimal = dec!(0.0002);
/// Decide in 15:55:00-15:58:59 New York.
const DECIDE_FROM_MIN: u32 = 15 * 60 + 55;
const DECIDE_TO_MIN: u32 = 15 * 60 + 59;

/// Internal bar strength of a day so far.
pub fn ibs(high: Decimal, low: Decimal, last: Decimal) -> Option<Decimal> {
    (high > low).then(|| (last - low) / (high - low))
}

/// High, low and last close of today's completed bars, if the latest one
/// ended at 15:45 or later (so an early close, or a feed gap, is no signal).
pub fn day_so_far(bars: &[Bar], day: NaiveDate, now: DateTime<Utc>) -> Option<(Decimal, Decimal, Decimal)> {
    let done: Vec<&Bar> = bars
        .iter()
        .filter(|b| b.ts.with_timezone(&New_York).date_naive() == day)
        .filter(|b| b.ts + chrono::Duration::minutes(5) <= now)
        .collect();
    let last = done.last()?;
    let end = (last.ts + chrono::Duration::minutes(5)).with_timezone(&New_York);
    if end.hour() * 60 + end.minute() < 15 * 60 + 45 {
        return None;
    }
    let high = done.iter().map(|b| b.high).max()?;
    let low = done.iter().map(|b| b.low).min()?;
    Some((high, low, last.close))
}

/// What to do with one fund today.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Buy,
    Hold,
    Sell,
    Nothing,
}

pub fn decide(held: bool, ibs: Decimal) -> Action {
    match (held, ibs < THRESHOLD) {
        (false, true) => Action::Buy,
        (true, true) => Action::Hold,
        (true, false) => Action::Sell,
        (false, false) => Action::Nothing,
    }
}

async fn run_day(store: &Store, data: &AlpacaBars, day: NaiveDate, now: DateTime<Utc>) -> Result<()> {
    let open = New_York
        .from_local_datetime(&day.and_hms_opt(9, 30, 0).context("9:30")?)
        .earliest()
        .context("no 9:30 that day")?
        .with_timezone(&Utc);
    let symbols: Vec<String> = FUNDS.iter().map(|s| s.to_string()).collect();
    let bars = data.bars(&symbols, open).await?;
    let held = store.ibs_open().await?;
    let realized = store.ibs_realized().await?;
    let slot = (CAPITAL_USD + realized).min(CAPITAL_USD) / Decimal::from(FUNDS.len() as u32);
    let today = day.to_string();
    for fund in FUNDS {
        let Some((high, low, last)) = bars.get(fund).and_then(|b| day_so_far(b, day, now)) else {
            warn!(fund, "IBS: no complete bars to 15:45 today (early close or feed gap); holding as is");
            continue;
        };
        let Some(v) = ibs(high, low, last) else { continue };
        let open_trade = held.iter().find(|t| t.1 == fund);
        match decide(open_trade.is_some(), v) {
            Action::Buy => {
                let price = (last * (Decimal::ONE + SLIPPAGE)).round_dp(4);
                let qty = (slot / price).round_dp(6);
                if qty <= Decimal::ZERO {
                    continue;
                }
                store.ibs_open_trade(fund, qty, &today, price, v, now).await?;
                info!(fund, ibs = %v.round_dp(3), %price, %qty, "IBS book: weak close, bought (paper, simulated)");
            }
            Action::Sell => {
                let t = open_trade.expect("held");
                let (qty, entry) = (Decimal::from_str_exact(&t.2)?, Decimal::from_str_exact(&t.3)?);
                let price = (last * (Decimal::ONE - SLIPPAGE)).round_dp(4);
                let pnl = (price - entry) * qty;
                store.ibs_close_trade(t.0, &today, price, v, pnl, now).await?;
                info!(fund, ibs = %v.round_dp(3), %entry, %price, pnl = %pnl.round_dp(2), "IBS book: sold at the close (paper, simulated)");
            }
            Action::Hold => info!(fund, ibs = %v.round_dp(3), "IBS book: close still weak, holding another day"),
            Action::Nothing => info!(fund, ibs = %v.round_dp(3), "IBS book: no signal"),
        }
    }
    Ok(())
}

/// Check every 20 seconds; act once a trading day in the 15:55 window.
pub fn spawn_us_ibs_worker(store: Store, data: AlpacaBars) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(capital = %CAPITAL_USD, funds = ?FUNDS, "US IBS book started (paper, simulated, buy a weak close)");
        let mut done: Option<NaiveDate> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(20)).await;
            let now = Utc::now();
            let ny = now.with_timezone(&New_York);
            let day = ny.date_naive();
            let minute = ny.hour() * 60 + ny.minute();
            if done == Some(day)
                || !(DECIDE_FROM_MIN..DECIDE_TO_MIN).contains(&minute)
                || matches!(ny.format("%a").to_string().as_str(), "Sat" | "Sun")
                || us_market_holidays().contains(&day)
            {
                continue;
            }
            match run_day(&store, &data, day, now).await {
                Ok(()) => done = Some(day),
                Err(e) => warn!(error = %format!("{e:#}"), "IBS book pass failed; retrying"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(h: u32, m: u32, hi: Decimal, lo: Decimal, c: Decimal) -> Bar {
        let ts = New_York
            .with_ymd_and_hms(2026, 11, 9, h, m, 0)
            .unwrap()
            .with_timezone(&Utc);
        Bar { ts, open: c, high: hi, low: lo, close: c, volume: dec!(1) }
    }

    #[test]
    fn ibs_is_where_the_close_sits_in_the_range() {
        assert_eq!(ibs(dec!(110), dec!(100), dec!(101)), Some(dec!(0.1)));
        assert_eq!(ibs(dec!(100), dec!(100), dec!(100)), None);
    }

    #[test]
    fn decisions_roll_while_weak_and_sell_when_not() {
        assert_eq!(decide(false, dec!(0.1)), Action::Buy);
        assert_eq!(decide(true, dec!(0.1)), Action::Hold);
        assert_eq!(decide(true, dec!(0.5)), Action::Sell);
        assert_eq!(decide(false, dec!(0.5)), Action::Nothing);
    }

    #[test]
    fn needs_bars_through_15_45() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let now = New_York.with_ymd_and_hms(2026, 11, 9, 15, 56, 0).unwrap().with_timezone(&Utc);
        let bars = vec![
            bar(9, 30, dec!(110), dec!(105), dec!(108)),
            bar(15, 45, dec!(104), dec!(100), dec!(101)),
            bar(15, 50, dec!(102), dec!(99), dec!(99.5)), // ends 15:55
        ];
        assert_eq!(day_so_far(&bars, day, now), Some((dec!(110), dec!(99), dec!(99.5))));
        let early = New_York.with_ymd_and_hms(2026, 11, 9, 15, 54, 0).unwrap().with_timezone(&Utc);
        assert_eq!(day_so_far(&bars, day, early), Some((dec!(110), dec!(100), dec!(101))));
        assert_eq!(day_so_far(&bars[..1], day, now), None);
    }
}
