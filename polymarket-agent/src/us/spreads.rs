//! The U.S. credit-spread book: a same-day (0DTE) bear call spread on SPY and
//! QQQ, paper only. Instead of guessing direction it is paid a small premium
//! to bet the index will not rally past a strike by the end of the day.
//!
//! Rules:
//! 1. At 11:00 New York, for each underlying: sell the same-day call with the
//!    first listed strike at least `otm_pct` above the price, and buy the call
//!    `width` dollars higher, as one multi-leg order: first at the mid credit;
//!    unfilled after a minute, once more at the natural credit (short bid less
//!    long ask); unfilled after two more minutes, no trade today.
//! 2. No stop: the bought call caps the loss at width less credit.
//! 3. Buy the spread back at 15:50 (before the bell, so nothing is exercised
//!    or assigned), at the natural debit, a little higher on each retry.
//! 4. One spread per underlying per day. The halt stops entries, never exits.
//!
//! Backtest (5 Nov 2026, May - Oct 2026, Alpaca option bars, half-spread
//! max(3%, $0.02) a leg, held to the close): $2 wide 0.6% out of the money,
//! SPY + QQQ, 148 spreads +$1,207 (+$759 / +$448 by half, 5 of 6 months up,
//! worst day -$188, max drawdown -$305, 0.8% chance of luck by bootstrap).
//! $5 wide: +$2,526, +$1,795 at double costs, walk-forward +$2,783. Unlike the
//! ORB it does better with the FOMC / CPI / jobs days in, so it trades them.
//! Only six months, in which rally afternoons were rare enough; a runaway bull
//! market is its weak spot. Set by the user on 6 Nov 2026.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc, Weekday};
use chrono_tz::America::New_York;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::agent::kill_switch::KillSwitch;
use crate::config::UsSpreadsConfig;
use crate::db::store::{SpreadTrade, Store};
use crate::us::options::{AlpacaOptions, OrderAck};

pub const STRATEGY: &str = "us_spreads";
/// The mid-price entry waits this long before trying the natural credit.
const MID_WAIT_SECS: i64 = 60;
/// The natural-credit entry still unfilled after this is cancelled: no trade.
const ENTRY_WAIT_SECS: i64 = 120;
/// Client order id prefix of the second, natural-credit entry.
const NATURAL_PREFIX: &str = "spr-n-";
/// A buy-back still unfilled after this is cancelled and sent again higher.
const EXIT_WAIT_SECS: i64 = 30;
/// Added to the buy-back price on each retry.
const EXIT_STEP: Decimal = dec!(0.05);
/// Minutes after the entry time in which a missed entry may still go.
const ENTRY_WINDOW_MINS: u32 = 15;
const OPEN: u32 = 9 * 60 + 30;
const CLOSE: u32 = 16 * 60;

/// New York date and minutes since midnight on a weekday.
fn ny_minutes(now: DateTime<Utc>) -> Option<(NaiveDate, u32)> {
    let ny = now.with_timezone(&New_York);
    if matches!(ny.weekday(), Weekday::Sat | Weekday::Sun) {
        return None;
    }
    Some((ny.date_naive(), ny.hour() * 60 + ny.minute()))
}

/// The two strikes: the first listed strike at least `otm_pct` above `price`,
/// and the strike exactly `width` above it. None when either is not listed.
pub fn pick_strikes(
    strikes: &[Decimal],
    price: Decimal,
    otm_pct: Decimal,
    width: Decimal,
) -> Option<(Decimal, Decimal)> {
    let floor = price * (Decimal::ONE + otm_pct);
    let short = strikes.iter().copied().filter(|k| *k >= floor).min()?;
    let long = short + width;
    strikes.contains(&long).then_some((short, long))
}

/// The credit for selling the spread at the natural prices: the short call's
/// bid less the long call's ask, a share.
pub fn natural_credit(short_bid: Decimal, long_ask: Decimal) -> Decimal {
    short_bid - long_ask
}

/// The credit at the middle of both legs' quotes, rounded down to a cent and
/// never under the natural credit.
pub fn mid_credit(
    short_bid: Decimal,
    short_ask: Decimal,
    long_bid: Decimal,
    long_ask: Decimal,
) -> Decimal {
    let mid = (short_bid + short_ask) / dec!(2) - (long_bid + long_ask) / dec!(2);
    mid.round_dp_with_strategy(2, rust_decimal::RoundingStrategy::ToZero)
        .max(natural_credit(short_bid, long_ask))
}

