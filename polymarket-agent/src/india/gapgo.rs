//! NSE "gap and go" on a paper book of its own, long and short, intraday.
//!
//! Rules, on 5-minute bars of NIFTY 50 stocks:
//! 1. The stock opens at least 2% away from yesterday's close, in the same
//!    direction as NIFTY's own gap.
//! 2. Its 9:15 bar closes with the gap (above its open after a gap up, below
//!    after a gap down): the gap is still being bought or sold.
//! 3. At 9:20 take the one stock with the biggest gap, in its direction. A
//!    missed 9:20 bar is a missed day.
//! 4. Stop at the far side of the 9:15 bar, flat at 15:15 whatever happens.
//! 5. One trade a day with all the capital: whole shares, no leverage, profits
//!    never reinvested.
//!
//! A test, not a proven edge. On 3 Nov 2026 six third-book ideas were tried on
//! NIFTY 50, May - Oct 2026, Rs 10,000: all lost after costs but this one,
//! which was flat (+Rs 12 over 55 trades at 3 bp slippage; -Rs 691 at 10 bp;
//! -Rs 430 then +Rs 443 by half; drawdown Rs 1,344). The user chose to paper
//! it anyway alongside the ORB and the burst. Paper only.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::agent::kill_switch::KillSwitch;
use crate::db::store::Store;
use crate::india::desk::NIFTY50;
use crate::india::market_feed::UpstoxLiveMarketFeed;
use crate::india::momentum::NIFTY_50_KEY;
use crate::india::orb::{indexed, manage_open, nse_slot, trading_cash, whole_shares, UpstoxBars};
use crate::india::session::detect_nse_session;
use crate::us::orb::Bar;

pub const STRATEGY: &str = "india_gapgo";
/// The 9:20 bar: the only one an entry may fill in.
const ENTRY_SLOT: usize = 1;
/// The smallest gap traded. 1.5% until 6 Nov 2026; on May-Oct 2026 the 1.5-2%
/// gaps lost: 2% made +Rs 810 on 35 trades (both halves up, 5 of 6 months,
/// +Rs 353 at 10 bp) against +Rs 12 on 55, and 1.75-2.25% all held up.
const GAP_MIN: Decimal = dec!(0.02);
const SLIPPAGE: Decimal = dec!(0.0003);

/// A stock that gapped and kept going in its first bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Gap {
    pub symbol: String,
    pub key: String,
    pub buy: bool,
    pub high: Decimal,
    pub low: Decimal,
    pub gap: Decimal,
}

/// Whether a stock gapped and went: (long?, gap), or None. The gap must be at
/// least 2% and the 9:15 bar must close in its direction.
pub fn gap_side(prev_close: Decimal, open: Decimal, close: Decimal) -> Option<(bool, Decimal)> {
    if prev_close <= Decimal::ZERO || open <= Decimal::ZERO {
        return None;
    }
    let gap = open / prev_close - Decimal::ONE;
    if gap.abs() < GAP_MIN {
        return None;
    }
    let up = gap > Decimal::ZERO;
    ((close > open) == up && close != open).then_some((up, gap))
}

/// The day's pick: the biggest gap in NIFTY's direction.
pub fn pick(gaps: Vec<Gap>, nifty_gap_up: bool) -> Option<Gap> {
    gaps.into_iter()
        .filter(|g| g.buy == nifty_gap_up)
        .max_by(|a, b| a.gap.abs().cmp(&b.gap.abs()))
}

/// The last close of a session before `day`.
pub fn prior_close(bars: &[Bar], day: NaiveDate) -> Option<Decimal> {
    bars.iter()
        .filter(|b| nse_slot(b.ts).is_some_and(|(d, _)| d < day))
        .max_by_key(|b| b.ts)
        .map(|b| b.close)
}

async fn previous_close(data: &UpstoxBars, key: &str, day: NaiveDate) -> Result<Option<Decimal>> {
    // A week back covers a long weekend plus a holiday.
    let hist = data
        .history(
            key,
            5,
            day - chrono::Duration::days(7),
            day - chrono::Duration::days(1),
        )
        .await?;
    Ok(prior_close(&hist, day))
}

