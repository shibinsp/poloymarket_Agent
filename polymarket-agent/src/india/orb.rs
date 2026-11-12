//! NSE intraday opening-range breakout on a small paper book, long and short.
//!
//! Rules, all on 5-minute bars of NIFTY 50 stocks:
//! 1. The range is the first 15 minutes (9:15-9:30). A stock is in play when
//!    those bars' volume is at least 2.5x their 14-session average.
//! 2. Enter on the first close beyond the range, at 12:30 at the latest, and
//!    only in NIFTY's direction for the day. A missed bar is a missed trade.
//! 3. Stop at the far side of the range, flat at 15:15 whatever happens.
//! 4. One position at a time in whole shares, no leverage, at most 6 trades a
//!    day, no new trade after a 2% day loss. (Until 2 Nov 2026: 11:30 and 3.)
//!
//! Paper only: fills are simulated from Upstox bars with 3 bp slippage and
//! intraday (MIS) charges, in a ledger apart from the cash sandbox. Replayed on
//! Rs 2,500 this was roughly break-even: -9.4% over Nov 2024 - Dec 2025, +7.6%
//! over Jan - Oct 2026. Charges take about 15 bp a round trip at this size, so
//! it is a live experiment, not a proven edge. 5x margin lost in both periods.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, Timelike, Utc};
use chrono_tz::Asia::Kolkata;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::agent::kill_switch::KillSwitch;
use crate::db::store::{IntradayOpen, Store};
use crate::india::desk::NIFTY50;
use crate::india::market_feed::UpstoxLiveMarketFeed;
use crate::india::momentum::NIFTY_50_KEY;
use crate::india::session::detect_nse_session;
use crate::india::timesfm;
use crate::us::orb::{relative_volume, Bar};
use crate::venue::types::Side;

pub const STRATEGY: &str = "india_orb";
const OR_BARS: usize = 3;
/// Bar 38 starts 12:25 and closes 12:30.
const LAST_SIGNAL_BAR: usize = 38;
const RVOL_MIN: Decimal = dec!(2.5);
const MAX_TRADES_PER_DAY: i64 = 6;
const DAILY_LOSS_PCT: Decimal = dec!(0.02);
const SLIPPAGE: Decimal = dec!(0.0003);

/// The IST session date of a bar and its 5-minute index from 9:15.
pub fn nse_slot(ts: DateTime<Utc>) -> Option<(NaiveDate, usize)> {
    let ist = ts.with_timezone(&Kolkata);
    let minutes = ist.hour() as i64 * 60 + ist.minute() as i64 - (9 * 60 + 15);
    (0..375)
        .contains(&minutes)
        .then(|| (ist.date_naive(), (minutes / 5) as usize))
}

/// One side's intraday charges in rupees: 0.05% brokerage capped at Rs 20,
/// STT on the sell, stamp duty on the buy, exchange fees and GST.
pub fn mis_charges(value: Decimal, buy: bool) -> Decimal {
    let brokerage = (value * dec!(0.0005)).min(dec!(20));
    let exchange = value * dec!(0.0000297);
    let sebi = value * dec!(0.000001);
    let gst = (brokerage + exchange + sebi) * dec!(0.18);
    let tax = if buy {
        value * dec!(0.00003)
    } else {
        value * dec!(0.00025)
    };
    (brokerage + exchange + sebi + gst + tax).round_dp(2)
}

/// (high, low, volume) of the opening range, once all its bars exist.
pub fn opening_range(bars: &[(usize, Bar)]) -> Option<(Decimal, Decimal, Decimal)> {
    let first: Vec<&Bar> = bars
        .iter()
        .filter(|(i, _)| *i < OR_BARS)
        .map(|(_, b)| b)
        .collect();
    if first.len() < OR_BARS {
        return None;
    }
    let high = first.iter().map(|b| b.high).max()?;
    let low = first.iter().map(|b| b.low).min()?;
    (high > low).then(|| (high, low, first.iter().map(|b| b.volume).sum()))
}

