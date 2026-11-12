//! US pre-FOMC drift paper book, added 11 Nov 2026 at the user's yes.
//!
//! The rule: buy SPY at the close of the trading day before a Fed rate
//! decision, sell it at the decision day's close (the statement comes out at
//! 14:00 New York, so the hold covers the run-up and the reaction).
//!
//! Simulated, like the IBS and Turnaround Tuesday books: $1,000, fractional
//! shares, losses come off the capital and profits are set aside, no Alpaca
//! orders. Both trades are done at 15:55-15:59 New York at the last IEX price,
//! with 2 bp slippage (5 minutes before the close, as the IBS book does).
//!
//! Evidence (research/pipeline/ideas/fomc_and_india_turnaround.py, SPY daily
//! 2002-2026, 200 meetings, 2 bp a side): 55% won, +0.26% a trade, profit
//! factor 2.14, 16 of 19 research years up, QQQ/DIA/IWM and a 2-day entry all
//! positive, Monte Carlo 100%; without its best 3 trades +38% of +52%. The
//! same hold a week later loses (-0.10% a trade), so it is Fed-specific.
//! Weak spot: since about 2020 (last 50 meetings) only +2.6% in all; the
//! effect was published in 2015 and has faded. About 8 trades a year.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::America::New_York;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::db::store::Store;
use crate::us::ibs::day_so_far;
use crate::us::orb::AlpacaBars;
use crate::us::session::us_market_holidays;

pub const SYMBOL: &str = "SPY";
pub const CAPITAL_USD: Decimal = dec!(1000);
const SLIPPAGE: Decimal = dec!(0.0002);
const DECIDE_FROM: u32 = 15 * 60 + 55;
const DECIDE_TO: u32 = 15 * 60 + 59;

/// Fed rate-decision days (second day of each FOMC meeting), from
/// federalreserve.gov/monetarypolicy/fomccalendars.htm, checked 11 Nov 2026.
/// The 2027 dates are tentative until each previous meeting confirms them,
/// and on 28 Oct 2026 the chair said one 2027 meeting will discuss strategy
/// only, with no rate vote: re-check this list against the Fed calendar.
pub const DECISION_DAYS: [&str; 10] = [
    "2026-12-09", "2027-01-27", "2027-03-17", "2027-04-28", "2027-06-09",
    "2027-07-28", "2027-09-15", "2027-10-27", "2027-12-08", "2028-01-26",
];

pub fn decision_days() -> Vec<NaiveDate> {
    DECISION_DAYS.iter().filter_map(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).collect()
}

/// The trading day before `d` (skipping weekends and market holidays).
pub fn previous_trading_day(d: NaiveDate, holidays: &[NaiveDate]) -> NaiveDate {
    let mut x = d - chrono::Duration::days(1);
    while matches!(x.weekday(), Weekday::Sat | Weekday::Sun) || holidays.contains(&x) {
        x -= chrono::Duration::days(1);
    }
    x
}

/// What to do today: Some((fomc_day, true)) = buy for that meeting,
/// Some((fomc_day, false)) = sell on that meeting day.
pub fn today_action(today: NaiveDate, holidays: &[NaiveDate]) -> Option<(NaiveDate, bool)> {
    for f in decision_days() {
        if f == today {
            return Some((f, false));
        }
        if previous_trading_day(f, holidays) == today {
            return Some((f, true));
        }
    }
    None
}

async fn last_price(iex: &AlpacaBars, day: NaiveDate, now: DateTime<Utc>) -> Result<Decimal> {
    let open = New_York
        .from_local_datetime(&day.and_hms_opt(9, 30, 0).context("9:30")?)
        .earliest()
        .context("no 9:30 that day")?
        .with_timezone(&Utc);
    let bars = iex.bars(&[SYMBOL.to_string()], open).await?;
    bars.get(SYMBOL)
        .and_then(|b| day_so_far(b, day, now))
        .map(|(_, _, last)| last)
        .context("no bars to 15:45 yet")
}

