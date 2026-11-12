//! The U.S. options book: a call or put bought on each ORB / burst stock entry,
//! on a paper book of its own ($500), long premium only, intraday.
//!
//! Rules:
//! 1. When the ORB or the burst opens a stock trade, buy the matching option:
//!    a call on a long breakout, a put on a short breakdown. The stock trade
//!    keeps its own $500 book; this one mirrors its decision, not its money.
//! 2. The nearest expiry 2 to 9 days out (never same-day), the strike nearest
//!    the money whose ask fits a third of the book, a bid-ask spread within
//!    10% of the mid. A limit order at the ask; unfilled after a minute, no trade.
//! 3. Sell at the market when the stock trade closes (its stop, break-even
//!    stop or 15:45 exit), and in any case by 15:50 New York.
//! 4. The most a trade can lose is its premium. Losing trades come off the
//!    capital; profits are set aside and never traded.
//!
//! Backtest (3 Nov 2026, May - Oct 2026, the live ORB signals, $166 a trade,
//! Alpaca option bars, half-spread max(3%, $0.02) a side): 156 trades,
//! +$2,086 (+$686 then +$1,401 by half; 5 of 6 months up) against +$95 for the
//! same trades in stock. Fragile: 37% winners, the median trade -$15, and the
//! best three trades made +$2,131, so without them it lost $45. Still positive
//! at double the costs (+$574), negative at triple (-$1,340). Paper only.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc, Weekday};
use chrono_tz::America::New_York;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde_json::json;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::agent::kill_switch::KillSwitch;
use crate::config::UsOptionsConfig;
use crate::db::store::{OptionTrade, Store};
use crate::india::orb::trading_cash;
use crate::json_decimal::{de_decimal, de_decimal_opt};

pub const STRATEGY: &str = "us_options";
/// Trades a day, as the ORB and burst share between them.
const MAX_TRADES_PER_DAY: i64 = 3;
/// A stock entry older than this is not mirrored (after a restart, say).
const MIRROR_WINDOW_MINS: i64 = 5;
/// An entry limit still unfilled after this is cancelled: no trade.
const ENTRY_WAIT_SECS: i64 = 60;
/// Cheaper options are mostly spread.
const MIN_PREMIUM: Decimal = dec!(0.10);

/// One quoted contract: (symbol, strike, bid, ask).
pub type Quoted = (String, Decimal, Decimal, Decimal);

/// The contract to buy: the strike nearest `price` whose ask fits `budget`
/// for at least one contract, with a live bid and a spread within
/// `max_spread` of the mid. Returns (symbol, ask, contracts).
pub fn choose(
    quoted: &[Quoted],
    price: Decimal,
    budget: Decimal,
    max_spread: Decimal,
) -> Option<(String, Decimal, i64)> {
    quoted
        .iter()
        .filter(|(_, _, bid, ask)| {
            let mid = (*bid + *ask) / dec!(2);
            *bid > Decimal::ZERO
                && *ask >= MIN_PREMIUM
                && *ask * dec!(100) <= budget
                && (*ask - *bid) <= mid * max_spread
        })
        .min_by_key(|(_, strike, _, _)| (*strike - price).abs())
        .map(|(symbol, _, _, ask)| {
            let qty = (budget / (*ask * dec!(100))).floor().to_i64().unwrap_or(0);
            (symbol.clone(), *ask, qty)
        })
        .filter(|(_, _, qty)| *qty >= 1)
}

/// The strike band searched: at to out of the money, a little in.
pub fn strike_band(price: Decimal, is_call: bool) -> (Decimal, Decimal) {
    if is_call {
        (price * dec!(0.97), price * dec!(1.10))
    } else {
        (price * dec!(0.90), price * dec!(1.03))
    }
}

/// Whether a stock entry gets an option: a call always, a put unless the book
/// is set to calls only.
pub fn mirrors(is_call: bool, calls_only: bool) -> bool {
    is_call || !calls_only
}

