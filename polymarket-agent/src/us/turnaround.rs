//! US "Turnaround Tuesday" paper book, added 11 Nov 2026 at the user's yes.
//!
//! The rule (from a reel the user sent; tested before building): if DIA's
//! Friday close is below its 25-day average of closes, buy DIA at Monday's
//! open and sell it at Tuesday's close. Skip the week when Monday or Tuesday
//! is a market holiday. Long only, no stop, no target.
//!
//! Simulated, like the IBS book: $1,000, fractional shares, losses come off the
//! capital and profits are set aside. No Alpaca orders are sent.
//! - Entry: after 9:50 New York on Monday (free SIP bars are 15 minutes late),
//!   at the official 9:30 open from the SIP 5-minute bar, plus 2 bp.
//! - Exit: Tuesday 15:55-15:59 at the last IEX price, minus 2 bp (5 minutes
//!   before the close, as the IBS book does).
//!
//! Evidence (research/pipeline/reel_strats.py, DIA daily Dec 2001 - Nov 2026,
//! 2 bp a side): 416 trades, 56% won, +0.23% a trade, profit factor 1.52,
//! worst fall -13.9%, 16 of 20 research years up, holdout (last 25% of trades,
//! from Sep 2020) +18%, all five neighbours (SMA 20/30, SPY, QQQ, IWM)
//! positive, Monte Carlo 100%. Without its best 3 trades +69% of +94%. The
//! same rule on Wednesday or Thursday loses, so it is Monday-specific.
//! Untested: the 15:55 exit instead of the exact close.

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
use crate::us::orb::{AlpacaBars, Bar};
use crate::us::session::us_market_holidays;

pub const SYMBOL: &str = "DIA";
pub const CAPITAL_USD: Decimal = dec!(1000);
pub const SMA_DAYS: usize = 25;
const SLIPPAGE: Decimal = dec!(0.0002);
/// Monday entry window (New York minutes): 9:51 to 11:00.
const ENTRY_FROM: u32 = 9 * 60 + 51;
const ENTRY_TO: u32 = 11 * 60;
/// Tuesday exit window: 15:55:00-15:58:59.
const EXIT_FROM: u32 = 15 * 60 + 55;
const EXIT_TO: u32 = 15 * 60 + 59;

/// The signal: (Friday close, 25-day average) when Friday closed below the
/// average of the last 25 daily closes up to and including Friday.
/// `daily` holds daily bars oldest first; only days before `monday` count.
pub fn signal(daily: &[Bar], monday: NaiveDate) -> Option<(Decimal, Decimal)> {
    let closes: Vec<Decimal> = daily
        .iter()
        .filter(|b| b.ts.with_timezone(&New_York).date_naive() < monday && b.ts.date_naive() < monday)
        .map(|b| b.close)
        .collect();
    if closes.len() < SMA_DAYS {
        return None;
    }
    let last = &closes[closes.len() - SMA_DAYS..];
    let sma = last.iter().copied().sum::<Decimal>() / Decimal::from(SMA_DAYS as u32);
    let friday = *last.last()?;
    (friday < sma).then_some((friday, sma))
}

/// True when this Monday and the next day are both full trading days.
pub fn tradable_week(monday: NaiveDate, holidays: &[NaiveDate]) -> bool {
    monday.weekday() == Weekday::Mon
        && !holidays.contains(&monday)
        && !holidays.contains(&(monday + chrono::Duration::days(1)))
}

async fn enter(store: &Store, sip: &AlpacaBars, monday: NaiveDate, now: DateTime<Utc>) -> Result<()> {
    if store.turnaround_entered_on(SYMBOL, &monday.to_string()).await? {
        return Ok(());
    }
    let symbols = vec![SYMBOL.to_string()];
    let start = Utc.from_utc_datetime(&(monday - chrono::Duration::days(60)).and_hms_opt(0, 0, 0).context("midnight")?);
    let daily = sip.daily_bars(&symbols, start).await?;
    let Some((friday, sma)) = daily.get(SYMBOL).and_then(|d| signal(d, monday)) else {
        info!(symbol = SYMBOL, "Turnaround Tuesday: Friday closed at or above its 25-day average; no trade this week");
        store.turnaround_mark_skip(SYMBOL, &monday.to_string()).await?;
        return Ok(());
    };
    let open = New_York
        .from_local_datetime(&monday.and_hms_opt(9, 30, 0).context("9:30")?)
        .earliest()
        .context("no 9:30 that day")?
        .with_timezone(&Utc);
    let bars = sip.bars_between(&symbols, open, Some(open + chrono::Duration::minutes(5))).await?;
    let Some(first) = bars.get(SYMBOL).and_then(|b| b.first()) else {
        anyhow::bail!("no 9:30 bar for {SYMBOL} yet");
    };
    let realized = store.turnaround_realized().await?;
    let capital = (CAPITAL_USD + realized).min(CAPITAL_USD);
    let price = (first.open * (Decimal::ONE + SLIPPAGE)).round_dp(4);
    let qty = (capital / price).round_dp(6);
    store.turnaround_open_trade(SYMBOL, qty, &monday.to_string(), price, friday, sma, now).await?;
    info!(symbol = SYMBOL, %friday, sma = %sma.round_dp(2), %price, %qty,
          "Turnaround Tuesday: Friday below its 25-day average; bought at Monday's open (paper, simulated)");
    Ok(())
}

