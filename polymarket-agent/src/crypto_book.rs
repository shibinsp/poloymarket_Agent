//! The crypto books: BTC and ETH on a slow momentum rule, $20 each, paper only.
//!
//! Rules:
//! 1. Once a day, after the daily candle closes (00:10 UTC, 05:40 IST), each
//!    coin is held while its last daily close is above the close 60 days
//!    earlier, otherwise the book sits in cash. Long or cash, no leverage.
//! 2. The `crypto` book trades it on the Alpaca paper account, with a news
//!    guard: the day's headlines and the market picture go to the model, and
//!    a *severe* specific event (an exchange collapse or hack, a ban, a major
//!    regulatory action, a stablecoin depeg) vetoes a buy or moves the coin
//!    to cash. While holding, the news is read again every hour. News can only
//!    take risk off; it never buys what the rule does not.
//! 3. The `crypto_rule` book follows the rule alone, filled on paper at the
//!    same prices, so whether the news guard helps is measured, not assumed.
//! 4. Each coin gets half the book's trading cash: losing trades come off the
//!    capital; profits are set aside and never traded.
//! 5. Three rival rules (`RIVALS`) run the same way on paper, to pick the best
//!    rule on live results.
//!
//! Backtest (3 Nov 2026, Alpaca daily bars Jan 2021 - Oct 2026, $20, 0.20%
//! a side; the same at 0.35%): the 60-day rule passed on both coins - both
//! halves positive, all nearby settings (30/60/90 days) positive, walk-forward
//! positive. BTC $20 -> $51 (holding: $53) with the worst fall -61% against
//! -77%; ETH $20 -> $100 (holding: $62), worst fall -56% against -79%. Choppy
//! years make about nothing (BTC 2025 +5%, 2026 +2%). The news guard is not
//! backtested: that is what the shadow book is for.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Timelike, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde_json::json;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::agent::kill_switch::KillSwitch;
use crate::config::CryptoConfig;
use crate::db::store::Store;
use crate::india::orb::trading_cash;
use crate::json_decimal::{de_decimal, de_decimal_opt};
use crate::valuation::llm::LlmClient;

pub const BOOK: &str = "crypto";
pub const SHADOW: &str = "crypto_rule";
/// Alpaca's taker fee at the entry tier, charged on the shadow book's fills.
const FEE: Decimal = dec!(0.0025);
/// Alpaca's smallest crypto order is about $1.
const MIN_ORDER_USD: Decimal = dec!(2);

/// The rule: hold while the last close is above the close `lookback` days
/// before it. `closes` are completed daily closes, oldest first.
pub fn momentum_holds(closes: &[Decimal], lookback: usize) -> Option<bool> {
    let n = closes.len();
    (n > lookback).then(|| closes[n - 1] > closes[n - 1 - lookback])
}

/// Rival rules run side by side on paper from 4 Nov 2026, so the best one is
/// picked on live results, not on the backtest alone. Each passed on at least
/// one coin in `crypto_best.py` (2021 - Oct 2026, 0.35% a side); only the
/// 60-day rule passed on both, so it stays the one that trades.
/// (book, short description of the rule)
pub const RIVALS: [(&str, &str); 3] = [
    ("crypto_sma50", "above its 50-day average"),
    (
        "crypto_donchian",
        "20-day closing high in, 10-day closing low out",
    ),
    (
        "crypto_mom_sma20",
        "above 60 days ago or above its 20-day average",
    ),
];

/// Whether a rival book wants to hold, from completed daily closes (oldest
/// first) and whether it already holds. None: not enough history.
pub fn rival_wants(book: &str, closes: &[Decimal], holding: bool) -> Option<bool> {
    let n = closes.len();
    let last = *closes.last()?;
    let avg = |k: usize| {
        (n >= k).then(|| closes[n - k..].iter().sum::<Decimal>() / Decimal::from(k as i64))
    };
    match book {
        "crypto_sma50" => Some(last > avg(50)?),
        "crypto_donchian" => {
            if holding {
                // Out on a close below the lowest of the 10 closes before it.
                (n > 10).then(|| last >= *closes[n - 11..n - 1].iter().min().unwrap_or(&last))
            } else {
                (n >= 20).then(|| last >= *closes[n - 20..].iter().max().unwrap_or(&last))
            }
        }
        "crypto_mom_sma20" => Some(momentum_holds(closes, 60)? || last > avg(20)?),
        _ => None,
    }
}

