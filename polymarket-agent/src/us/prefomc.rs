//! US pre-FOMC drift paper book, added 11 Nov 2026 at the user's yes.
//!
//! The rule: buy SPY at the close of the trading day before a Fed rate
//! decision, sell it on the decision day at 13:55 New York, just before the
//! 14:00 statement. (Until 12 Nov 2026 it sold at the decision day's close.)
//!
//! Simulated, like the IBS and Turnaround Tuesday books: $1,000, fractional
//! shares, losses come off the capital and profits are set aside, no Alpaca
//! orders. The buy is done at 15:55-15:59 New York on the day before and the
//! sell at 13:55-13:59 on the decision day, each at the last completed IEX
//! 5-minute bar's close, with 2 bp slippage.
//!
//! Evidence (research/pipeline/ideas/fomc_and_india_turnaround.py, SPY daily
//! 2002-2026, 200 meetings, 2 bp a side): 55% won, +0.26% a trade, profit
//! factor 2.14, 16 of 19 research years up, QQQ/DIA/IWM and a 2-day entry all
//! positive, Monte Carlo 100%; without its best 3 trades +38% of +52%. The
//! same hold a week later loses (-0.10% a trade), so it is Fed-specific.
//! Weak spot: since about 2020 (last 50 meetings) only +2.6% in all; the
//! effect was published in 2015 and has faded. About 8 trades a year.
//!
//! Sell time (research/pipeline/ideas/prefomc_intraday.py, QQQ 5-min SIP
//! 2016-2026, 86 meetings): selling at 13:55 won 65% (+0.31% a trade,
//! holdout +7.0%, 8 of 11 years up) against 55% (+0.23%, holdout +1.0%, 6 of
//! 11) for the close. The statement-to-close part alone averaged -0.07% with
//! 1.2% swings: risk without return. The user chose 13:55 on 12 Nov 2026.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::America::New_York;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::db::store::Store;
use crate::us::orb::{AlpacaBars, Bar};
use crate::us::session::us_market_holidays;

pub const SYMBOL: &str = "SPY";
pub const CAPITAL_USD: Decimal = dec!(1000);
const SLIPPAGE: Decimal = dec!(0.0002);
/// Buy window on the day before a decision: 15:55:00-15:58:59 New York.
const BUY_WINDOW: (u32, u32) = (15 * 60 + 55, 15 * 60 + 59);
/// Sell window on the decision day: 13:55:00-13:58:59, before the 14:00 statement.
const SELL_WINDOW: (u32, u32) = (13 * 60 + 55, 13 * 60 + 59);

/// Whether `minute` (New York minutes after midnight) is in the window for a buy or a sell.
pub fn in_window(minute: u32, buy: bool) -> bool {
    let (from, to) = if buy { BUY_WINDOW } else { SELL_WINDOW };
    (from..to).contains(&minute)
}

/// The close of the last 5-minute bar of `day` that has ended by `now`, if
/// that bar ended at `need_end` (New York minutes) or later; otherwise None
/// (the feed is behind, so do not trade on a stale price).
pub fn last_completed_close(
    bars: &[Bar],
    day: NaiveDate,
    now: DateTime<Utc>,
    need_end: u32,
) -> Option<Decimal> {
    let last = bars.iter().rev().find(|b| {
        b.ts.with_timezone(&New_York).date_naive() == day
            && b.ts + chrono::Duration::minutes(5) <= now
    })?;
    let end = (last.ts + chrono::Duration::minutes(5)).with_timezone(&New_York);
    (end.hour() * 60 + end.minute() >= need_end).then_some(last.close)
}

/// Fed rate-decision days (second day of each FOMC meeting), from
/// federalreserve.gov/monetarypolicy/fomccalendars.htm, checked 11 Nov 2026.
/// The 2027 dates are tentative until each previous meeting confirms them,
/// and on 28 Oct 2026 the chair said one 2027 meeting will discuss strategy
/// only, with no rate vote: re-check this list against the Fed calendar.
pub const DECISION_DAYS: [&str; 10] = [
    "2026-12-09",
    "2027-01-27",
    "2027-03-17",
    "2027-04-28",
    "2027-06-09",
    "2027-07-28",
    "2027-09-15",
    "2027-10-27",
    "2027-12-08",
    "2028-01-26",
];