/// ATLAS: every NIFTY 50 stock that gapped and went.
async fn scan(data: &UpstoxBars, day: NaiveDate) -> Vec<Gap> {
    let mut out = Vec::new();
    for (symbol, key) in NIFTY50 {
        let (prev, today) = match (previous_close(data, key, day).await, data.today(key, 5).await) {
            (Ok(Some(p)), Ok(t)) => (p, t),
            (Ok(None), _) => continue,
            (Err(e), _) | (_, Err(e)) => {
                warn!(symbol, error = %e, "Gap scan could not read bars; left out today");
                continue;
            }
        };
        let Some((_, first)) = indexed(&today, day).into_iter().find(|(i, _)| *i == 0) else {
            continue;
        };
        if let Some((buy, gap)) = gap_side(prev, first.open, first.close) {
            out.push(Gap {
                symbol: symbol.to_string(),
                key: key.to_string(),
                buy,
                high: first.high,
                low: first.low,
                gap: gap.round_dp(4),
            });
        }
    }
    out
}

/// Run the gap-and-go book; checks every 30 seconds in the session.
pub fn spawn_india_gapgo_worker(
    feed: Arc<UpstoxLiveMarketFeed>,
    store: Store,
    kill_switch: Arc<KillSwitch>,
    capital: Decimal,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(%capital, "NSE gap-and-go worker started (paper only, no leverage)");
        let data = UpstoxBars::new(feed.access_token.clone());
        let mut decided: Option<NaiveDate> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let now = Utc::now();
            if let Err(e) = manage_open(&data, &store, STRATEGY, now).await {
                warn!(error = %e, "Could not manage the open gap-and-go position");
            }
            crate::india::ladder::manage_all(&data, &store, STRATEGY, now).await;
            if kill_switch.is_tripped() || !detect_nse_session(now).allows_order_entry {
                continue;
            }
            let Some((today, slot)) = nse_slot(now) else {
                continue;
            };
            if slot != ENTRY_SLOT || decided == Some(today) {
                continue;
            }
            match look_for_entry(&data, &store, capital, today, now).await {
                Ok(()) => decided = Some(today),
                Err(e) => warn!(error = %e, "Gap-and-go entry pass failed; retrying"),
            }
        }
    })
}