/// Change from `days` closes ago to `price`.
pub fn change(closes: &[Decimal], days: usize, price: Decimal) -> Option<Decimal> {
    let n = closes.len();
    (n >= days && !closes[n - days].is_zero()).then(|| price / closes[n - days] - Decimal::ONE)
}

/// What the news guard concluded.
#[derive(Debug, Clone, PartialEq)]
pub struct NewsRead {
    /// normal, severe, or unknown (no answer: the rule decides alone)
    pub risk: String,
    pub reason: String,
    pub view: String,
}

/// Pull `{"risk": ..., "reason": ..., "view": ...}` out of a model reply,
/// which may wrap it in reasoning or a code fence.
pub fn parse_news_read(text: &str) -> Option<NewsRead> {
    let mut best = None;
    for (i, _) in text.match_indices('{') {
        if let Some(end) = text[i..].find('}') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text[i..i + end + 1]) {
                if let Some(risk) = v.get("risk").and_then(|r| r.as_str()) {
                    let risk = risk.trim().to_ascii_lowercase();
                    if risk == "normal" || risk == "severe" {
                        best = Some(NewsRead {
                            risk,
                            reason: v
                                .get("reason")
                                .and_then(|r| r.as_str())
                                .unwrap_or("")
                                .to_string(),
                            view: v
                                .get("view")
                                .and_then(|r| r.as_str())
                                .unwrap_or("")
                                .to_string(),
                        });
                    }
                }
            }
        }
    }
    best
}

#[derive(Debug, Deserialize)]
struct Bar {
    t: DateTime<Utc>,
    #[serde(deserialize_with = "de_decimal")]
    c: Decimal,
}

#[derive(Debug, Deserialize)]
struct BarsResponse {
    #[serde(default)]
    bars: std::collections::HashMap<String, Vec<Bar>>,
}

#[derive(Debug, Deserialize)]
struct Trade {
    #[serde(deserialize_with = "de_decimal")]
    p: Decimal,
}

#[derive(Debug, Deserialize)]
struct TradesResponse {
    #[serde(default)]
    trades: std::collections::HashMap<String, Trade>,
}

#[derive(Debug, Deserialize)]
struct OrderAck {
    id: String,
    #[serde(default)]
    client_order_id: String,
    status: String,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    filled_qty: Option<Decimal>,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    filled_avg_price: Option<Decimal>,
}

#[derive(Debug, Deserialize)]
struct PositionRow {
    #[serde(deserialize_with = "de_decimal")]
    qty: Decimal,
}

/// Alpaca's REST API and Google News for the crypto book: paper trading only.
pub struct CryptoApi {
    http: reqwest::Client,
    key: String,
    secret: String,
    trading: String,
    data: String,
}