/// The first completed bar after the range closing beyond it.
pub fn first_breakout(
    high: Decimal,
    low: Decimal,
    bars: &[(usize, Bar)],
    current_slot: usize,
) -> Option<(usize, Side)> {
    for (i, bar) in bars {
        if *i < OR_BARS || *i >= current_slot {
            continue;
        }
        if *i > LAST_SIGNAL_BAR {
            return None;
        }
        if bar.close > high {
            return Some((*i, Side::Buy));
        }
        if bar.close < low {
            return Some((*i, Side::Sell));
        }
    }
    None
}

/// Cash a new trade may use: the capital only. Profits stay in the ledger and
/// are never reinvested, while losses do shrink what is left to trade. The
/// India books pass their losing trades only, so a profit never offsets them.
pub fn trading_cash(capital: Decimal, realized: Decimal) -> Decimal {
    (capital + realized).min(capital)
}

/// Whole shares the cash buys at this price, after the entry's charges.
pub fn whole_shares(cash: Decimal, price: Decimal, buy: bool) -> i64 {
    if price <= Decimal::ZERO || cash <= Decimal::ZERO {
        return 0;
    }
    let mut qty = (cash / price).floor();
    while qty > Decimal::ZERO && qty * price + mis_charges(qty * price, buy) > cash {
        qty -= Decimal::ONE;
    }
    qty.to_string().parse().unwrap_or(0)
}

/// Where a stop filled on this bar, if it was touched: a gap fills at the open.
pub fn stop_fill(buy: bool, stop: Decimal, bar: &Bar) -> Option<Decimal> {
    if buy {
        (bar.low <= stop).then(|| bar.open.min(stop))
    } else {
        (bar.high >= stop).then(|| bar.open.max(stop))
    }
}

/// Net P&L of a round trip and its total charges.
pub fn round_trip(buy: bool, qty: i64, entry: Decimal, exit: Decimal) -> (Decimal, Decimal) {
    let q = Decimal::from(qty);
    let gross = (exit - entry)
        * q
        * if buy {
            Decimal::ONE
        } else {
            Decimal::NEGATIVE_ONE
        };
    let charges = mis_charges(q * entry, buy) + mis_charges(q * exit, !buy);
    ((gross - charges).round_dp(2), charges)
}

/// Upstox candles for any instrument key.
pub struct UpstoxBars {
    http: reqwest::Client,
    token: Arc<RwLock<Option<String>>>,
}