/// Whether today's closed option trades have lost the book's daily limit.
pub fn daily_limit_hit(pnl_today: Decimal, limit: Decimal) -> bool {
    -pnl_today >= limit
}

/// P&L of a long option position in dollars (100 shares a contract).
pub fn pnl(entry: Decimal, exit: Decimal, qty: i64) -> Decimal {
    (exit - entry) * Decimal::from(qty) * dec!(100)
}

/// New York regular session, weekdays 9:30 to 16:00 (holidays fail harmlessly:
/// no stock entry to mirror, no quotes).
fn ny_minutes(now: DateTime<Utc>) -> Option<(NaiveDate, u32)> {
    let ny = now.with_timezone(&New_York);
    if matches!(ny.weekday(), Weekday::Sat | Weekday::Sun) {
        return None;
    }
    Some((ny.date_naive(), ny.hour() * 60 + ny.minute()))
}

const OPEN: u32 = 9 * 60 + 30;
const LAST_ENTRY: u32 = 15 * 60 + 30;
const FLAT_BY: u32 = 15 * 60 + 50;
const CLOSE: u32 = 16 * 60;

#[derive(Debug, Deserialize)]
pub(crate) struct Contract {
    pub(crate) symbol: String,
    pub(crate) expiration_date: NaiveDate,
    // Alpaca sends prices as strings ("137"), sometimes as numbers.
    #[serde(deserialize_with = "de_decimal")]
    pub(crate) strike_price: Decimal,
    #[serde(default)]
    pub(crate) tradable: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct Contracts {
    #[serde(default)]
    option_contracts: Vec<Contract>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OptionQuote {
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub(crate) bp: Option<Decimal>,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub(crate) ap: Option<Decimal>,
}

#[derive(Debug, Deserialize)]
struct QuotesResponse {
    #[serde(default)]
    quotes: std::collections::HashMap<String, OptionQuote>,
}

#[derive(Debug, Deserialize)]
struct LatestTrade {
    trade: StockTrade,
}

#[derive(Debug, Deserialize)]
struct StockTrade {
    #[serde(deserialize_with = "de_decimal")]
    p: Decimal,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OrderAck {
    pub(crate) id: String,
    pub(crate) status: String,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub(crate) filled_qty: Option<Decimal>,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub(crate) filled_avg_price: Option<Decimal>,
}

/// Alpaca's REST API for the options book: paper trading only.
pub struct AlpacaOptions {
    http: reqwest::Client,
    key: String,
    secret: String,
    pub(crate) trading: String,
    data: String,
}

impl AlpacaOptions {
    pub fn paper(key: String, secret: String, trading: String) -> Result<Self> {
        if !trading.contains("paper-api") {
            bail!("the options book is paper only, not {trading}");
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(StdDuration::from_secs(20))
                .build()
                .unwrap_or_default(),
            key,
            secret,
            trading: trading.trim_end_matches('/').to_string(),
            data: "https://data.alpaca.markets".to_string(),
        })
    }

    pub(crate) fn req(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .header("APCA-API-KEY-ID", &self.key)
            .header("APCA-API-SECRET-KEY", &self.secret)
    }

    pub(crate) async fn send<T: for<'de> Deserialize<'de>>(&self, rb: reqwest::RequestBuilder) -> Result<T> {
        let resp = rb.send().await.context("Alpaca options request failed")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Alpaca {status}: {}", body.chars().take(300).collect::<String>());
        }
        serde_json::from_str(&body).with_context(|| format!("unreadable Alpaca reply: {}", body.chars().take(200).collect::<String>()))
    }

    pub(crate) async fn last_price(&self, symbol: &str) -> Result<Decimal> {
        let url = format!("{}/v2/stocks/{symbol}/trades/latest?feed=iex", self.data);
        Ok(self.send::<LatestTrade>(self.req(reqwest::Method::GET, url)).await?.trade.p)
    }

    pub(crate) async fn contracts(
        &self,
        underlying: &str,
        is_call: bool,
        from: NaiveDate,
        to: NaiveDate,
        band: (Decimal, Decimal),
    ) -> Result<Vec<Contract>> {
        let url = format!(
            "{}/v2/options/contracts?underlying_symbols={underlying}&type={}&status=active\
             &expiration_date_gte={from}&expiration_date_lte={to}\
             &strike_price_gte={}&strike_price_lte={}&limit=500",
            self.trading,
            if is_call { "call" } else { "put" },
            band.0.round_dp(2),
            band.1.round_dp(2),
        );
        Ok(self.send::<Contracts>(self.req(reqwest::Method::GET, url)).await?.option_contracts)
    }

    pub(crate) async fn quotes(&self, symbols: &[String]) -> Result<std::collections::HashMap<String, OptionQuote>> {
        let url = format!(
            "{}/v1beta1/options/quotes/latest?symbols={}&feed=indicative",
            self.data,
            symbols.join(",")
        );
        Ok(self.send::<QuotesResponse>(self.req(reqwest::Method::GET, url)).await?.quotes)
    }

    async fn buy_limit(&self, contract: &str, qty: i64, limit: Decimal, client_order_id: &str) -> Result<OrderAck> {
        let body = json!({
            "symbol": contract, "qty": qty.to_string(), "side": "buy", "type": "limit",
            "limit_price": limit.to_string(), "time_in_force": "day",
            "client_order_id": client_order_id,
        });
        self.send(self.req(reqwest::Method::POST, format!("{}/v2/orders", self.trading)).json(&body))
            .await
    }

    async fn sell_market(&self, contract: &str, qty: i64, client_order_id: &str) -> Result<OrderAck> {
        let body = json!({
            "symbol": contract, "qty": qty.to_string(), "side": "sell", "type": "market",
            "time_in_force": "day", "client_order_id": client_order_id,
        });
        self.send(self.req(reqwest::Method::POST, format!("{}/v2/orders", self.trading)).json(&body))
            .await
    }

    pub(crate) async fn order(&self, id: &str) -> Result<OrderAck> {
        self.send(self.req(reqwest::Method::GET, format!("{}/v2/orders/{id}", self.trading)))
            .await
    }

    pub(crate) async fn cancel(&self, id: &str) -> Result<()> {
        let resp = self
            .req(reqwest::Method::DELETE, format!("{}/v2/orders/{id}", self.trading))
            .send()
            .await
            .context("Alpaca cancel failed")?;
        // 422: already filled or done; the next poll reads the final state.
        if !resp.status().is_success() && resp.status().as_u16() != 422 {
            bail!("Alpaca cancel {}", resp.status());
        }
        Ok(())
    }
}

pub fn spawn_us_options_worker(
    api: AlpacaOptions,
    store: Store,
    kill_switch: Arc<KillSwitch>,
    config: UsOptionsConfig,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(capital = %config.capital_usd, "U.S. options worker started (paper only, long premium)");
        loop {
            tokio::time::sleep(StdDuration::from_secs(10)).await;
            let now = Utc::now();
            if let Err(e) = manage(&api, &store, now).await {
                warn!(error = %format!("{e:#}"), "Options book: managing open trades failed");
            }
            let Some((today, mins)) = ny_minutes(now) else {
                continue;
            };
            if kill_switch.is_tripped() || !(OPEN + 5..LAST_ENTRY).contains(&mins) {
                continue;
            }
            if let Err(e) = mirror_entries(&api, &store, &config, today, now).await {
                warn!(error = %format!("{e:#}"), "Options book: entry pass failed");
            }
        }
    })
}

