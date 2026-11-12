//! NSE "opening burst" on a paper book of its own, long and short, intraday.
//!
//! Rules, on 5-minute bars of NIFTY 50 stocks:
//! 1. The 9:15 bar must move at least 1% from its open to its close on at
//!    least 3x its 14-session average volume, in the same direction as
//!    NIFTY's own 9:15 bar.
//! 2. At 9:20 take the one stock with the highest relative volume, in the
//!    direction of its move. A missed 9:20 bar is a missed day.
//! 3. Stop at the far side of the 9:15 bar, flat at 15:15 whatever happens.
//! 4. One trade a day with all the capital: whole shares, no leverage, profits
//!    never reinvested. Taking the best 3 with a third each was tried on May -
//!    Oct 2026 and earned +1.2% against +10.1%: the second and third picks lose.
//!
//! Chosen on 2 Nov 2026 from 232 variants on 188 stocks (Nov 2024 - Oct 2026):
//! +18.7% then +27.4% on Rs 2,500 at 3 bp slippage, drawdowns of 17-30%. On the
//! NIFTY 50 alone, May - Oct 2026: +9.8% over 42 trades, 7.1% drawdown. It was
//! picked after seeing both periods, so the backtest flatters it: this paper
//! book is the real test. Paper only; no live order is ever sent.

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
use crate::india::orb::{
    indexed, manage_open, nse_slot, prior_range_volumes, trading_cash, whole_shares, UpstoxBars,
};
use crate::india::session::detect_nse_session;
use crate::india::timesfm;
use crate::us::orb::relative_volume;

pub const STRATEGY: &str = "india_burst";
/// The 9:20 bar: the only one an entry may fill in.
const ENTRY_SLOT: usize = 1;
const RVOL_MIN: Decimal = dec!(3);
const MOVE_MIN: Decimal = dec!(0.01);
const SLIPPAGE: Decimal = dec!(0.0003);
/// Picks a day; each gets this share of the capital.
const MAX_TRADES_PER_DAY: usize = 1;

/// A stock whose 9:15 bar qualifies.
#[derive(Debug, Clone, PartialEq)]
pub struct Burst {
    pub symbol: String,
    pub key: String,
    pub buy: bool,
    pub high: Decimal,
    pub low: Decimal,
    pub change: Decimal,
    pub rvol: Decimal,
}

/// Whether a 9:15 bar is a burst: (long?, open-to-close change), or None.
pub fn burst_side(open: Decimal, close: Decimal, rvol: Decimal) -> Option<(bool, Decimal)> {
    if open <= Decimal::ZERO || rvol < RVOL_MIN {
        return None;
    }
    let change = close / open - Decimal::ONE;
    (change.abs() >= MOVE_MIN).then_some((change > Decimal::ZERO, change))
}

/// The day's picks: the `n` highest relative volumes among bursts in NIFTY's
/// direction, loudest first.
pub fn pick(bursts: Vec<Burst>, nifty_up: bool, n: usize) -> Vec<Burst> {
    let mut with_nifty: Vec<Burst> = bursts.into_iter().filter(|b| b.buy == nifty_up).collect();
    with_nifty.sort_by(|a, b| b.rvol.cmp(&a.rvol));
    with_nifty.truncate(n);
    with_nifty
}

/// ATLAS: every NIFTY 50 stock whose 9:15 bar is a burst.
async fn scan(data: &UpstoxBars, day: NaiveDate) -> Vec<Burst> {
    let mut out = Vec::new();
    for (symbol, key) in NIFTY50 {
        let (hist, today) = match (
            data.history(
                key,
                5,
                day - chrono::Duration::days(29),
                day - chrono::Duration::days(1),
            )
            .await,
            data.today(key, 5).await,
        ) {
            (Ok(h), Ok(t)) => (h, t),
            (Err(e), _) | (_, Err(e)) => {
                warn!(symbol, error = %e, "Burst scan could not read bars; left out today");
                continue;
            }
        };
        let Some((_, first)) = indexed(&today, day).into_iter().find(|(i, _)| *i == 0) else {
            continue;
        };
        let prior = prior_range_volumes(&hist, day, 1);
        let prior = &prior[prior.len().saturating_sub(14)..];
        let Some(rvol) = relative_volume(first.volume, prior) else {
            continue;
        };
        if let Some((buy, change)) = burst_side(first.open, first.close, rvol) {
            out.push(Burst {
                symbol: symbol.to_string(),
                key: key.to_string(),
                buy,
                high: first.high,
                low: first.low,
                change: change.round_dp(4),
                rvol: rvol.round_dp(2),
            });
        }
    }
    out
}

