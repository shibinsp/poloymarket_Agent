//! Post-mortems of closed strategy trades: step one of learning from mistakes.
//!
//! Every closed trade, India or US, is measured against the price path it
//! lived through and filed under one label:
//!
//! | label            | what happened                                          |
//! |------------------|--------------------------------------------------------|
//! | WIN              | closed with a profit                                   |
//! | COSTS            | the price moved our way, but charges made it a loss    |
//! | GAVE_BACK        | was at least 1R up at some point, closed at a loss     |
//! | FALSE_BREAKOUT   | stopped out within 15 minutes of entry                 |
//! | FOUGHT_MARKET    | the index moved 0.3% or more against the trade         |
//! | STOPPED_LATER    | stopped out after the first 15 minutes                 |
//! | FADED            | never hit the stop, closed at the time exit at a loss  |
//!
//! The first match wins, in that order, so every loser has exactly one label.
//! R is the planned risk: the distance from entry to the stop. Nothing here
//! changes a strategy: it only records what went wrong, for a person (and,
//! later, for a backtest of a candidate fix) to act on.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::db::store::{ClosedTrade, Store, TradeReviewRow};
use crate::india::desk::NIFTY50;
use crate::india::momentum::NIFTY_50_KEY;
use crate::india::orb::UpstoxBars;
use crate::us::orb::{AlpacaBars, Bar, BURST_STRATEGY, ORB_STRATEGY};

const FAST_STOP_MINUTES: i64 = 15;
const GIVE_BACK_R: Decimal = dec!(1);
/// Index move against the trade, in percent, that counts as fighting it.
const FOUGHT_INDEX_PCT: Decimal = dec!(0.3);
/// Alpaca's free plan serves SIP bars only once they are 15 minutes old.
const US_SETTLE_MINUTES: i64 = 20;

/// The price path's verdict on one trade. `bars` are the stock's bars and
/// `index` the index's, each `bar_minutes` long and stamped at their start;
/// `tz` gives the session date.
pub fn assess(
    t: &ClosedTrade,
    bars: &[Bar],
    index: &[Bar],
    bar_minutes: i64,
    tz: Tz,
) -> TradeReviewRow {
    let len = chrono::Duration::minutes(bar_minutes);
    let during = |b: &&Bar| b.ts + len > t.entry_at && b.ts < t.exit_at;
    let path: Vec<&Bar> = bars.iter().filter(during).collect();
    let risk = (t.entry_price - t.stop_price).abs();

    let (mfe_r, mae_r) = if risk > Decimal::ZERO && !path.is_empty() {
        let high = path.iter().map(|b| b.high).max().unwrap_or(t.entry_price);
        let low = path.iter().map(|b| b.low).min().unwrap_or(t.entry_price);
        let (best, worst) = if t.is_buy {
            (high - t.entry_price, t.entry_price - low)
        } else {
            (t.entry_price - low, high - t.entry_price)
        };
        let r = |x: Decimal| Some((x.max(Decimal::ZERO) / risk).round_dp(2));
        (r(best), r(worst))
    } else {
        (None, None)
    };

    let idx: Vec<&Bar> = index.iter().filter(during).collect();
    let index_move_pct = match (idx.first(), idx.last()) {
        (Some(a), Some(b)) if a.close > Decimal::ZERO => {
            let raw = (b.close / a.close - Decimal::ONE) * dec!(100);
            Some(if t.is_buy { raw } else { -raw }.round_dp(2))
        }
        _ => None,
    };

    let minutes_held = (t.exit_at - t.entry_at).num_minutes();
    let (label, lesson) = label(t, minutes_held, mfe_r, index_move_pct);
    TradeReviewRow {
        day: t.entry_at.with_timezone(&tz).date_naive().to_string(),
        minutes_held,
        mfe_r,
        mae_r,
        index_move_pct,
        label: label.to_string(),
        lesson,
    }
}