impl CryptoApi {
    pub fn paper(key: String, secret: String, trading: String) -> Result<Self> {
        if !trading.contains("paper-api") {
            bail!("the crypto book is paper only, not {trading}");
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(StdDuration::from_secs(30))
                .build()
                .unwrap_or_default(),
            key,
            secret,
            trading: trading.trim_end_matches('/').to_string(),
            data: "https://data.alpaca.markets".to_string(),
        })
    }

    fn req(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .header("APCA-API-KEY-ID", &self.key)
            .header("APCA-API-SECRET-KEY", &self.secret)
    }

    async fn send<T: for<'de> Deserialize<'de>>(&self, rb: reqwest::RequestBuilder) -> Result<T> {
        let resp = rb.send().await.context("Alpaca crypto request failed")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!(
                "Alpaca {status}: {}",
                body.chars().take(300).collect::<String>()
            );
        }
        serde_json::from_str(&body).with_context(|| {
            format!(
                "unreadable Alpaca reply: {}",
                body.chars().take(200).collect::<String>()
            )
        })
    }

    /// Completed daily closes, oldest first (today's unfinished bar dropped).
    async fn daily_closes(&self, symbol: &str, now: DateTime<Utc>) -> Result<Vec<Decimal>> {
        let start = (now - Duration::days(120)).format("%Y-%m-%dT00:00:00Z");
        let url = format!(
            "{}/v1beta3/crypto/us/bars?symbols={}&timeframe=1Day&start={start}&limit=500",
            self.data,
            urlencoding::encode(symbol)
        );
        let mut bars = self
            .send::<BarsResponse>(self.req(reqwest::Method::GET, url))
            .await?
            .bars
            .remove(symbol)
            .unwrap_or_default();
        let today = now.date_naive();
        bars.retain(|b| b.t.date_naive() < today);
        bars.sort_by_key(|b| b.t);
        Ok(bars.into_iter().map(|b| b.c).collect())
    }

    async fn last_price(&self, symbol: &str) -> Result<Decimal> {
        let url = format!(
            "{}/v1beta3/crypto/us/latest/trades?symbols={}",
            self.data,
            urlencoding::encode(symbol)
        );
        self.send::<TradesResponse>(self.req(reqwest::Method::GET, url))
            .await?
            .trades
            .remove(symbol)
            .map(|t| t.p)
            .context("no latest trade")
    }

    async fn buy_notional(
        &self,
        symbol: &str,
        usd: Decimal,
        client_order_id: &str,
    ) -> Result<OrderAck> {
        let body = json!({
            "symbol": symbol, "notional": usd.round_dp(2).to_string(), "side": "buy",
            "type": "market", "time_in_force": "gtc", "client_order_id": client_order_id,
        });
        self.send(
            self.req(reqwest::Method::POST, format!("{}/v2/orders", self.trading))
                .json(&body),
        )
        .await
    }

    /// Sell the whole position (Alpaca's own close, so no dust is left behind).
    async fn close_position(&self, symbol: &str) -> Result<OrderAck> {
        let url = format!("{}/v2/positions/{}", self.trading, symbol.replace('/', ""));
        self.send(self.req(reqwest::Method::DELETE, url)).await
    }

    async fn position_qty(&self, symbol: &str) -> Result<Option<Decimal>> {
        let url = format!("{}/v2/positions/{}", self.trading, symbol.replace('/', ""));
        let resp = self
            .req(reqwest::Method::GET, url)
            .send()
            .await
            .context("position request failed")?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let body = resp.text().await.unwrap_or_default();
        Ok(Some(
            serde_json::from_str::<PositionRow>(&body)
                .context("unreadable position")?
                .qty,
        ))
    }

    async fn order(&self, id: &str) -> Result<OrderAck> {
        self.send(self.req(
            reqwest::Method::GET,
            format!("{}/v2/orders/{id}", self.trading),
        ))
        .await
    }

    /// Poll a market order until it fills (up to a minute).
    async fn wait_fill(&self, mut ack: OrderAck) -> Result<OrderAck> {
        for _ in 0..12 {
            if ack.status == "filled" {
                return Ok(ack);
            }
            if matches!(ack.status.as_str(), "canceled" | "expired" | "rejected") {
                bail!("order {} ended {}", ack.id, ack.status);
            }
            tokio::time::sleep(StdDuration::from_secs(5)).await;
            ack = self.order(&ack.id).await?;
        }
        bail!(
            "order {} not filled after a minute ({})",
            ack.id,
            ack.status
        )
    }

    /// The day's headlines for a coin from Google News (titles only).
    async fn headlines(&self, coin: &str) -> Vec<String> {
        let url = format!(
            "https://news.google.com/rss/search?q={}&hl=en-US&gl=US&ceid=US:en",
            urlencoding::encode(&format!("{coin} crypto when:1d"))
        );
        let body = match self.http.get(url).send().await {
            Ok(r) => r.text().await.unwrap_or_default(),
            Err(e) => {
                warn!(coin, error = %e, "Crypto news fetch failed");
                return Vec::new();
            }
        };
        body.split("<item>")
            .skip(1)
            .filter_map(|item| {
                let s = item.find("<title>")? + 7;
                let e = item.find("</title>")?;
                (s < e).then(|| {
                    item[s..e]
                        .replace("<![CDATA[", "")
                        .replace("]]>", "")
                        .trim()
                        .to_string()
                })
            })
            .take(12)
            .collect()
    }
}

fn coin_name(symbol: &str) -> &'static str {
    match symbol {
        "BTC/USD" => "bitcoin",
        "ETH/USD" => "ethereum",
        _ => "crypto",
    }
}