impl UpstoxBars {
    pub fn new(token: Arc<RwLock<Option<String>>>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
            token,
        }
    }

    async fn get(&self, path: String) -> Result<Vec<Bar>> {
        let token = self
            .token
            .read()
            .await
            .clone()
            .context("Upstox access token required")?;
        let resp = self
            .http
            .get(format!(
                "https://api.upstox.com/v3/historical-candle/{path}"
            ))
            .bearer_auth(token)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Upstox candle request failed")?;
        if !resp.status().is_success() {
            bail!("Upstox candles returned {}", resp.status());
        }
        let payload: serde_json::Value = resp.json().await?;
        let mut bars: Vec<Bar> = payload["data"]["candles"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter_map(|row| {
                let v = row.as_array()?;
                let d = |i: usize| v.get(i)?.as_f64().and_then(Decimal::from_f64_retain);
                Some(Bar {
                    ts: DateTime::parse_from_rfc3339(v.first()?.as_str()?)
                        .ok()?
                        .with_timezone(&Utc),
                    open: d(1)?.round_dp(2),
                    high: d(2)?.round_dp(2),
                    low: d(3)?.round_dp(2),
                    close: d(4)?.round_dp(2),
                    volume: d(5)?,
                })
            })
            .collect();
        bars.sort_by_key(|b| b.ts);
        Ok(bars)
    }

    /// Past sessions' bars (the current session is not included).
    pub async fn history(
        &self,
        key: &str,
        minutes: u32,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<Bar>> {
        let key = urlencoding::encode(key);
        self.get(format!("{key}/minutes/{minutes}/{to}/{from}"))
            .await
    }

    /// Daily candles of past sessions between two dates.
    pub async fn daily(&self, key: &str, from: NaiveDate, to: NaiveDate) -> Result<Vec<Bar>> {
        let key = urlencoding::encode(key);
        self.get(format!("{key}/days/1/{to}/{from}")).await
    }

    /// Today's bars so far, the forming one included.
    pub async fn today(&self, key: &str, minutes: u32) -> Result<Vec<Bar>> {
        let key = urlencoding::encode(key);
        self.get(format!("intraday/{key}/minutes/{minutes}")).await
    }
}

/// Today's 5-minute bars as (index, bar).
pub fn indexed(bars: &[Bar], day: NaiveDate) -> Vec<(usize, Bar)> {
    bars.iter()
        .filter_map(|b| {
            nse_slot(b.ts)
                .filter(|(d, _)| *d == day)
                .map(|(_, i)| (i, *b))
        })
        .collect()
}

/// Each prior session's volume over its first `range_bars` bars, oldest first.
pub fn prior_range_volumes(bars: &[Bar], before: NaiveDate, range_bars: usize) -> Vec<Decimal> {
    let mut by_day: std::collections::BTreeMap<NaiveDate, (usize, Decimal)> = Default::default();
    for b in bars {
        if let Some((d, i)) = nse_slot(b.ts) {
            if d < before && i < range_bars {
                let e = by_day.entry(d).or_default();
                e.0 += 1;
                e.1 += b.volume;
            }
        }
    }
    by_day
        .into_values()
        .filter(|(n, _)| *n == range_bars)
        .map(|(_, v)| v)
        .collect()
}

/// A stock in play today.
#[derive(Debug, Clone)]
pub struct InPlay {
    pub symbol: String,
    pub key: String,
    pub high: Decimal,
    pub low: Decimal,
    pub rvol: Decimal,
}

/// ATLAS: rank the NIFTY 50 once the opening range is complete.
pub async fn rank(data: &UpstoxBars, day: NaiveDate) -> Vec<InPlay> {
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
                warn!(symbol, error = %e, "ATLAS could not read bars; left out today");
                continue;
            }
        };
        let Some((high, low, volume)) = opening_range(&indexed(&today, day)) else {
            continue;
        };
        let prior = prior_range_volumes(&hist, day, OR_BARS);
        let prior = &prior[prior.len().saturating_sub(14)..];
        if let Some(rvol) = relative_volume(volume, prior) {
            if rvol >= RVOL_MIN {
                out.push(InPlay {
                    symbol: symbol.to_string(),
                    key: key.to_string(),
                    high,
                    low,
                    rvol: rvol.round_dp(2),
                });
            }
        }
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.rvol));
    out
}

#[derive(Default)]
struct Day {
    date: Option<NaiveDate>,
    ranked: bool,
    in_play: Vec<InPlay>,
    done: HashSet<String>,
    last_slot: Option<usize>,
}

/// The ORB's tracking ledger: every trade it would take, followed as if taken,
/// while the book itself trades nothing. On 3 Nov 2026 the ORB failed the
/// walk-forward and robustness scores (May-Oct 2026: -Rs 574, 1 of 6 months
/// up; +Rs 223 with TimesFM but losing Aug-Oct), so the user moved it to
/// tracking only and let the burst lead India.
pub const TRACK: &str = "india_orb_track";