async fn look_for_entry(
    data: &UpstoxBars,
    store: &Store,
    capital: Decimal,
    today: NaiveDate,
    now: DateTime<Utc>,
) -> Result<()> {
    let (trades_today, _, _) = store.intraday_totals(STRATEGY, &today.to_string()).await?;
    // Only losses come off the capital; profits are set aside, never netted.
    let losses = store.intraday_losses(STRATEGY).await?;
    if trades_today > 0 || !store.open_intraday_trades(STRATEGY).await?.is_empty() {
        return Ok(());
    }
    let nifty = data.today(NIFTY_50_KEY, 5).await?;
    let Some((_, nifty_first)) = indexed(&nifty, today).into_iter().find(|(i, _)| *i == 0) else {
        anyhow::bail!("NIFTY's 9:15 bar is not in yet");
    };
    let Some(nifty_prev) = previous_close(data, NIFTY_50_KEY, today).await? else {
        anyhow::bail!("NIFTY's previous close is not available");
    };
    let nifty_gap_up = nifty_first.open > nifty_prev;
    let gaps = scan(data, today).await;
    info!(gaps = ?gaps.iter().map(|g| (&g.symbol, g.buy, g.gap)).collect::<Vec<_>>(), nifty_gap_up, "ATLAS: NSE gaps that kept going");
    let Some(p) = pick(gaps.clone(), nifty_gap_up) else {
        info!("No gap-and-go in NIFTY's direction today");
        return Ok(());
    };
    // Too late is a missed trade: the scan can take a while.
    if nse_slot(Utc::now()).map(|(_, s)| s) != Some(ENTRY_SLOT) {
        info!(symbol = %p.symbol, "Gap-and-go entry after the 9:20 bar; skipping");
        return Ok(());
    }
    let balance = trading_cash(capital, losses);
    if let Err(e) = enter(data, store, capital, balance, today, now, &p, gaps.len()).await {
        warn!(symbol = %p.symbol, error = %e, "Gap-and-go entry failed");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn enter(
    data: &UpstoxBars,
    store: &Store,
    capital: Decimal,
    balance: Decimal,
    today: NaiveDate,
    now: DateTime<Utc>,
    p: &Gap,
    gaps_today: usize,
) -> Result<()> {
    let one_minute = data.today(&p.key, 1).await?;
    let Some(last) = one_minute.last().map(|b| b.close) else {
        return Ok(());
    };
    let buy = p.buy;
    let stop = if buy { p.low } else { p.high };
    let price = if buy {
        last * (Decimal::ONE + SLIPPAGE)
    } else {
        last * (Decimal::ONE - SLIPPAGE)
    }
    .round_dp(2);

    // TITAN.
    let qty = whole_shares(balance, price, buy);
    let blocked = if (buy && price <= stop) || (!buy && price >= stop) {
        Some(format!("price {price} is already through the stop {stop}"))
    } else if qty < 1 {
        Some(format!(
            "Rs {} cannot buy one share at {price}",
            balance.round_dp(2)
        ))
    } else {
        None
    };


    let action = if blocked.is_some() { "SKIP" } else { "TRADE" };
    let card = serde_json::json!({
        "at": now,
        "market": "IN",
        "strategy": STRATEGY,
        "symbol": p.symbol,
        "atlas": { "gap": p.gap, "range_high": p.high, "range_low": p.low, "gaps_today": gaps_today },
        "vega": { "side": if buy { "LONG" } else { "SHORT" }, "signal_bar": 0, "stop": stop },
        "titan": { "allowed": blocked.is_none(), "reason": blocked.clone().unwrap_or_else(|| "within limits".into()), "qty": qty, "notional_inr": (Decimal::from(qty) * price).round_dp(2), "balance_inr": balance.round_dp(2) },
        "nova": { "action": action },
    });
    store
        .insert_desk_card(now, "IN", STRATEGY, &p.symbol, action, "NONE", &card)
        .await?;
    if blocked.is_some() {
        info!(symbol = %p.symbol, reason = ?blocked, "NOVA (India gap-and-go): skip");
        return Ok(());
    }
    store
        .insert_intraday_trade(
            STRATEGY,
            &today.to_string(),
            &p.symbol,
            buy,
            qty,
            &price.to_string(),
            &stop.to_string(),
            &capital.to_string(),
            now,
        )
        .await?;
    info!(symbol = %p.symbol, buy, qty, %price, %stop, gap = %p.gap, "NSE gap-and-go paper entry; no live order was sent");
    crate::india::ladder::mirror_entry(store, STRATEGY, &today.to_string(), &p.symbol, buy, price, stop, now).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gap(symbol: &str, buy: bool, g: Decimal) -> Gap {
        Gap {
            symbol: symbol.into(),
            key: String::new(),
            buy,
            high: dec!(101),
            low: dec!(99),
            gap: g,
        }
    }

    #[test]
    fn a_gap_needs_two_percent_and_a_first_bar_that_keeps_going() {
        assert_eq!(gap_side(dec!(100), dec!(102), dec!(102.5)), Some((true, dec!(0.02))));
        assert_eq!(gap_side(dec!(100), dec!(101.5), dec!(102)), None, "1.5% is too small");
        assert_eq!(gap_side(dec!(100), dec!(98), dec!(97.5)), Some((false, dec!(-0.02))));
        assert_eq!(gap_side(dec!(100), dec!(101.9), dec!(103)), None, "gap too small");
        assert_eq!(gap_side(dec!(100), dec!(102), dec!(101.5)), None, "gap up being sold");
        assert_eq!(gap_side(dec!(100), dec!(98), dec!(98.5)), None, "gap down being bought");
        assert_eq!(gap_side(dec!(100), dec!(102), dec!(102)), None, "a flat first bar");
    }

    #[test]
    fn the_pick_is_the_biggest_gap_with_nifty() {
        let all = vec![
            gap("A", true, dec!(0.02)),
            gap("B", true, dec!(0.03)),
            gap("C", false, dec!(-0.05)),
        ];
        assert_eq!(pick(all.clone(), true).unwrap().symbol, "B");
        assert_eq!(pick(all, false).unwrap().symbol, "C");
        assert!(pick(vec![gap("A", true, dec!(0.02))], false).is_none());
    }
}