fn label(
    t: &ClosedTrade,
    minutes: i64,
    mfe_r: Option<Decimal>,
    index_pct: Option<Decimal>,
) -> (&'static str, String) {
    let stopped = t
        .exit_reason
        .as_deref()
        .is_some_and(|r| r.to_ascii_uppercase().contains("STOP"));
    let best = mfe_r
        .map(|r| format!("{r}R"))
        .unwrap_or_else(|| "an unknown amount".into());
    let gross = t.pnl + t.charges.unwrap_or_default();
    if t.pnl > Decimal::ZERO {
        return ("WIN", format!("Worked: it went as far as {best} in our favour."));
    }
    if t.charges.is_some() && gross >= Decimal::ZERO {
        return (
            "COSTS",
            format!(
                "The price moved our way, but charges of {} were more than the {} it made. Fix to test: skip moves too small to pay for the round trip.",
                t.charges.unwrap_or_default().round_dp(2),
                gross.round_dp(2)
            ),
        );
    }
    if mfe_r.is_some_and(|r| r >= GIVE_BACK_R) {
        return (
            "GAVE_BACK",
            format!("Was {best} up, then closed at a loss. Fix to test: move the stop to the entry price after +1R, or trail it."),
        );
    }
    if stopped && minutes <= FAST_STOP_MINUTES {
        return (
            "FALSE_BREAKOUT",
            format!("Stopped out after {minutes} minutes: the breakout failed at once (best {best}). Fix to test: wait for a second close beyond the level, or demand more volume."),
        );
    }
    if let Some(p) = index_pct.filter(|p| *p <= -FOUGHT_INDEX_PCT) {
        return (
            "FOUGHT_MARKET",
            format!("The index moved {}% against the trade while it was open. Fix to test: a stronger market-direction filter.", p.abs()),
        );
    }
    if stopped {
        return (
            "STOPPED_LATER",
            format!("Held {minutes} minutes, reached {best}, then hit the stop. Fix to test: a time stop, or a tighter stop once the move stalls."),
        );
    }
    (
        "FADED",
        format!("Never hit the stop, but drifted to a loss by the time exit (best {best}). Fix to test: a time stop that cuts trades going nowhere."),
    )
}

/// Review every closed trade that has no post-mortem yet. Trades whose bars
/// are not available yet are left for the next pass.
pub async fn review_pending(
    store: &Store,
    upstox: &UpstoxBars,
    alpaca: Option<&AlpacaBars>,
    now: DateTime<Utc>,
) -> Result<usize> {
    let mut done = 0;
    let ist = chrono_tz::Asia::Kolkata;
    let today_ist = now.with_timezone(&ist).date_naive();
    for t in store.unreviewed_intraday_trades().await? {
        let Some(key) = NIFTY50.iter().find(|(s, _)| *s == t.symbol).map(|(_, k)| *k) else {
            continue;
        };
        let day = t.entry_at.with_timezone(&ist).date_naive();
        let (stock, nifty) = if day == today_ist {
            (upstox.today(key, 1).await, upstox.today(NIFTY_50_KEY, 1).await)
        } else {
            (
                upstox.history(key, 1, day, day).await,
                upstox.history(NIFTY_50_KEY, 1, day, day).await,
            )
        };
        let (bars, index) = match (stock, nifty) {
            (Ok(b), Ok(i)) if !b.is_empty() => (b, i),
            (Err(e), _) | (_, Err(e)) => {
                warn!(symbol = %t.symbol, error = %e, "Review: no Upstox bars yet");
                continue;
            }
            _ => continue,
        };
        let review = assess(&t, &bars, &index, 1, ist);
        store.insert_trade_review(&t, &review, now).await?;
        info!(market = "IN", strategy = %t.strategy, symbol = %t.symbol, label = %review.label, "Trade reviewed");
        done += 1;
    }

    if let Some(alpaca) = alpaca {
        let mut pending = store.unreviewed_venue_trades("alpaca", ORB_STRATEGY).await?;
        pending.extend(store.unreviewed_venue_trades("alpaca", BURST_STRATEGY).await?);
        for t in pending {
            if now - t.exit_at < chrono::Duration::minutes(US_SETTLE_MINUTES) {
                continue;
            }
            let symbols = vec![t.symbol.clone(), "SPY".to_string()];
            let five = chrono::Duration::minutes(5);
            let mut bars = match alpaca
                .bars_between(&symbols, t.entry_at - five, Some(t.exit_at + five))
                .await
            {
                Ok(b) => b,
                Err(e) => {
                    warn!(symbol = %t.symbol, error = %e, "Review: no Alpaca bars yet");
                    continue;
                }
            };
            let stock = bars.remove(&t.symbol).unwrap_or_default();
            if stock.is_empty() {
                continue;
            }
            let index = bars.remove("SPY").unwrap_or_default();
            let review = assess(&t, &stock, &index, 5, chrono_tz::America::New_York);
            store.insert_trade_review(&t, &review, now).await?;
            info!(market = "US", strategy = %t.strategy, symbol = %t.symbol, label = %review.label, "Trade reviewed");
            done += 1;
        }
    }
    Ok(done)
}