const NEWS_SYSTEM: &str = "You are the risk officer of a small, long-only crypto book that holds a coin \
while its price is above its level 60 days ago. Read the last day's headlines and the market figures. \
Decide one thing: is there a SEVERE, specific negative event in the last 24 hours that makes a large \
further fall likely soon - for example a major exchange collapse or hack, a ban or major enforcement \
action, a large stablecoin losing its peg, a protocol failure, or a market-wide liquidation crash? \
Ordinary price moves, predictions, analyst opinions, ETF flow chatter and old news are NOT severe. \
Most days are normal. Reply with JSON only, no other text: \
{\"risk\": \"normal\" or \"severe\", \"reason\": \"one short sentence\", \"view\": \"one short sentence on where the market is going and why\"}";

/// Words that usually mean a specific event rather than price chatter.
const RED_FLAGS: [&str; 12] = [
    "hack",
    "heist",
    "exploit",
    "stolen",
    "drained",
    "depeg",
    "insolven",
    "bankrupt",
    "halts withdrawals",
    "suspends withdrawals",
    "ban",
    "liquidation cascade",
];

/// The headlines carrying a red-flag word, so the model is pointed at them.
/// On 3 Nov 2026 it read "Binance customers withdraw $7.4 billion after
/// crypto heist" as a normal day.
pub fn red_flag_headlines(headlines: &[String]) -> Vec<&String> {
    headlines
        .iter()
        .filter(|h| {
            let l = h.to_ascii_lowercase();
            RED_FLAGS.iter().any(|w| {
                // "ban" only as a word, not inside "bank" or "urban".
                if *w == "ban" {
                    l.split(|c: char| !c.is_ascii_alphabetic())
                        .any(|t| t == "ban" || t == "bans" || t == "banned")
                } else {
                    l.contains(w)
                }
            })
        })
        .collect()
}

async fn read_news(
    llm: Option<&LlmClient>,
    coin: &str,
    price: Decimal,
    chg: (Option<Decimal>, Option<Decimal>, Option<Decimal>),
    headlines: &[String],
) -> NewsRead {
    let unknown = |reason: &str| NewsRead {
        risk: "unknown".into(),
        reason: reason.into(),
        view: String::new(),
    };
    let Some(llm) = llm else {
        return unknown("no model configured");
    };
    if headlines.is_empty() {
        return unknown("no headlines could be fetched");
    }
    let pct = |x: Option<Decimal>| {
        x.map(|v| format!("{:+.1}%", v * Decimal::ONE_HUNDRED))
            .unwrap_or("n/a".into())
    };
    let flagged = red_flag_headlines(headlines);
    let flag_note = if flagged.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nThese headlines name a possible specific event. Decide whether each is real, new (last 24h) \
             and large enough to be severe; do not dismiss them as ordinary news without saying why:\n- {}",
            flagged.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n- ")
        )
    };
    let user = format!(
        "Coin: {coin}. Price ${price}. Change: 24h {}, 7 days {}, 60 days {}.\nHeadlines (last 24h):\n- {}{flag_note}",
        pct(chg.0),
        pct(chg.1),
        pct(chg.2),
        headlines.join("\n- ")
    );
    match llm.complete(NEWS_SYSTEM, &user, None).await {
        Ok(resp) => parse_news_read(&resp.text)
            .unwrap_or_else(|| unknown("the model's reply had no verdict")),
        Err(e) => {
            warn!(coin, error = %format!("{e:#}"), "Crypto news read failed");
            unknown("the model did not answer")
        }
    }
}

pub fn spawn_crypto_worker(
    api: CryptoApi,
    llm: Option<Arc<LlmClient>>,
    store: Store,
    kill_switch: Arc<KillSwitch>,
    config: CryptoConfig,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(capital = %config.capital_usd, coins = ?config.symbols, news_guard = config.news_guard,
              "Crypto worker started (paper only, long or cash)");
        loop {
            let now = Utc::now();
            for symbol in &config.symbols {
                if let Err(e) = tick(
                    &api,
                    llm.as_deref(),
                    &store,
                    &kill_switch,
                    &config,
                    symbol,
                    now,
                )
                .await
                {
                    warn!(symbol = %symbol, error = %format!("{e:#}"), "Crypto book check failed");
                }
            }
            tokio::time::sleep(StdDuration::from_secs(300)).await;
        }
    })
}