/// The price to buy the spread back: the short's ask less the long's bid, at
/// least a cent, plus a step per earlier try, never more than the width.
pub fn buyback_debit(short_ask: Decimal, long_bid: Decimal, tries: i64, width: Decimal) -> Decimal {
    let base = (short_ask - long_bid).max(dec!(0.01));
    (base + EXIT_STEP * Decimal::from(tries))
        .min(width)
        .round_dp(2)
}

/// Dollars made on a spread: credit less debit, a hundred shares a contract.
pub fn pnl(credit: Decimal, debit: Decimal, qty: i64) -> Decimal {
    (credit - debit) * dec!(100) * Decimal::from(qty)
}

/// What the spread is worth at expiry with the underlying at `price`.
pub fn value_at_expiry(price: Decimal, short: Decimal, long: Decimal) -> Decimal {
    (price - short).max(Decimal::ZERO) - (price - long).max(Decimal::ZERO)
}

impl AlpacaOptions {
    /// One multi-leg limit order. Alpaca reads `limit_price` as a debit when
    /// positive and a credit when negative.
    async fn mleg(
        &self,
        legs: serde_json::Value,
        qty: i64,
        limit_price: Decimal,
        client_order_id: &str,
    ) -> Result<OrderAck> {
        let body = json!({
            "order_class": "mleg", "qty": qty.to_string(), "type": "limit",
            "limit_price": limit_price.round_dp(2).to_string(), "time_in_force": "day",
            "client_order_id": client_order_id, "legs": legs,
        });
        self.send(
            self.req(reqwest::Method::POST, format!("{}/v2/orders", self.trading))
                .json(&body),
        )
        .await
    }
}

pub fn spawn_us_spreads_worker(
    api: AlpacaOptions,
    store: Store,
    kill_switch: Arc<KillSwitch>,
    config: UsSpreadsConfig,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(underlyings = ?config.underlyings, otm_pct = %config.otm_pct, width = %config.width,
              "U.S. credit-spread worker started (paper only, 0DTE bear call spreads)");
        let mut exit_sent: HashMap<i64, DateTime<Utc>> = HashMap::new();
        loop {
            tokio::time::sleep(StdDuration::from_secs(15)).await;
            let now = Utc::now();
            if let Err(e) = manage(&api, &store, &config, &mut exit_sent, now).await {
                warn!(error = %format!("{e:#}"), "Spread book: managing open spreads failed");
            }
            let Some((today, mins)) = ny_minutes(now) else {
                continue;
            };
            let window = config.entry_minute..config.entry_minute + ENTRY_WINDOW_MINS;
            if kill_switch.is_tripped() || !window.contains(&mins) {
                continue;
            }
            if let Err(e) = enter_all(&api, &store, &config, today, now).await {
                warn!(error = %format!("{e:#}"), "Spread book: entry pass failed");
            }
        }
    })
}

async fn enter_all(
    api: &AlpacaOptions,
    store: &Store,
    config: &UsSpreadsConfig,
    today: NaiveDate,
    now: DateTime<Utc>,
) -> Result<()> {
    let done = store.spread_underlyings_on(&today.to_string()).await?;
    for u in config.underlyings.iter().filter(|u| !done.contains(u)) {
        if let Err(e) = enter(api, store, config, u, today, now).await {
            warn!(underlying = %u, error = %format!("{e:#}"), "Spread book: entry failed");
        }
    }
    Ok(())
}