/// Review closed trades every 10 minutes, India and US.
pub fn spawn_review_worker(
    store: Store,
    upstox_token: Arc<RwLock<Option<String>>>,
    alpaca: Option<AlpacaBars>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let upstox = UpstoxBars::new(upstox_token);
        info!(us = alpaca.is_some(), "Trade review worker started");
        loop {
            match review_pending(&store, &upstox, alpaca.as_ref(), Utc::now()).await {
                Ok(0) => {}
                Ok(n) => info!(reviewed = n, "Trade reviews written"),
                Err(e) => warn!(error = %e, "Trade review pass failed"),
            }
            tokio::time::sleep(Duration::from_secs(600)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, h, m, 0).unwrap()
    }

    fn bar(h: u32, m: u32, hi: Decimal, lo: Decimal, c: Decimal) -> Bar {
        Bar { ts: at(h, m), open: c, high: hi, low: lo, close: c, volume: dec!(100) }
    }

    fn trade(pnl: Decimal, reason: &str, minutes: i64) -> ClosedTrade {
        ClosedTrade {
            market: "IN".into(),
            strategy: "india_orb".into(),
            trade_ref: 1,
            symbol: "TCS".into(),
            is_buy: true,
            entry_price: dec!(100),
            stop_price: dec!(98),
            exit_price: dec!(99),
            entry_at: at(4, 0),
            exit_at: at(4, 0) + chrono::Duration::minutes(minutes),
            exit_reason: Some(reason.into()),
            pnl,
            charges: Some(dec!(3)),
        }
    }

    const IST: Tz = chrono_tz::Asia::Kolkata;

    #[test]
    fn excursions_are_measured_in_r_over_the_trade_only() {
        let t = trade(dec!(-5), "FLAT_1515", 60);
        let bars = vec![
            bar(3, 59, dec!(130), dec!(70), dec!(100)), // before entry: ignored
            bar(4, 10, dec!(103), dec!(99), dec!(101)),
            bar(5, 0, dec!(130), dec!(70), dec!(100)), // at exit: ignored
        ];
        let r = assess(&t, &bars, &[], 1, IST);
        assert_eq!(r.mfe_r, Some(dec!(1.5)));
        assert_eq!(r.mae_r, Some(dec!(0.5)));
        assert_eq!(r.minutes_held, 60);
        assert_eq!(r.label, "GAVE_BACK");
    }

    #[test]
    fn each_loser_gets_the_first_label_that_fits() {
        let flat = vec![bar(4, 1, dec!(100.5), dec!(98), dec!(99))];
        let a = |t: &ClosedTrade, idx: &[Bar]| assess(t, &flat, idx, 1, IST).label;
        assert_eq!(a(&trade(dec!(4), "FLAT_1515", 60), &[]), "WIN");
        assert_eq!(a(&trade(dec!(-1), "FLAT_1515", 60), &[]), "COSTS", "gross +2 before charges of 3");
        assert_eq!(a(&trade(dec!(-10), "STOP", 10), &[]), "FALSE_BREAKOUT");
        assert_eq!(a(&trade(dec!(-10), "STOP", 90), &[]), "STOPPED_LATER");
        assert_eq!(a(&trade(dec!(-10), "FLAT_1515", 90), &[]), "FADED");
        let falling = vec![
            bar(4, 1, dec!(1000), dec!(1000), dec!(1000)),
            bar(4, 30, dec!(995), dec!(995), dec!(995)),
        ];
        assert_eq!(a(&trade(dec!(-10), "STOP", 90), &falling), "FOUGHT_MARKET", "index -0.5% against a long");
    }

    #[test]
    fn a_short_measures_in_its_own_direction() {
        let mut t = trade(dec!(-10), "STOP", 30);
        t.is_buy = false;
        t.stop_price = dec!(102);
        let bars = vec![bar(4, 5, dec!(102), dec!(97), dec!(101))];
        let index = vec![
            bar(4, 1, dec!(1000), dec!(1000), dec!(1000)),
            bar(4, 20, dec!(1010), dec!(1010), dec!(1010)),
        ];
        let r = assess(&t, &bars, &index, 1, IST);
        assert_eq!(r.mfe_r, Some(dec!(1.5)));
        assert_eq!(r.index_move_pct, Some(dec!(-1.00)));
        assert_eq!(r.label, "GAVE_BACK");
    }
}