/// Run the intraday book; checks every 30 seconds in the session. With
/// `track_only`, its trades go to the [`TRACK`] ledger instead of the book.
/// With `longs_only`, only longs trade on the book; shorts go to [`TRACK`].
/// May-Oct 2026: longs TimesFM agreed with made +Rs 618 on 21 trades (both
/// halves up, every setting positive, +Rs 491 at 2x costs, but 2 of 6 months
/// and a negative walk-forward); all longs -Rs 495, shorts -Rs 538 or -Rs 348
/// with TimesFM. Set by the user on 5 Nov 2026 as a paper test.
pub fn spawn_india_orb_worker(
    feed: Arc<UpstoxLiveMarketFeed>,
    store: Store,
    kill_switch: Arc<KillSwitch>,
    capital: Decimal,
    track_only: bool,
    longs_only: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let book = if track_only { TRACK } else { STRATEGY };
        info!(%capital, track_only, longs_only, "NSE intraday ORB worker started (paper only, no leverage)");
        let data = UpstoxBars::new(feed.access_token.clone());
        let mut day = Day::default();
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let now = Utc::now();
            for ledger in [STRATEGY, TRACK] {
                if let Err(e) = manage_open(&data, &store, ledger, now).await {
                    warn!(error = %e, ledger, "Could not manage the open intraday position");
                }
            }
            if let Err(e) = manage_open(&data, &store, timesfm::ORB_SHADOW, now).await {
                warn!(error = %e, "Could not manage the TimesFM shadow positions");
            }
            crate::india::ladder::manage_all(&data, &store, STRATEGY, now).await;
            if kill_switch.is_tripped() || !detect_nse_session(now).allows_order_entry {
                continue;
            }
            if let Err(e) = look_for_entry(&data, &store, &mut day, capital, book, longs_only, now).await {
                warn!(error = %e, "Intraday entry pass failed");
            }
        }
    })
}

/// Close one strategy's open position at its stop or at 15:15; exits ignore
/// the halt.
pub async fn manage_open(
    data: &UpstoxBars,
    store: &Store,
    strategy: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let flat_at = NaiveTime::from_hms_opt(15, 15, 0).expect("valid time");
    let ist = now.with_timezone(&Kolkata);
    for t in store.open_intraday_trades(strategy).await? {
        let IntradayOpen {
            id,
            symbol,
            is_buy,
            qty,
            entry_price,
            stop_price,
            entry_at,
            day,
        } = t;
        let key = NIFTY50
            .iter()
            .find(|(s, _)| *s == symbol)
            .map(|(_, k)| *k)
            .context("open trade outside the NIFTY 50 table")?;
        let trade_day = entry_at.with_timezone(&Kolkata).date_naive();
        let stale = trade_day != ist.date_naive();
        let bars = if stale {
            data.history(key, 1, trade_day, trade_day).await?
        } else {
            data.today(key, 1).await?
        };
        let after: Vec<&Bar> = bars
            .iter()
            .filter(|b| b.ts >= entry_at && (stale || b.ts + chrono::Duration::minutes(1) <= now))
            .filter(|b| b.ts.with_timezone(&Kolkata).time() < flat_at)
            .collect();
        let slip = |px: Decimal| {
            if is_buy {
                px * (Decimal::ONE - SLIPPAGE)
            } else {
                px * (Decimal::ONE + SLIPPAGE)
            }
        };
        let hit = after.iter().find_map(|b| stop_fill(is_buy, stop_price, b));
        let (exit, reason) = if let Some(px) = hit {
            (slip(px).round_dp(2), "STOP")
        } else if stale || ist.time() >= flat_at {
            let last = after
                .last()
                .map(|b| b.close)
                .or_else(|| bars.last().map(|b| b.close))
                .unwrap_or(entry_price);
            (slip(last).round_dp(2), "FLAT_1515")
        } else {
            continue;
        };
        let (pnl, charges) = round_trip(is_buy, qty, entry_price, exit);
        store
            .close_intraday_trade(
                id,
                &exit.to_string(),
                reason,
                &charges.to_string(),
                &pnl.to_string(),
                now,
            )
            .await?;
        info!(strategy, symbol, day, reason, %exit, %pnl, %charges, "NSE intraday position closed (paper)");
    }
    Ok(())
}