/// Follow every option row in flight: fills, unfilled entries, exits.
async fn manage(api: &AlpacaOptions, store: &Store, now: DateTime<Utc>) -> Result<()> {
    let live = store.live_option_trades().await?;
    if live.is_empty() {
        return Ok(());
    }
    let ny = ny_minutes(now);
    for t in live {
        if let Err(e) = manage_one(api, store, &t, now, ny).await {
            warn!(contract = %t.contract, error = %format!("{e:#}"), "Options book: could not update a trade");
        }
    }
    Ok(())
}

async fn manage_one(
    api: &AlpacaOptions,
    store: &Store,
    t: &OptionTrade,
    now: DateTime<Utc>,
    ny: Option<(NaiveDate, u32)>,
) -> Result<()> {
    match t.status.as_str() {
        "PENDING" => {
            let Some(id) = t.venue_order_id.as_deref() else {
                return store.option_cancelled(t.id, "never reached Alpaca").await;
            };
            let ack = api.order(id).await?;
            let filled = ack.filled_qty.unwrap_or_default().to_i64().unwrap_or(0);
            match ack.status.as_str() {
                "filled" => {
                    let px = ack.filled_avg_price.context("filled with no price")?;
                    info!(contract = %t.contract, qty = filled, price = %px, "Options book: bought");
                    store.option_opened(t.id, &px.to_string(), filled).await
                }
                "canceled" | "expired" | "rejected" | "done_for_day" => {
                    if filled > 0 {
                        let px = ack.filled_avg_price.context("part-filled with no price")?;
                        store.option_opened(t.id, &px.to_string(), filled).await
                    } else {
                        info!(contract = %t.contract, status = %ack.status, "Options book: entry not filled, no trade");
                        store.option_cancelled(t.id, &format!("entry {}", ack.status)).await
                    }
                }
                _ => {
                    let sent = DateTime::parse_from_rfc3339(&t.entry_at)
                        .map(|d| d.with_timezone(&Utc))
                        .unwrap_or(now);
                    if now - sent > Duration::seconds(ENTRY_WAIT_SECS) {
                        api.cancel(id).await?;
                    }
                    Ok(())
                }
            }
        }
        "OPEN" => {
            let stock_open = matches!(
                store.trade_status(t.stock_trade_id).await?.as_deref(),
                Some("OPEN") | Some("PARTIAL") | Some("PENDING")
            );
            let late = match ny {
                Some((day, mins)) => day.to_string() != t.day || mins >= FLAT_BY,
                None => true,
            };
            let in_session = ny.is_some_and(|(_, m)| (OPEN..CLOSE).contains(&m));
            if (stock_open && !late) || !in_session {
                return Ok(());
            }
            let reason = if stock_open { "FLAT_1550" } else { "STOCK_EXIT" };
            let cid = format!("opt-x-{}-{}", t.id, now.timestamp());
            let ack = api.sell_market(&t.contract, t.qty, &cid).await?;
            info!(contract = %t.contract, qty = t.qty, reason, "Options book: selling");
            store.option_closing(t.id, &cid, Some(&ack.id), reason).await
        }
        "CLOSING" => {
            let Some(id) = t.exit_order_id.as_deref() else {
                return store.option_reopen(t.id).await;
            };
            let ack = api.order(id).await?;
            match ack.status.as_str() {
                "filled" => {
                    let exit = ack.filled_avg_price.context("exit filled with no price")?;
                    let entry: Decimal = t.entry_price.as_deref().unwrap_or("0").parse()?;
                    let p = pnl(entry, exit, t.qty);
                    info!(contract = %t.contract, %entry, %exit, pnl = %p, "Options book: closed");
                    store.option_closed(t.id, &exit.to_string(), &p.round_dp(2).to_string(), now).await
                }
                "canceled" | "expired" | "rejected" | "done_for_day" => store.option_reopen(t.id).await,
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

/// Buy the option for each ORB / burst stock entry of the last few minutes.
async fn mirror_entries(
    api: &AlpacaOptions,
    store: &Store,
    config: &UsOptionsConfig,
    today: NaiveDate,
    now: DateTime<Utc>,
) -> Result<()> {
    let fresh = store
        .us_orb_stock_trades_since(now - Duration::minutes(MIRROR_WINDOW_MINS))
        .await?;
    if fresh.is_empty() {
        return Ok(());
    }
    let done = store.option_stock_ids().await?;
    for (stock_id, symbol, side, status) in fresh {
        if done.contains(&stock_id) || !matches!(status.as_str(), "OPEN" | "PARTIAL") {
            continue;
        }
        let (n_today, pnl_today, _, losses) = store.option_totals(&today.to_string()).await?;
        if n_today >= MAX_TRADES_PER_DAY {
            info!(%symbol, "Options book: 3 trades today already; not mirroring");
            return Ok(());
        }
        if daily_limit_hit(pnl_today, config.max_daily_loss_usd) {
            info!(%symbol, pnl_today = %pnl_today, limit = %config.max_daily_loss_usd,
                  "Options book: daily loss limit reached; not mirroring");
            return Ok(());
        }
        let budget = trading_cash(config.capital_usd, losses) / Decimal::from(MAX_TRADES_PER_DAY);
        let is_call = side.eq_ignore_ascii_case("BUY");
        if !mirrors(is_call, config.calls_only) {
            info!(%symbol, "Options book: calls only; no put on a short breakdown");
            continue;
        }
        if let Err(e) = enter(api, store, config, today, now, stock_id, &symbol, is_call, budget).await {
            warn!(%symbol, error = %format!("{e:#}"), "Options book: entry failed");
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn enter(
    api: &AlpacaOptions,
    store: &Store,
    config: &UsOptionsConfig,
    today: NaiveDate,
    now: DateTime<Utc>,
    stock_id: i64,
    symbol: &str,
    is_call: bool,
    budget: Decimal,
) -> Result<()> {
    let price = api.last_price(symbol).await?;
    let contracts = api
        .contracts(
            symbol,
            is_call,
            today + Duration::days(config.min_days_to_expiry),
            today + Duration::days(config.max_days_to_expiry),
            strike_band(price, is_call),
        )
        .await?;
    let Some(expiry) = contracts
        .iter()
        .filter(|c| c.tradable != Some(false))
        .map(|c| c.expiration_date)
        .min()
    else {
        info!(%symbol, "Options book: no weekly contract in range; skipping");
        return store_skip(store, today, stock_id, symbol, is_call, now, "no contract").await;
    };
    let nearest: Vec<&Contract> = contracts
        .iter()
        .filter(|c| c.expiration_date == expiry && c.tradable != Some(false))
        .collect();
    let symbols: Vec<String> = nearest.iter().map(|c| c.symbol.clone()).collect();
    let quotes = api.quotes(&symbols).await?;
    let quoted: Vec<Quoted> = nearest
        .iter()
        .filter_map(|c| {
            let q = quotes.get(&c.symbol)?;
            Some((c.symbol.clone(), c.strike_price, q.bp?, q.ap?))
        })
        .collect();
    let Some((contract, ask, qty)) = choose(&quoted, price, budget, config.max_spread_pct) else {
        info!(%symbol, %budget, "Options book: nothing affordable with a tight spread; skipping");
        return store_skip(store, today, stock_id, symbol, is_call, now, "nothing affordable").await;
    };
    let cid = format!("opt-{stock_id}-{}", now.timestamp());
    let row = store
        .insert_option_trade(
            &today.to_string(), stock_id, symbol, &contract, is_call, qty, &ask.to_string(), &cid, None, now,
        )
        .await?;
    match api.buy_limit(&contract, qty, ask, &cid).await {
        Ok(ack) => {
            info!(%symbol, %contract, qty, limit = %ask, "Options book: buy order sent");
            store.set_option_order_id(row, &ack.id).await
        }
        Err(e) => {
            store.option_cancelled(row, "order refused").await?;
            Err(e)
        }
    }
}

/// A stock entry the book looked at and passed on: recorded so it is not
/// tried again every pass, and shown on the dashboard.
async fn store_skip(
    store: &Store,
    today: NaiveDate,
    stock_id: i64,
    symbol: &str,
    is_call: bool,
    now: DateTime<Utc>,
    why: &str,
) -> Result<()> {
    let row = store
        .insert_option_trade(&today.to_string(), stock_id, symbol, "-", is_call, 0, "0", &format!("opt-skip-{stock_id}"), None, now)
        .await?;
    store.option_cancelled(row, &format!("skipped: {why}")).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_only_skips_the_puts() {
        assert!(mirrors(true, true));
        assert!(!mirrors(false, true), "no put on a short when calls only");
        assert!(mirrors(false, false), "both sides when the switch is off");
    }

    #[test]
    fn the_daily_limit_stops_entries_once_reached() {
        assert!(!daily_limit_hit(dec!(-99.99), dec!(100)));
        assert!(daily_limit_hit(dec!(-100), dec!(100)));
        assert!(!daily_limit_hit(dec!(250), dec!(100)), "profits never stop it");
    }

    #[test]
    fn reads_alpaca_string_prices() {
        let c: Contracts = serde_json::from_str(
            r#"{"option_contracts":[{"symbol":"SHOP261106C00137000","tradable":true,
               "expiration_date":"2026-11-06","strike_price":"137"}]}"#,
        )
        .unwrap();
        assert_eq!(c.option_contracts[0].strike_price, dec!(137));
        let o: OrderAck = serde_json::from_str(
            r#"{"id":"x","status":"filled","filled_qty":"2","filled_avg_price":"1.35"}"#,
        )
        .unwrap();
        assert_eq!(o.filled_avg_price, Some(dec!(1.35)));
        let q: OptionQuote = serde_json::from_str(r#"{"bp":1.3,"ap":1.4}"#).unwrap();
        assert_eq!(q.ap, Some(dec!(1.4)));
    }

    fn q(sym: &str, k: Decimal, bid: Decimal, ask: Decimal) -> Quoted {
        (sym.to_string(), k, bid, ask)
    }

    #[test]
    fn picks_the_affordable_strike_nearest_the_money() {
        let quoted = vec![
            q("ATM", dec!(100), dec!(2.40), dec!(2.50)), // $250 a contract: over budget
            q("OTM1", dec!(102), dec!(1.50), dec!(1.55)),
            q("OTM2", dec!(105), dec!(0.70), dec!(0.74)),
        ];
        let (sym, ask, qty) = choose(&quoted, dec!(100.5), dec!(166), dec!(0.10)).unwrap();
        assert_eq!((sym.as_str(), ask, qty), ("OTM1", dec!(1.55), 1));
    }

    #[test]
    fn wide_spreads_dead_bids_and_pennies_are_refused() {
        let quoted = vec![
            q("WIDE", dec!(100), dec!(1.00), dec!(1.40)),
            q("NOBID", dec!(101), dec!(0), dec!(1.00)),
            q("PENNY", dec!(110), dec!(0.04), dec!(0.05)),
        ];
        assert!(choose(&quoted, dec!(100), dec!(166), dec!(0.10)).is_none());
    }

    #[test]
    fn several_contracts_when_cheap_and_never_over_budget() {
        let quoted = vec![q("C", dec!(50), dec!(0.40), dec!(0.42))];
        let (_, _, qty) = choose(&quoted, dec!(50), dec!(166), dec!(0.10)).unwrap();
        assert_eq!(qty, 3); // 3 x $42 = $126; a 4th would be $168
    }

    #[test]
    fn option_pnl_is_per_hundred_shares() {
        assert_eq!(pnl(dec!(1.50), dec!(2.10), 2), dec!(120));
        assert_eq!(pnl(dec!(1.50), dec!(0.30), 1), dec!(-120));
    }

    #[test]
    fn strike_band_leans_out_of_the_money() {
        assert_eq!(strike_band(dec!(100), true), (dec!(97.00), dec!(110.00)));
        assert_eq!(strike_band(dec!(100), false), (dec!(90.00), dec!(103.00)));
    }
}