async fn enter(
    api: &AlpacaOptions,
    store: &Store,
    config: &UsSpreadsConfig,
    underlying: &str,
    today: NaiveDate,
    now: DateTime<Utc>,
) -> Result<()> {
    let day = today.to_string();
    let price = api.last_price(underlying).await?;
    let contracts = api
        .contracts(underlying, true, today, today, (price, price * dec!(1.03)))
        .await?;
    let listed: Vec<_> = contracts
        .iter()
        .filter(|c| c.expiration_date == today && c.tradable != Some(false))
        .collect();
    let strikes: Vec<Decimal> = listed.iter().map(|c| c.strike_price).collect();
    let day_s = day.as_str();
    let skip = |why: String| async move {
        info!(%underlying, %price, reason = %why, "Spread book: no spread today");
        let row = store
            .insert_spread_trade(
                day_s,
                underlying,
                "-",
                "-",
                "0",
                "0",
                &price.to_string(),
                0,
                "0",
                &format!("spr-skip-{underlying}-{today}"),
                now,
            )
            .await?;
        store
            .spread_cancelled(row, &format!("skipped: {why}"))
            .await
    };
    let Some((ks, kl)) = pick_strikes(&strikes, price, config.otm_pct, config.width) else {
        return skip("no same-day strikes at the distance and width".into()).await;
    };
    let symbol_of = |k: Decimal| {
        listed
            .iter()
            .find(|c| c.strike_price == k)
            .map(|c| c.symbol.clone())
    };
    let (Some(short), Some(long)) = (symbol_of(ks), symbol_of(kl)) else {
        return skip("strike symbols missing".into()).await;
    };
    let quotes = api.quotes(&[short.clone(), long.clone()]).await?;
    let (Some(sb), Some(sa), Some(lb), Some(la)) = (
        quotes.get(&short).and_then(|q| q.bp),
        quotes.get(&short).and_then(|q| q.ap),
        quotes.get(&long).and_then(|q| q.bp),
        quotes.get(&long).and_then(|q| q.ap),
    ) else {
        return skip("no quotes".into()).await;
    };
    let natural = natural_credit(sb, la).round_dp(2);
    if natural < config.min_credit {
        return skip(format!("credit {natural} under {}", config.min_credit)).await;
    }
    let credit = mid_credit(sb, sa, lb, la);
    let cid = format!("spr-{underlying}-{}", now.timestamp());
    let row = store
        .insert_spread_trade(
            &day,
            underlying,
            &short,
            &long,
            &ks.to_string(),
            &kl.to_string(),
            &price.to_string(),
            config.qty,
            &credit.to_string(),
            &cid,
            now,
        )
        .await?;
    let legs = json!([
        { "symbol": short, "ratio_qty": "1", "side": "sell", "position_intent": "sell_to_open" },
        { "symbol": long, "ratio_qty": "1", "side": "buy", "position_intent": "buy_to_open" },
    ]);
    match api.mleg(legs, config.qty, -credit, &cid).await {
        Ok(ack) => {
            info!(%underlying, %price, short = %ks, long = %kl, %credit, %natural, qty = config.qty,
                  "Spread book: bear call spread sent at the mid");
            store.set_spread_order_id(row, &ack.id).await
        }
        Err(e) => {
            let why: String = format!("order refused: {e:#}").chars().take(200).collect();
            store.spread_cancelled(row, &why).await?;
            Err(e)
        }
    }
}

/// Send the unfilled mid entry again at the natural credit, if it is still
/// worth selling; otherwise no trade today.
async fn natural_retry(
    api: &AlpacaOptions,
    store: &Store,
    config: &UsSpreadsConfig,
    t: &SpreadTrade,
    now: DateTime<Utc>,
) -> Result<()> {
    let quotes = api
        .quotes(&[t.short_contract.clone(), t.long_contract.clone()])
        .await?;
    let natural = match (
        quotes.get(&t.short_contract).and_then(|q| q.bp),
        quotes.get(&t.long_contract).and_then(|q| q.ap),
    ) {
        (Some(b), Some(a)) => natural_credit(b, a).round_dp(2),
        _ => Decimal::ZERO,
    };
    if natural < config.min_credit {
        info!(underlying = %t.underlying, %natural, "Spread book: mid unfilled and the natural credit is too small; no trade");
        return store
            .spread_cancelled(t.id, &format!("mid unfilled; natural credit {natural}"))
            .await;
    }
    let cid = format!("{NATURAL_PREFIX}{}-{}", t.underlying, now.timestamp());
    let legs = json!([
        { "symbol": t.short_contract, "ratio_qty": "1", "side": "sell", "position_intent": "sell_to_open" },
        { "symbol": t.long_contract, "ratio_qty": "1", "side": "buy", "position_intent": "buy_to_open" },
    ]);
    match api.mleg(legs, t.qty, -natural, &cid).await {
        Ok(ack) => {
            info!(underlying = %t.underlying, %natural, "Spread book: mid unfilled; sent at the natural credit");
            store
                .spread_resend(t.id, &natural.to_string(), &cid, &ack.id, now)
                .await
        }
        Err(e) => {
            let why: String = format!("natural order refused: {e:#}")
                .chars()
                .take(200)
                .collect();
            store.spread_cancelled(t.id, &why).await?;
            Err(e)
        }
    }
}