async fn tick(
    api: &CryptoApi,
    llm: Option<&LlmClient>,
    store: &Store,
    kill_switch: &KillSwitch,
    config: &CryptoConfig,
    symbol: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let today = now.date_naive().to_string();
    let daily_done = store
        .crypto_last_check(symbol, "daily")
        .await?
        .is_some_and(|(day, _)| day == today);
    // The daily candle closes at 00:00 UTC; a few minutes for the bar to settle.
    let daily_due = !daily_done && (now.hour() > 0 || now.minute() >= 10);
    let holding = store.crypto_open(BOOK, symbol).await?.is_some();
    let hourly_due = !daily_due
        && holding
        && config.news_guard
        && store
            .crypto_last_check(symbol, "hourly")
            .await?
            .or(store.crypto_last_check(symbol, "daily").await?)
            .and_then(|(_, at)| DateTime::parse_from_rfc3339(&at).ok())
            .map_or(true, |at| {
                now - at.with_timezone(&Utc) >= Duration::minutes(60)
            });
    if !daily_due && !hourly_due {
        return Ok(());
    }

    let closes = api.daily_closes(symbol, now).await?;
    let price = api.last_price(symbol).await?;
    let rule = momentum_holds(&closes, config.lookback_days)
        .with_context(|| format!("only {} daily closes", closes.len()))?;
    let chg = (
        change(&closes, 1, price),
        change(&closes, 7, price),
        change(&closes, config.lookback_days, price),
    );
    let heads = if config.news_guard {
        api.headlines(coin_name(symbol)).await
    } else {
        Vec::new()
    };
    let news = if config.news_guard {
        read_news(llm, coin_name(symbol), price, chg, &heads).await
    } else {
        NewsRead {
            risk: "unknown".into(),
            reason: "news guard off".into(),
            view: String::new(),
        }
    };
    let severe = news.risk == "severe";
    let rule_says = if rule { "HOLD" } else { "CASH" };
    let mut actions = Vec::new();

    if daily_due {
        // The shadow book: the rule alone, filled at this price less the fee.
        paper_fill(
            store,
            config,
            SHADOW,
            symbol,
            rule,
            price,
            "above 60 days ago",
            &mut actions,
        )
        .await?;
        for (book, about) in RIVALS {
            let holding = store.crypto_open(book, symbol).await?.is_some();
            match rival_wants(book, &closes, holding) {
                Some(want) => {
                    paper_fill(
                        store,
                        config,
                        book,
                        symbol,
                        want,
                        price,
                        about,
                        &mut actions,
                    )
                    .await?
                }
                None => warn!(
                    book,
                    symbol,
                    closes = closes.len(),
                    "Not enough history for a rival crypto rule"
                ),
            }
        }
    }

    // The real book: the rule, with news able only to take risk off.
    let want = rule && !severe;
    match store.crypto_open(BOOK, symbol).await? {
        None if want && daily_due => {
            let budget = budget(store, BOOK, config).await?;
            // A buy whose fill was not confirmed last time is already held:
            // adopt it rather than buy a second one.
            let held = api
                .position_qty(symbol)
                .await?
                .filter(|q| *q > Decimal::ZERO);
            if let Some(qty) = held {
                store
                    .crypto_insert_open(
                        BOOK,
                        symbol,
                        qty,
                        budget,
                        price,
                        "adopted the position already held",
                        None,
                    )
                    .await?;
                actions.push(format!("crypto adopted {qty} already held"));
            } else if kill_switch.is_tripped() {
                actions.push("crypto: no buy, the agent is halted".into());
            } else if budget < MIN_ORDER_USD {
                actions.push(format!(
                    "crypto: no buy, ${} left to trade",
                    budget.round_dp(2)
                ));
            } else {
                let cid = format!(
                    "crypto-{}-{}",
                    symbol.replace('/', "").to_lowercase(),
                    now.timestamp()
                );
                let ack = api
                    .wait_fill(api.buy_notional(symbol, budget, &cid).await?)
                    .await?;
                let fill = ack.filled_avg_price.unwrap_or(price);
                let qty = match api.position_qty(symbol).await {
                    Ok(Some(q)) => q,
                    _ => ack.filled_qty.unwrap_or_default(),
                };
                store
                    .crypto_insert_open(
                        BOOK,
                        symbol,
                        qty,
                        budget,
                        fill,
                        "rule: above 60 days ago; news normal",
                        Some(&cid),
                    )
                    .await?;
                info!(symbol, usd = %budget.round_dp(2), price = %fill, "Crypto book bought");
                actions.push(format!(
                    "crypto bought ${} at {}",
                    budget.round_dp(2),
                    fill.round_dp(2)
                ));
            }
        }
        None if rule && severe && daily_due => {
            actions.push(format!("crypto: buy vetoed by news ({})", news.reason))
        }
        Some((id, _, cost)) if !want => {
            let why = if severe {
                format!("news: {}", news.reason)
            } else {
                "rule: below 60 days ago".to_string()
            };
            let ack = api.wait_fill(api.close_position(symbol).await?).await?;
            let fill = ack.filled_avg_price.unwrap_or(price);
            let proceeds = ack.filled_qty.unwrap_or_default() * fill;
            let cid = (!ack.client_order_id.is_empty()).then_some(ack.client_order_id.as_str());
            store
                .crypto_close(id, fill, proceeds, cost, &why, cid)
                .await?;
            info!(symbol, proceeds = %proceeds.round_dp(2), reason = %why, "Crypto book sold");
            actions.push(format!("crypto sold for ${} ({why})", proceeds.round_dp(2)));
        }
        _ => {}
    }

    let action = if actions.is_empty() {
        "no change".to_string()
    } else {
        actions.join("; ")
    };
    info!(symbol, kind = if daily_due { "daily" } else { "hourly" }, rule = rule_says, news = %news.risk,
          price = %price, action = %action, "Crypto check");
    store
        .insert_crypto_check(
            &today,
            symbol,
            if daily_due { "daily" } else { "hourly" },
            price,
            chg,
            rule_says,
            &news.risk,
            &news.reason,
            &news.view,
            &heads
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
            &action,
        )
        .await
}