/// Run the opening-burst book; checks every 30 seconds in the session.
pub fn spawn_india_burst_worker(
    feed: Arc<UpstoxLiveMarketFeed>,
    store: Store,
    kill_switch: Arc<KillSwitch>,
    capital: Decimal,
    tfm_veto: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(%capital, tfm_veto, "NSE opening-burst worker started (paper only, no leverage)");
        let data = UpstoxBars::new(feed.access_token.clone());
        let mut decided: Option<NaiveDate> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let now = Utc::now();
            if let Err(e) = manage_open(&data, &store, STRATEGY, now).await {
                warn!(error = %e, "Could not manage the open burst position");
            }
            if let Err(e) = manage_open(&data, &store, timesfm::BURST_SHADOW, now).await {
                warn!(error = %e, "Could not manage the burst's TimesFM shadow position");
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
            match look_for_entry(&data, &store, capital, today, now, tfm_veto).await {
                Ok(()) => decided = Some(today),
                Err(e) => warn!(error = %e, "Burst entry pass failed; retrying"),
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
    tfm_veto: bool,
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
    let nifty_up = nifty_first.close > nifty_first.open;
    let bursts = scan(data, today).await;
    info!(bursts = ?bursts.iter().map(|b| (&b.symbol, b.buy, b.change, b.rvol)).collect::<Vec<_>>(), nifty_up, "ATLAS: NSE opening bursts");
    let picks = pick(bursts.clone(), nifty_up, MAX_TRADES_PER_DAY);
    if picks.is_empty() {
        info!("No opening burst in NIFTY's direction today");
        return Ok(());
    }
    // Each pick gets an equal slice, so the picks never exceed the capital.
    let slice = trading_cash(capital, losses) / Decimal::from(MAX_TRADES_PER_DAY as u64);
    for p in picks {
        // Too late is a missed trade: the scan can take a while.
        if nse_slot(Utc::now()).map(|(_, s)| s) != Some(ENTRY_SLOT) {
            info!(symbol = %p.symbol, "Burst entry after the 9:20 bar; skipping");
            continue;
        }
        if let Err(e) = enter(data, store, capital, slice, today, now, &p, bursts.len(), tfm_veto).await {
            warn!(symbol = %p.symbol, error = %e, "Burst entry failed");
        }
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
    p: &Burst,
    bursts_today: usize,
    tfm_veto: bool,
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

    // TimesFM, from the 9:15 bar: with the veto on, skip a trade its 15:15
    // forecast argues against and follow it on the shadow ledger; off (from
    // 6 Nov 2026), only record the forecast. Down means no opinion.
    let tfm = if blocked.is_none() {
        let view = match data.today(&p.key, 5).await {
            Ok(b) => timesfm::view_at_bar(data, &p.key, &indexed(&b, today), 0, today, buy).await,
            Err(e) => Err(e),
        };
        match view {
            Ok(v) => Some(v),
            Err(e) => {
                warn!(symbol = %p.symbol, error = %format!("{e:#}"), "TimesFM unavailable; trading the burst without it");
                None
            }
        }
    } else {
        None
    };
    let tfm_skip = tfm_veto && tfm.is_some_and(|v| !v.agrees);


    let action = if blocked.is_some() || tfm_skip { "SKIP" } else { "TRADE" };
    let card = serde_json::json!({
        "at": now,
        "market": "IN",
        "strategy": STRATEGY,
        "symbol": p.symbol,
        "atlas": { "relative_volume": p.rvol, "first_bar_change": p.change, "range_high": p.high, "range_low": p.low, "bursts_today": bursts_today },
        "vega": { "side": if buy { "LONG" } else { "SHORT" }, "signal_bar": 0, "stop": stop, "nifty_up": p.buy },
        "titan": { "allowed": blocked.is_none(), "reason": blocked.clone().unwrap_or_else(|| "within limits".into()), "qty": qty, "notional_inr": (Decimal::from(qty) * price).round_dp(2), "balance_inr": balance.round_dp(2) },
        "timesfm": match tfm {
            Some(v) => serde_json::json!({ "forecast_1515": v.forecast_end, "agrees": v.agrees }),
            None => serde_json::json!(null),
        },
        "nova": { "action": action },
    });
    store
        .insert_desk_card(now, "IN", STRATEGY, &p.symbol, action, "NONE", &card)
        .await?;
    if blocked.is_some() {
        info!(symbol = %p.symbol, reason = ?blocked, "NOVA (India burst): skip");
        return Ok(());
    }
    // Followed as if taken, so the filter is judged on what it gave up.
    let strategy = if tfm_skip {
        info!(symbol = %p.symbol, buy, %price, forecast = ?tfm.map(|v| v.forecast_end), "TimesFM disagrees: burst skipped, followed on the shadow ledger");
        timesfm::BURST_SHADOW
    } else {
        STRATEGY
    };
    store
        .insert_intraday_trade(
            strategy,
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
    if !tfm_skip {
        info!(symbol = %p.symbol, buy, qty, %price, %stop, rvol = %p.rvol, "NSE opening-burst paper entry; no live order was sent");
        crate::india::ladder::mirror_entry(store, STRATEGY, &today.to_string(), &p.symbol, buy, price, stop, now).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burst(symbol: &str, buy: bool, rvol: Decimal) -> Burst {
        Burst {
            symbol: symbol.into(),
            key: String::new(),
            buy,
            high: dec!(101),
            low: dec!(99),
            change: dec!(0.01),
            rvol,
        }
    }

    #[test]
    fn a_burst_needs_a_one_percent_move_on_three_times_the_volume() {
        assert_eq!(burst_side(dec!(100), dec!(101), dec!(3)), Some((true, dec!(0.01))));
        assert_eq!(burst_side(dec!(100), dec!(98.5), dec!(4)), Some((false, dec!(-0.015))));
        assert_eq!(burst_side(dec!(100), dec!(100.9), dec!(9)), None, "move too small");
        assert_eq!(burst_side(dec!(100), dec!(102), dec!(2.9)), None, "volume too low");
    }

    #[test]
    fn the_pick_is_the_loudest_burst_with_nifty() {
        let all = vec![
            burst("A", true, dec!(4)),
            burst("B", true, dec!(6)),
            burst("C", false, dec!(9)),
        ];
        let names = |v: Vec<Burst>| v.into_iter().map(|b| b.symbol).collect::<Vec<_>>();
        assert_eq!(names(pick(all.clone(), true, 3)), vec!["B", "A"]);
        assert_eq!(names(pick(all.clone(), true, 1)), vec!["B"]);
        assert_eq!(names(pick(all, false, 3)), vec!["C"]);
        assert!(pick(vec![burst("A", true, dec!(4))], false, 3).is_empty());
    }
}