async fn act(store: &Store, iex: &AlpacaBars, today: NaiveDate, fomc: NaiveDate, buy: bool, now: DateTime<Utc>) -> Result<()> {
    let last = last_price(iex, today, now).await?;
    if buy {
        if store.prefomc_has(SYMBOL, &fomc.to_string()).await? {
            return Ok(());
        }
        let realized = store.prefomc_realized().await?;
        let capital = (CAPITAL_USD + realized).min(CAPITAL_USD);
        let price = (last * (Decimal::ONE + SLIPPAGE)).round_dp(4);
        let qty = (capital / price).round_dp(6);
        store.prefomc_open_trade(SYMBOL, qty, &fomc.to_string(), &today.to_string(), price, now).await?;
        info!(symbol = SYMBOL, fomc = %fomc, %price, %qty, "Pre-FOMC book: bought at the close before the Fed decision (paper, simulated)");
    } else {
        let price = (last * (Decimal::ONE - SLIPPAGE)).round_dp(4);
        for (id, qty, entry) in store.prefomc_open().await? {
            let (qty, entry) = (Decimal::from_str_exact(&qty)?, Decimal::from_str_exact(&entry)?);
            let pnl = (price - entry) * qty;
            store.prefomc_close_trade(id, &today.to_string(), price, pnl, now).await?;
            info!(symbol = SYMBOL, %entry, %price, pnl = %pnl.round_dp(2), "Pre-FOMC book: sold at the Fed decision day's close (paper, simulated)");
        }
    }
    Ok(())
}

/// Check every 20 seconds; act once in the 15:55 window on the day before a
/// Fed decision (buy) and on the decision day (sell).
pub fn spawn_us_prefomc_worker(store: Store, iex: AlpacaBars) -> JoinHandle<()> {
    tokio::spawn(async move {
        let next = decision_days().into_iter().find(|d| *d >= Utc::now().with_timezone(&New_York).date_naive());
        info!(capital = %CAPITAL_USD, symbol = SYMBOL, next_decision = ?next, "US pre-FOMC drift book started (paper, simulated)");
        let mut done: Option<NaiveDate> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(20)).await;
            let now = Utc::now();
            let ny = now.with_timezone(&New_York);
            let today = ny.date_naive();
            let minute = ny.hour() * 60 + ny.minute();
            if done == Some(today) || !(DECIDE_FROM..DECIDE_TO).contains(&minute) {
                continue;
            }
            let holidays = us_market_holidays();
            let Some((fomc, buy)) = today_action(today, &holidays) else {
                done = Some(today);
                continue;
            };
            match act(&store, &iex, today, fomc, buy, now).await {
                Ok(()) => done = Some(today),
                Err(e) => warn!(error = %format!("{e:#}"), "Pre-FOMC book pass failed; retrying"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn buys_the_day_before_and_sells_on_the_decision_day() {
        assert_eq!(today_action(d("2026-12-08"), &[]), Some((d("2026-12-09"), true)));
        assert_eq!(today_action(d("2026-12-09"), &[]), Some((d("2026-12-09"), false)));
        assert_eq!(today_action(d("2026-12-10"), &[]), None);
    }

    #[test]
    fn the_day_before_skips_weekends_and_holidays() {
        // 2027-01-27 is a Wednesday; a Tuesday holiday pushes the buy to Monday
        assert_eq!(previous_trading_day(d("2027-01-27"), &[]), d("2027-01-26"));
        assert_eq!(previous_trading_day(d("2027-01-27"), &[d("2027-01-26")]), d("2027-01-25"));
        // a Monday decision would buy on the Friday before
        assert_eq!(previous_trading_day(d("2026-11-16"), &[]), d("2026-11-13"));
    }

    #[test]
    fn decision_days_parse_and_are_ordered() {
        let v = decision_days();
        assert_eq!(v.len(), DECISION_DAYS.len());
        assert!(v.windows(2).all(|w| w[0] < w[1]));
    }
}