/// The close of today's bar `index`, the price a signal is judged from.
fn signal_close(bars: &[(usize, Bar)], index: usize) -> Option<Decimal> {
    bars.iter().find(|(i, _)| *i == index).map(|(_, b)| b.close)
}

async fn look_for_entry(
    data: &UpstoxBars,
    store: &Store,
    day: &mut Day,
    capital: Decimal,
    book: &str,
    longs_only: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    let Some((today, slot)) = nse_slot(now) else {
        return Ok(());
    };
    if day.date != Some(today) {
        *day = Day {
            date: Some(today),
            ..Day::default()
        };
    }
    // Signals only form from 9:30 and may fill until the bar after 12:30.
    if !(OR_BARS..=LAST_SIGNAL_BAR + 1).contains(&slot) || day.last_slot == Some(slot) {
        return Ok(());
    }
    if !day.ranked {
        day.in_play = rank(data, today).await;
        day.ranked = true;
        info!(in_play = ?day.in_play.iter().map(|p| (&p.symbol, p.rvol)).collect::<Vec<_>>(), "ATLAS: NSE stocks in play");
    }
    day.last_slot = Some(slot);

    let (trades_today, day_pnl, _) = store.intraday_totals(book, &today.to_string()).await?;
    // Only losses come off the capital; profits are set aside, never netted.
    let balance = trading_cash(capital, store.intraday_losses(book).await?);
    if trades_today >= MAX_TRADES_PER_DAY
        || day_pnl <= -(capital * DAILY_LOSS_PCT)
        || !store.open_intraday_trades(book).await?.is_empty()
    {
        return Ok(());
    }

    let nifty = data.today(NIFTY_50_KEY, 5).await?;
    let nifty = indexed(&nifty, today);
    let Some(nifty_open) = nifty.first().map(|(_, b)| b.open) else {
        return Ok(());
    };

    let mut bars_by_symbol: HashMap<String, Vec<(usize, Bar)>> = HashMap::new();
    for p in day.in_play.iter().filter(|p| !day.done.contains(&p.symbol)) {
        if let Ok(b) = data.today(&p.key, 5).await {
            bars_by_symbol.insert(p.symbol.clone(), indexed(&b, today));
        }
    }
    let candidates = day.in_play.clone();
    for p in candidates {
        if day.done.contains(&p.symbol) {
            continue;
        }
        let Some(bars) = bars_by_symbol.get(&p.symbol) else {
            continue;
        };
        let Some((index, side)) = first_breakout(p.high, p.low, bars, slot) else {
            continue;
        };
        day.done.insert(p.symbol.clone());
        if slot > index + 1 {
            info!(symbol = %p.symbol, index, "Breakout noticed too late; skipping today");
            continue;
        }
        let buy = side == Side::Buy;
        let Some((_, nifty_bar)) = nifty.iter().find(|(i, _)| *i == index) else {
            continue;
        };
        if (nifty_bar.close > nifty_open) != buy {
            info!(symbol = %p.symbol, buy, "Breakout against NIFTY's direction; skipping today");
            continue;
        }
        let one_minute = data.today(&p.key, 1).await?;
        let Some(last) = one_minute.last().map(|b| b.close) else {
            continue;
        };
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

        // TimesFM: skip a trade its 15:15 forecast argues against, and follow
        // it on the shadow ledger instead. Down or failing means no opinion.
        let tfm = if blocked.is_none() {
            match timesfm::view_at_bar(data, &p.key, bars, index, today, buy).await {
                Ok(v) => Some(v),
                Err(e) => {
                    warn!(symbol = %p.symbol, error = %format!("{e:#}"), "TimesFM unavailable; trading without it");
                    None
                }
            }
        } else {
            None
        };
        let tfm_skip = tfm.is_some_and(|v| !v.agrees);


        let action = if blocked.is_some() || tfm_skip { "SKIP" } else { "TRADE" };
        let card = serde_json::json!({
            "at": now,
            "market": "IN",
            "strategy": STRATEGY,
            "symbol": p.symbol,
            "atlas": { "relative_volume": p.rvol, "range_high": p.high, "range_low": p.low, "in_play_today": day.in_play.len() },
            "vega": { "side": if buy { "LONG" } else { "SHORT" }, "signal_bar": index, "stop": stop, "nifty_open": nifty_open, "nifty_at_signal": nifty_bar.close },
            "titan": { "allowed": blocked.is_none(), "reason": blocked.clone().unwrap_or_else(|| "within limits".into()), "qty": qty, "notional_inr": (Decimal::from(qty) * price).round_dp(2), "balance_inr": balance.round_dp(2) },
            "timesfm": match tfm {
                Some(v) => serde_json::json!({ "forecast_1515": v.forecast_end, "signal_close": signal_close(bars, index), "agrees": v.agrees }),
                None => serde_json::json!(null),
            },
            "nova": { "action": action },
        });
        store
            .insert_desk_card(now, "IN", STRATEGY, &p.symbol, action, "NONE", &card)
            .await?;
        if blocked.is_some() {
            info!(symbol = %p.symbol, reason = ?blocked, "NOVA (India intraday): skip");
            continue;
        }
        if tfm_skip {
            // Followed as if taken, so the filter is judged on what it gave
            // up; the real book stays free for the next breakout.
            store
                .insert_intraday_trade(
                    timesfm::ORB_SHADOW,
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
            info!(symbol = %p.symbol, buy, %price, forecast = ?tfm.map(|v| v.forecast_end), "TimesFM disagrees: skipped, followed on the shadow ledger");
            continue;
        }
        // Longs only: a short is followed on the tracking ledger and leaves
        // the book free for the next long breakout.
        let ledger = if longs_only && !buy { TRACK } else { book };
        store
            .insert_intraday_trade(
                ledger,
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
        info!(symbol = %p.symbol, buy, qty, %price, %stop, book = ledger, "NSE intraday paper entry; no live order was sent");
        if ledger != book {
            continue;
        }
        if book == STRATEGY {
            crate::india::ladder::mirror_entry(store, book, &today.to_string(), &p.symbol, buy, price, stop, now).await;
        }
        break; // one position at a time
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Kolkata
            .with_ymd_and_hms(2026, 11, 2, h, m, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn bar(h: u32, m: u32, o: Decimal, hi: Decimal, lo: Decimal, c: Decimal) -> Bar {
        Bar {
            ts: at(h, m),
            open: o,
            high: hi,
            low: lo,
            close: c,
            volume: dec!(100),
        }
    }

    fn flat(h: u32, m: u32, c: Decimal) -> Bar {
        bar(h, m, c, c + dec!(1), c - dec!(1), c)
    }

    #[test]
    fn slots_follow_ist_from_the_open() {
        assert_eq!(nse_slot(at(9, 15)).map(|s| s.1), Some(0));
        assert_eq!(nse_slot(at(9, 30)).map(|s| s.1), Some(3));
        assert_eq!(nse_slot(at(11, 25)).map(|s| s.1), Some(26));
        assert_eq!(nse_slot(at(9, 14)), None);
        assert_eq!(nse_slot(at(15, 30)), None);
    }

    #[test]
    fn the_range_needs_all_three_bars() {
        let bars = indexed(
            &[flat(9, 15, dec!(100)), flat(9, 20, dec!(102))],
            at(9, 15).with_timezone(&Kolkata).date_naive(),
        );
        assert_eq!(opening_range(&bars), None);
        let bars = indexed(
            &[
                flat(9, 15, dec!(100)),
                flat(9, 20, dec!(102)),
                flat(9, 25, dec!(101)),
            ],
            at(9, 15).with_timezone(&Kolkata).date_naive(),
        );
        assert_eq!(opening_range(&bars), Some((dec!(103), dec!(99), dec!(300))));
    }

    #[test]
    fn only_a_completed_bar_before_the_cutoff_signals() {
        let bars = vec![(3, flat(9, 30, dec!(105))), (4, flat(9, 35, dec!(90)))];
        assert_eq!(
            first_breakout(dec!(103), dec!(99), &bars, 3),
            None,
            "still forming"
        );
        assert_eq!(
            first_breakout(dec!(103), dec!(99), &bars, 4),
            Some((3, Side::Buy))
        );
        let short = vec![(3, flat(9, 30, dec!(95)))];
        assert_eq!(
            first_breakout(dec!(103), dec!(99), &short, 4),
            Some((3, Side::Sell))
        );
        let noon = vec![(38, flat(12, 25, dec!(105)))];
        assert_eq!(
            first_breakout(dec!(103), dec!(99), &noon, 39),
            Some((38, Side::Buy)),
            "the 12:25 bar is the last that may signal"
        );
        let late = vec![(39, flat(12, 30, dec!(105)))];
        assert_eq!(first_breakout(dec!(103), dec!(99), &late, 42), None);
    }

    #[test]
    fn prior_volumes_ignore_today_and_partial_days() {
        let day = at(9, 15).with_timezone(&Kolkata).date_naive();
        let shift = |b: Bar, d: i64| Bar {
            ts: b.ts - chrono::Duration::days(d),
            ..b
        };
        let bars = vec![
            shift(flat(9, 15, dec!(1)), 1),
            shift(flat(9, 20, dec!(1)), 1),
            shift(flat(9, 25, dec!(1)), 1),
            shift(flat(9, 15, dec!(1)), 2),
            flat(9, 15, dec!(1)),
        ];
        assert_eq!(prior_range_volumes(&bars, day, OR_BARS), vec![dec!(300)]);
    }

    #[test]
    fn shares_are_whole_and_fit_after_charges() {
        assert_eq!(
            whole_shares(dec!(2500), dec!(250), true),
            9,
            "10 x 250 leaves nothing for charges"
        );
        assert_eq!(whole_shares(dec!(2500), dec!(100), true), 24);
        assert_eq!(whole_shares(dec!(2500), dec!(3000), true), 0);
    }

    #[test]
    fn profits_are_not_reinvested_but_losses_shrink_the_cash() {
        assert_eq!(trading_cash(dec!(2500), dec!(61.24)), dec!(2500));
        assert_eq!(trading_cash(dec!(2500), dec!(0)), dec!(2500));
        assert_eq!(trading_cash(dec!(2500), dec!(-40)), dec!(2460));
    }

    #[test]
    fn stops_fill_at_the_level_or_a_gap_open() {
        let b = bar(10, 0, dec!(100), dec!(101), dec!(95), dec!(96));
        assert_eq!(stop_fill(true, dec!(97), &b), Some(dec!(97)));
        assert_eq!(
            stop_fill(true, dec!(102), &b),
            Some(dec!(100)),
            "gapped through"
        );
        assert_eq!(stop_fill(true, dec!(90), &b), None);
        assert_eq!(stop_fill(false, dec!(100.5), &b), Some(dec!(100.5)));
        assert_eq!(
            stop_fill(false, dec!(99), &b),
            Some(dec!(100)),
            "gapped through"
        );
    }

    #[test]
    fn round_trip_charges_both_sides_and_signs_shorts() {
        let (long, charges) = round_trip(true, 10, dec!(250), dec!(255));
        assert!(
            charges > dec!(3) && charges < dec!(5),
            "about 15 bp of Rs 2,500: {charges}"
        );
        assert_eq!(long, dec!(50) - charges);
        let (short, _) = round_trip(false, 10, dec!(250), dec!(255));
        assert!(short < dec!(-50));
    }
}