async fn exit(store: &Store, iex: &AlpacaBars, tuesday: NaiveDate, now: DateTime<Utc>) -> Result<()> {
    let open_trades = store.turnaround_open().await?;
    if open_trades.is_empty() {
        return Ok(());
    }
    let open = New_York
        .from_local_datetime(&tuesday.and_hms_opt(9, 30, 0).context("9:30")?)
        .earliest()
        .context("no 9:30 that day")?
        .with_timezone(&Utc);
    let bars = iex.bars(&[SYMBOL.to_string()], open).await?;
    let Some((_, _, last)) = bars.get(SYMBOL).and_then(|b| day_so_far(b, tuesday, now)) else {
        anyhow::bail!("no bars to 15:45 for {SYMBOL} yet");
    };
    let price = (last * (Decimal::ONE - SLIPPAGE)).round_dp(4);
    for (id, _sym, qty, entry, _day) in open_trades {
        let (qty, entry) = (Decimal::from_str_exact(&qty)?, Decimal::from_str_exact(&entry)?);
        let pnl = (price - entry) * qty;
        store.turnaround_close_trade(id, &tuesday.to_string(), price, pnl, now).await?;
        info!(symbol = SYMBOL, %entry, %price, pnl = %pnl.round_dp(2), "Turnaround Tuesday: sold at Tuesday's close (paper, simulated)");
    }
    Ok(())
}

/// Check every 30 seconds. Monday 9:51-11:00: decide and enter. Tuesday 15:55: exit.
pub fn spawn_us_turnaround_worker(store: Store, sip: AlpacaBars, iex: AlpacaBars) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(capital = %CAPITAL_USD, symbol = SYMBOL, "US Turnaround Tuesday book started (paper, simulated)");
        let mut entered: Option<NaiveDate> = None;
        let mut exited: Option<NaiveDate> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let now = Utc::now();
            let ny = now.with_timezone(&New_York);
            let day = ny.date_naive();
            let minute = ny.hour() * 60 + ny.minute();
            let holidays = us_market_holidays();
            if day.weekday() == Weekday::Mon && entered != Some(day) && (ENTRY_FROM..ENTRY_TO).contains(&minute) {
                if !tradable_week(day, &holidays) {
                    entered = Some(day);
                    info!("Turnaround Tuesday: holiday on Monday or Tuesday; no trade this week");
                    continue;
                }
                match enter(&store, &sip, day, now).await {
                    Ok(()) => entered = Some(day),
                    Err(e) => warn!(error = %format!("{e:#}"), "Turnaround Tuesday entry failed; retrying"),
                }
            }
            if day.weekday() == Weekday::Tue && exited != Some(day) && (EXIT_FROM..EXIT_TO).contains(&minute) && !holidays.contains(&day) {
                match exit(&store, &iex, day, now).await {
                    Ok(()) => exited = Some(day),
                    Err(e) => warn!(error = %format!("{e:#}"), "Turnaround Tuesday exit failed; retrying"),
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daily(closes: &[Decimal], last_day: NaiveDate) -> Vec<Bar> {
        let n = closes.len() as i64;
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let d = last_day - chrono::Duration::days(n - 1 - i as i64);
                // Alpaca stamps daily bars at 00:00 or 04:00/05:00 UTC of the session date
                let ts = Utc.from_utc_datetime(&d.and_hms_opt(5, 0, 0).unwrap());
                Bar { ts, open: *c, high: *c, low: *c, close: *c, volume: dec!(1) }
            })
            .collect()
    }

    #[test]
    fn buys_when_friday_is_below_the_25_day_average() {
        let friday = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        let monday = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let mut c = vec![dec!(100); 24];
        c.push(dec!(90));
        let (f, sma) = signal(&daily(&c, friday), monday).unwrap();
        assert_eq!(f, dec!(90));
        assert_eq!(sma, dec!(99.6));
        let mut up = vec![dec!(100); 24];
        up.push(dec!(110));
        assert_eq!(signal(&daily(&up, friday), monday), None);
    }

    #[test]
    fn ignores_monday_itself_and_needs_25_days() {
        let monday = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let mut c = vec![dec!(100); 24];
        c.push(dec!(90)); // dated Monday: must not count
        assert_eq!(signal(&daily(&c, monday), monday), None);
        let short = daily(&[dec!(90); 10], monday - chrono::Duration::days(3));
        assert_eq!(signal(&short, monday), None);
    }

    #[test]
    fn skips_holiday_weeks() {
        let monday = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        assert!(tradable_week(monday, &[]));
        assert!(!tradable_week(monday, &[monday]));
        assert!(!tradable_week(monday, &[monday + chrono::Duration::days(1)]));
        assert!(!tradable_week(monday + chrono::Duration::days(1), &[]));
    }
}