async fn manage(
    api: &AlpacaOptions,
    store: &Store,
    config: &UsSpreadsConfig,
    exit_sent: &mut HashMap<i64, DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<()> {
    for t in store.live_spread_trades().await? {
        if let Err(e) = manage_one(api, store, config, exit_sent, &t, now).await {
            warn!(underlying = %t.underlying, id = t.id, error = %format!("{e:#}"), "Spread book: could not update a spread");
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct ClosedOrder {
    #[serde(default)]
    symbol: Option<String>,
    side: String,
    status: String,
    #[serde(default)]
    filled_avg_price: Option<String>,
    #[serde(default)]
    filled_at: Option<String>,
}

/// The price of a stand-alone buy of the short call filled after the entry:
/// an order Alpaca itself sent (our own buy-back is a two-leg order, whose
/// top-level symbol is empty). None while the short call is still held.
fn short_buyback_price(
    orders: &[ClosedOrder],
    short_contract: &str,
    entry_at: &str,
) -> Option<Decimal> {
    orders
        .iter()
        .filter(|o| {
            o.symbol.as_deref() == Some(short_contract) && o.side == "buy" && o.status == "filled"
        })
        .filter(|o| o.filled_at.as_deref().is_some_and(|f| f > entry_at))
        .find_map(|o| o.filled_avg_price.as_deref()?.parse::<Decimal>().ok())
}

async fn alpaca_bought_short(api: &AlpacaOptions, t: &SpreadTrade) -> Result<Option<Decimal>> {
    let url = format!(
        "{}/v2/orders?status=closed&symbols={}&limit=50&direction=desc",
        api.trading, t.short_contract
    );
    let orders: Vec<ClosedOrder> = api.send(api.req(reqwest::Method::GET, url)).await?;
    Ok(short_buyback_price(&orders, &t.short_contract, &t.entry_at))
}

async fn manage_one(
    api: &AlpacaOptions,
    store: &Store,
    config: &UsSpreadsConfig,
    exit_sent: &mut HashMap<i64, DateTime<Utc>>,
    t: &SpreadTrade,
    now: DateTime<Utc>,
) -> Result<()> {
    let ny = ny_minutes(now);
    match t.status.as_str() {
        "PENDING" => {
            let Some(id) = t.venue_order_id.as_deref() else {
                return store.spread_cancelled(t.id, "never reached Alpaca").await;
            };
            let ack = api.order(id).await?;
            match ack.status.as_str() {
                "filled" => {
                    let credit = ack
                        .filled_avg_price
                        .map(|p| p.abs())
                        .context("filled with no price")?;
                    info!(underlying = %t.underlying, %credit, "Spread book: spread sold");
                    store.spread_opened(t.id, &credit.to_string()).await
                }
                "canceled" | "expired"
                    if !t.client_order_id.starts_with(NATURAL_PREFIX)
                        && ack.status == "canceled" =>
                {
                    // The mid went unfilled: once more at the natural credit.
                    natural_retry(api, store, config, t, now).await
                }
                "canceled" | "expired" | "rejected" | "done_for_day" => {
                    info!(underlying = %t.underlying, status = %ack.status, "Spread book: entry not filled, no trade");
                    store
                        .spread_cancelled(t.id, &format!("entry {}", ack.status))
                        .await
                }
                _ => {
                    let sent = DateTime::parse_from_rfc3339(&t.entry_at)
                        .map(|d| d.with_timezone(&Utc))
                        .unwrap_or(now);
                    let wait = if t.client_order_id.starts_with(NATURAL_PREFIX) {
                        ENTRY_WAIT_SECS
                    } else {
                        MID_WAIT_SECS
                    };
                    if (now - sent).num_seconds() > wait {
                        api.cancel(id).await?;
                    }
                    Ok(())
                }
            }
        }
        "OPEN" => {
            let stale = ny.map_or(true, |(d, _)| d.to_string() != t.day);
            let short: Decimal = t.short_strike.parse()?;
            let long: Decimal = t.long_strike.parse()?;
            let credit: Decimal = t.entry_credit.as_deref().unwrap_or("0").parse()?;
            // From the exit minute on (or a later day), first ask whether
            // Alpaca already bought the short call back.
            let due = stale || ny.is_some_and(|(_, m)| m >= config.exit_minute);
            if !due {
                return Ok(());
            }
            if let Some(short_fill) = alpaca_bought_short(api, t).await? {
                // Alpaca closed the short leg itself (on expiry day it buys
                // back short calls at about 15:45 New York, before our 15:50
                // buy-back). Book that real fill; the bought call is left to
                // expire and is worth its value at the close.
                let px = api.last_price(&t.underlying).await?;
                let long_value = value_at_expiry(px, long, long + config.width);
                let debit = (short_fill - long_value).max(Decimal::ZERO);
                let p = pnl(credit, debit, t.qty);
                info!(underlying = %t.underlying, %credit, %short_fill, %long_value, pnl = %p, "Spread book: Alpaca bought the short call back; spread closed");
                exit_sent.remove(&t.id);
                return store
                    .spread_closed(
                        t.id,
                        &debit.to_string(),
                        &p.round_dp(2).to_string(),
                        "ALPACA_EXPIRY_CLOSE",
                        now,
                    )
                    .await;
            }
            if stale {
                // Missed the 15:50 buy-back (the agent was down): the spread
                // expired. Settle it on the underlying's last price.
                let px = api.last_price(&t.underlying).await?;
                let debit = value_at_expiry(px, short, long);
                let p = pnl(credit, debit, t.qty);
                warn!(underlying = %t.underlying, %px, %debit, pnl = %p, "Spread book: spread expired before it was bought back");
                return store
                    .spread_closed(
                        t.id,
                        &debit.to_string(),
                        &p.round_dp(2).to_string(),
                        "EXPIRED",
                        now,
                    )
                    .await;
            }
            let Some((_, mins)) = ny else { return Ok(()) };
            if mins < config.exit_minute || !(OPEN..CLOSE).contains(&mins) {
                return Ok(());
            }
            let quotes = api
                .quotes(&[t.short_contract.clone(), t.long_contract.clone()])
                .await?;
            let short_ask = quotes
                .get(&t.short_contract)
                .and_then(|q| q.ap)
                .unwrap_or(config.width);
            let long_bid = quotes
                .get(&t.long_contract)
                .and_then(|q| q.bp)
                .unwrap_or(Decimal::ZERO);
            let debit = buyback_debit(short_ask, long_bid, t.exit_tries, config.width);
            let cid = format!("spr-x-{}-{}", t.id, now.timestamp());
            let legs = json!([
                { "symbol": t.short_contract, "ratio_qty": "1", "side": "buy", "position_intent": "buy_to_close" },
                { "symbol": t.long_contract, "ratio_qty": "1", "side": "sell", "position_intent": "sell_to_close" },
            ]);
            let ack = api.mleg(legs, t.qty, debit, &cid).await?;
            info!(underlying = %t.underlying, %debit, tries = t.exit_tries, "Spread book: buying the spread back");
            exit_sent.insert(t.id, now);
            store.spread_closing(t.id, &cid, &ack.id, "FLAT_1550").await
        }
        "CLOSING" => {
            let Some(id) = t.exit_order_id.as_deref() else {
                return store.spread_reopen(t.id).await;
            };
            let ack = api.order(id).await?;
            match ack.status.as_str() {
                "filled" => {
                    let debit = ack
                        .filled_avg_price
                        .map(|p| p.abs())
                        .context("exit filled with no price")?;
                    let credit: Decimal = t.entry_credit.as_deref().unwrap_or("0").parse()?;
                    let p = pnl(credit, debit, t.qty);
                    info!(underlying = %t.underlying, %credit, %debit, pnl = %p, "Spread book: closed");
                    exit_sent.remove(&t.id);
                    store
                        .spread_closed(
                            t.id,
                            &debit.to_string(),
                            &p.round_dp(2).to_string(),
                            "FLAT_1550",
                            now,
                        )
                        .await
                }
                "canceled" | "expired" | "rejected" | "done_for_day" => {
                    exit_sent.remove(&t.id);
                    store.spread_reopen(t.id).await
                }
                _ => {
                    // After a restart the send time is unknown: wait one more pass.
                    let sent = *exit_sent.entry(t.id).or_insert(now);
                    if (now - sent).num_seconds() > EXIT_WAIT_SECS {
                        api.cancel(id).await?;
                    }
                    Ok(())
                }
            }
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strikes_are_the_first_far_enough_and_one_width_above() {
        let ks = [
            dec!(765),
            dec!(766),
            dec!(767),
            dec!(768),
            dec!(769),
            dec!(770),
            dec!(771),
            dec!(772),
        ];
        // 762.40 x 1.006 = 766.97: the first strike at or above is 767.
        assert_eq!(
            pick_strikes(&ks, dec!(762.40), dec!(0.006), dec!(2)),
            Some((dec!(767), dec!(769)))
        );
        assert_eq!(
            pick_strikes(&ks, dec!(768.50), dec!(0.006), dec!(2)),
            None,
            "no 775 listed"
        );
        assert_eq!(
            pick_strikes(&ks, dec!(790), dec!(0.006), dec!(2)),
            None,
            "nothing far enough"
        );
    }

    #[test]
    fn credit_is_short_bid_less_long_ask() {
        assert_eq!(natural_credit(dec!(0.42), dec!(0.18)), dec!(0.24));
    }

    #[test]
    fn the_mid_credit_sits_between_the_quotes_and_never_under_the_natural() {
        // short 1.06 x 1.10, long 0.66 x 0.70: mid 1.08 - 0.68 = 0.40, natural 0.36.
        assert_eq!(
            mid_credit(dec!(1.06), dec!(1.10), dec!(0.66), dec!(0.70)),
            dec!(0.40)
        );
        // half cents round down
        assert_eq!(
            mid_credit(dec!(1.05), dec!(1.10), dec!(0.66), dec!(0.70)),
            dec!(0.39)
        );
        assert_eq!(
            mid_credit(dec!(0.40), dec!(0.40), dec!(0.20), dec!(0.20)),
            dec!(0.20)
        );
    }

    #[test]
    fn buyback_starts_at_the_natural_debit_and_steps_up_to_the_width() {
        assert_eq!(
            buyback_debit(dec!(0.06), dec!(0.02), 0, dec!(2)),
            dec!(0.04)
        );
        assert_eq!(
            buyback_debit(dec!(0.01), dec!(0.03), 0, dec!(2)),
            dec!(0.01),
            "never below a cent"
        );
        assert_eq!(
            buyback_debit(dec!(0.06), dec!(0.02), 2, dec!(2)),
            dec!(0.14)
        );
        assert_eq!(
            buyback_debit(dec!(2.10), dec!(0.05), 3, dec!(2)),
            dec!(2),
            "never above the width"
        );
    }

    #[test]
    fn pnl_and_expiry_value() {
        assert_eq!(pnl(dec!(0.24), dec!(0.04), 1), dec!(20));
        assert_eq!(pnl(dec!(0.24), dec!(2), 1), dec!(-176));
        assert_eq!(
            value_at_expiry(dec!(766), dec!(767), dec!(769)),
            Decimal::ZERO
        );
        assert_eq!(value_at_expiry(dec!(768), dec!(767), dec!(769)), dec!(1));
        assert_eq!(value_at_expiry(dec!(775), dec!(767), dec!(769)), dec!(2));
    }

    fn order(sym: &str, side: &str, status: &str, px: &str, at: &str) -> ClosedOrder {
        ClosedOrder {
            symbol: Some(sym.into()),
            side: side.into(),
            status: status.into(),
            filled_avg_price: Some(px.into()),
            filled_at: Some(at.into()),
        }
    }

    #[test]
    fn alpacas_own_buyback_of_the_short_call_is_found() {
        let short = "QQQ261109C00683000";
        let entry = "2026-11-09T16:01:39+00:00";
        let orders = vec![
            order(
                "QQQ261109C00685000",
                "buy",
                "filled",
                "0.21",
                "2026-11-09T16:01:39.9Z",
            ),
            order(short, "buy", "canceled", "0.10", "2026-11-09T20:40:00Z"),
            order(short, "buy", "filled", "0.06", "2026-11-09T20:45:03.33Z"),
        ];
        assert_eq!(short_buyback_price(&orders, short, entry), Some(dec!(0.06)));
        // A fill from before this spread's entry (an older trade) does not count.
        assert_eq!(short_buyback_price(&orders[..1], short, entry), None);
        let old = vec![order(short, "buy", "filled", "0.5", "2026-11-09T15:00:00Z")];
        assert_eq!(short_buyback_price(&old, short, entry), None);
    }

    #[test]
    fn weekdays_only() {
        let sat = Utc.with_ymd_and_hms(2026, 11, 7, 16, 0, 0).unwrap();
        assert!(ny_minutes(sat).is_none());
        let fri_11 = Utc.with_ymd_and_hms(2026, 11, 6, 16, 0, 0).unwrap();
        assert_eq!(ny_minutes(fri_11).map(|(_, m)| m), Some(11 * 60));
    }

    use chrono::TimeZone;
}