/// Move a paper book to what its rule wants, filled at `price` less the fee.
#[allow(clippy::too_many_arguments)]
async fn paper_fill(
    store: &Store,
    config: &CryptoConfig,
    book: &str,
    symbol: &str,
    want: bool,
    price: Decimal,
    about: &str,
    actions: &mut Vec<String>,
) -> Result<()> {
    let budget = budget(store, book, config).await?;
    match (want, store.crypto_open(book, symbol).await?) {
        (true, None) if budget >= MIN_ORDER_USD => {
            let qty = budget * (Decimal::ONE - FEE) / price;
            store
                .crypto_insert_open(
                    book,
                    symbol,
                    qty.round_dp(9),
                    budget,
                    price,
                    &format!("rule: {about}"),
                    None,
                )
                .await?;
            actions.push(format!("{book} bought ${}", budget.round_dp(2)));
        }
        (false, Some((id, qty, cost))) => {
            let proceeds = qty * price * (Decimal::ONE - FEE);
            store
                .crypto_close(
                    id,
                    price,
                    proceeds,
                    cost,
                    &format!("rule no longer: {about}"),
                    None,
                )
                .await?;
            actions.push(format!("{book} sold for ${}", proceeds.round_dp(2)));
        }
        _ => {}
    }
    Ok(())
}