pub fn decision_days() -> Vec<NaiveDate> {
    DECISION_DAYS
        .iter()
        .filter_map(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .collect()
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

async fn last_price(
    iex: &AlpacaBars,
    day: NaiveDate,
    now: DateTime<Utc>,
    buy: bool,
) -> Result<Decimal> {
    let open = New_York
        .from_local_datetime(&day.and_hms_opt(9, 30, 0).context("9:30")?)
        .earliest()
        .context("no 9:30 that day")?
        .with_timezone(&Utc);
    let bars = iex.bars(&[SYMBOL.to_string()], open).await?;
    // The bar ending 5 minutes before the window must be in.
    let need_end = if buy {
        BUY_WINDOW.0 - 5
    } else {
        SELL_WINDOW.0 - 5
    };
    bars.get(SYMBOL)
        .and_then(|b| last_completed_close(b, day, now, need_end))
        .context("the latest 5-minute bar is not in yet")
}

async fn act(
    store: &Store,
    iex: &AlpacaBars,
    today: NaiveDate,
    fomc: NaiveDate,
    buy: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    let last = last_price(iex, today, now, buy).await?;
    if buy {
        if store.prefomc_has(SYMBOL, &fomc.to_string()).await? {
            return Ok(());
        }
        let realized = store.prefomc_realized().await?;
        let capital = (CAPITAL_USD + realized).min(CAPITAL_USD);
        let price = (last * (Decimal::ONE + SLIPPAGE)).round_dp(4);
        let qty = (capital / price).round_dp(6);
        store
            .prefomc_open_trade(
                SYMBOL,
                qty,
                &fomc.to_string(),
                &today.to_string(),
                price,
                now,
            )
            .await?;
        info!(symbol = SYMBOL, fomc = %fomc, %price, %qty, "Pre-FOMC book: bought at the close before the Fed decision (paper, simulated)");
    } else {
        let price = (last * (Decimal::ONE - SLIPPAGE)).round_dp(4);
        for (id, qty, entry) in store.prefomc_open().await? {
            let (qty, entry) = (
                Decimal::from_str_exact(&qty)?,
                Decimal::from_str_exact(&entry)?,
            );
            let pnl = (price - entry) * qty;
            store
                .prefomc_close_trade(id, &today.to_string(), price, pnl, now)
                .await?;
            info!(symbol = SYMBOL, %entry, %price, pnl = %pnl.round_dp(2), "Pre-FOMC book: sold at 13:55 on the Fed decision day, before the statement (paper, simulated)");
        }
    }
    Ok(())
}

/// Check every 20 seconds; buy once in the 15:55 window on the day before a
/// Fed decision and sell once in the 13:55 window on the decision day.
pub fn spawn_us_prefomc_worker(store: Store, iex: AlpacaBars) -> JoinHandle<()> {
    tokio::spawn(async move {
        let next = decision_days()
            .into_iter()
            .find(|d| *d >= Utc::now().with_timezone(&New_York).date_naive());
        info!(capital = %CAPITAL_USD, symbol = SYMBOL, next_decision = ?next, "US pre-FOMC drift book started (paper, simulated)");
        let mut done: Option<NaiveDate> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(20)).await;
            let now = Utc::now();
            let ny = now.with_timezone(&New_York);
            let today = ny.date_naive();
            let minute = ny.hour() * 60 + ny.minute();
            if done == Some(today) {
                continue;
            }
            let holidays = us_market_holidays();
            let Some((fomc, buy)) = today_action(today, &holidays) else {
                done = Some(today);
                continue;
            };
            if !in_window(minute, buy) {
                continue;
            }
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
        assert_eq!(
            today_action(d("2026-12-08"), &[]),
            Some((d("2026-12-09"), true))
        );
        assert_eq!(
            today_action(d("2026-12-09"), &[]),
            Some((d("2026-12-09"), false))
        );
        assert_eq!(today_action(d("2026-12-10"), &[]), None);
    }

    #[test]
    fn the_day_before_skips_weekends_and_holidays() {
        // 2027-01-27 is a Wednesday; a Tuesday holiday pushes the buy to Monday
        assert_eq!(previous_trading_day(d("2027-01-27"), &[]), d("2027-01-26"));
        assert_eq!(
            previous_trading_day(d("2027-01-27"), &[d("2027-01-26")]),
            d("2027-01-25")
        );
        // a Monday decision would buy on the Friday before
        assert_eq!(previous_trading_day(d("2026-11-16"), &[]), d("2026-11-13"));
    }

    #[test]
    fn buys_at_15_55_and_sells_at_13_55() {
        assert!(in_window(15 * 60 + 55, true));
        assert!(!in_window(13 * 60 + 55, true));
        assert!(in_window(13 * 60 + 55, false));
        assert!(in_window(13 * 60 + 58, false));
        assert!(!in_window(13 * 60 + 59, false));
        assert!(
            !in_window(14 * 60, false),
            "never after the 14:00 statement"
        );
        assert!(
            !in_window(15 * 60 + 55, false),
            "no longer sells at the close"
        );
    }

    #[test]
    fn sells_on_the_bar_before_the_statement_and_not_on_a_stale_feed() {
        let day = d("2026-12-09");
        let bar = |h: u32, m: u32, c: Decimal| Bar {
            ts: New_York
                .with_ymd_and_hms(2026, 12, 9, h, m, 0)
                .unwrap()
                .with_timezone(&Utc),
            open: c,
            high: c,
            low: c,
            close: c,
            volume: dec!(1),
        };
        let now = New_York
            .with_ymd_and_hms(2026, 12, 9, 13, 56, 0)
            .unwrap()
            .with_timezone(&Utc);
        let bars = vec![
            bar(13, 40, dec!(600)),
            bar(13, 45, dec!(601)),
            bar(13, 50, dec!(602)),
        ];
        // the 13:50 bar ended at 13:55: its close is the sell price
        assert_eq!(
            last_completed_close(&bars, day, now, 13 * 60 + 50),
            Some(dec!(602))
        );
        // a bar still forming is not used
        let early = New_York
            .with_ymd_and_hms(2026, 12, 9, 13, 54, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            last_completed_close(&bars, day, early, 13 * 60 + 45),
            Some(dec!(601))
        );
        // if the latest bar ended before 13:50 the feed is behind: no price
        assert_eq!(
            last_completed_close(&bars[..1], day, now, 13 * 60 + 50),
            None
        );
    }

    #[test]
    fn decision_days_parse_and_are_ordered() {
        let v = decision_days();
        assert_eq!(v.len(), DECISION_DAYS.len());
        assert!(v.windows(2).all(|w| w[0] < w[1]));
    }
}