/// One coin's share of a book's trading cash.
async fn budget(store: &Store, book: &str, config: &CryptoConfig) -> Result<Decimal> {
    let (losses, _) = store.crypto_realized(book).await?;
    let coins = Decimal::from(config.symbols.len().max(1) as i64);
    Ok((trading_cash(config.capital_usd, losses) / coins).round_dp(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closes(xs: &[i64]) -> Vec<Decimal> {
        xs.iter().map(|x| Decimal::from(*x)).collect()
    }

    #[test]
    fn holds_only_above_the_close_lookback_days_earlier() {
        assert_eq!(momentum_holds(&closes(&[100, 90, 95, 101]), 3), Some(true));
        assert_eq!(
            momentum_holds(&closes(&[100, 120, 130, 99]), 3),
            Some(false)
        );
        assert_eq!(
            momentum_holds(&closes(&[100, 101]), 3),
            None,
            "not enough history"
        );
    }

    #[test]
    fn rival_rules_read_the_closes_as_backtested() {
        // 59 closes at 100, then 60 at 90: below the 50-day average and 60 days ago.
        let mut c: Vec<Decimal> = std::iter::repeat_n(dec!(100), 59)
            .chain(std::iter::repeat_n(dec!(90), 60))
            .collect();
        assert_eq!(rival_wants("crypto_sma50", &c, false), Some(false));
        assert_eq!(rival_wants("crypto_mom_sma20", &c, false), Some(false));
        // A jump to a new 20-day high: in on all three.
        c.push(dec!(95));
        assert_eq!(rival_wants("crypto_donchian", &c, false), Some(true));
        assert_eq!(
            rival_wants("crypto_mom_sma20", &c, false),
            Some(true),
            "above the 20-day average"
        );
        // Held, it stays in until a close under the 10 closes before it.
        c.push(dec!(91));
        assert_eq!(rival_wants("crypto_donchian", &c, true), Some(true));
        c.push(dec!(89));
        assert_eq!(rival_wants("crypto_donchian", &c, true), Some(false));
        assert_eq!(
            rival_wants("crypto_sma50", &c[..10], false),
            None,
            "not enough history"
        );
        assert_eq!(rival_wants("crypto_unknown", &c, false), None);
    }

    #[test]
    fn red_flag_words_pick_out_event_headlines() {
        let h: Vec<String> = [
            "Binance customers withdraw $7.4 billion after crypto heist - CBS News",
            "Current price of Ethereum for November 3, 2026 - Fortune",
            "Bank of America raises bitcoin target",
            "Country X bans crypto trading",
            "Stablecoin USDX depegs to $0.91",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let flagged: Vec<&str> = red_flag_headlines(&h)
            .into_iter()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(flagged, vec![h[0].as_str(), h[3].as_str(), h[4].as_str()]);
    }

    #[test]
    fn reads_the_verdict_out_of_a_wordy_reply() {
        let r = parse_news_read("Thinking... {\"note\": 1}\n```json\n{\"risk\": \"Severe\", \"reason\": \"exchange hacked\", \"view\": \"down\"}\n```").unwrap();
        assert_eq!(r.risk, "severe");
        assert_eq!(r.reason, "exchange hacked");
        assert!(parse_news_read("no json here").is_none());
        assert!(
            parse_news_read("{\"risk\": \"maybe\"}").is_none(),
            "only normal or severe count"
        );
    }

    #[test]
    fn change_is_from_that_many_closes_back() {
        let c = closes(&[100, 110, 120]);
        assert_eq!(change(&c, 1, dec!(132)), Some(dec!(0.1)));
        assert_eq!(change(&c, 3, dec!(50)), Some(dec!(-0.5)));
        assert_eq!(change(&c, 4, dec!(50)), None);
    }

    #[tokio::test]
    async fn the_budget_loses_losses_but_never_grows_with_profits() {
        let store = Store::new(":memory:").await.unwrap();
        let cfg = CryptoConfig::default();
        assert_eq!(budget(&store, BOOK, &cfg).await.unwrap(), dec!(10));
        let id = store
            .crypto_insert_open(
                BOOK,
                "BTC/USD",
                dec!(0.0001),
                dec!(10),
                dec!(100000),
                "t",
                None,
            )
            .await
            .unwrap();
        store
            .crypto_close(id, dec!(80000), dec!(8), dec!(10), "t", None)
            .await
            .unwrap();
        assert_eq!(
            budget(&store, BOOK, &cfg).await.unwrap(),
            dec!(9),
            "a $2 loss comes off the $20"
        );
        let id = store
            .crypto_insert_open(BOOK, "ETH/USD", dec!(0.01), dec!(9), dec!(900), "t", None)
            .await
            .unwrap();
        store
            .crypto_close(id, dec!(1500), dec!(15), dec!(9), "t", None)
            .await
            .unwrap();
        assert_eq!(
            budget(&store, BOOK, &cfg).await.unwrap(),
            dec!(9),
            "the $6 profit is set aside"
        );
    }
}
