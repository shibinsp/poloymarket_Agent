//! "Stocks in play" opening-range breakout for U.S. equities, paper only.
//!
//! Rules, backtested on two years of 5-minute SIP bars for 91 liquid names
//! (Nov 2024 - Oct 2026, profitable in both halves after 5 bp slippage):
//! 1. Opening range = the 9:30 five-minute bar.
//! 2. Only "stocks in play": that bar's volume is at least `rvol_min` (2.5)
//!    times its average over the previous 14 sessions, and the open is >= $5.
//! 3. Enter on the first completed bar that CLOSES above the range high
//!    (long) or below the range low (short), no later than the 15:30 bar,
//!    filled during the very next bar; a signal noticed later is skipped.
//! 4. Stop at the opposite side of the range. No target: flat by the close
//!    (the exit pass closes intraday equity positions 15 minutes before it).
//! 5. Equal-dollar sizing (`position_pct` of capital), at most
//!    `max_trades_per_day` entries, no new entries after a day's loss of
//!    `daily_loss_pct`. One attempt per symbol per day.
//!
//! The skill's own variant (15-minute range, long only, midpoint stop, 2R)
//! lost money out of sample, as did risk-based (1% at the stop) sizing.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};
use chrono_tz::America::New_York;
use rust_decimal::Decimal;
use serde::Deserialize;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::UsOrbConfig;
use crate::db::store::{OrderRecord, Store, VenueTradeRecord};
use crate::us::desk::{self, Atlas, DecisionCard, Headline, Titan, Vega};
use crate::venue::types::{
    AssetClass, Instrument, OrderKind, OrderRequest, OrderState, Quote, ScanFilter, Side,
    TimeInForce,
};
use crate::venue::VenueRegistry;

pub const ORB_STRATEGY: &str = "orb";
const VENUE: &str = "alpaca";
const BAR_MINUTES: i64 = 5;
/// Bar 70 starts 15:20 and closes 15:30: the last bar that may trigger.
const LAST_SIGNAL_BAR: usize = 70;

/// One 5-minute bar, stamped at its start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    pub ts: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
}

/// The New York session date of a bar and its index from 9:30 (0 = 9:30-9:35),
/// or None outside 9:30-16:00.
pub fn session_slot(ts: DateTime<Utc>) -> Option<(NaiveDate, usize)> {
    let ny = ts.with_timezone(&New_York);
    let minutes = ny.hour() as i64 * 60 + ny.minute() as i64 - (9 * 60 + 30);
    (0..390)
        .contains(&minutes)
        .then(|| (ny.date_naive(), (minutes / BAR_MINUTES) as usize))
}

/// The bar's volume over the mean of the prior sessions' first-bar volumes;
/// None until there are at least 10 of them.
pub fn relative_volume(first_bar_volume: Decimal, prior: &[Decimal]) -> Option<Decimal> {
    if prior.len() < 10 {
        return None;
    }
    let mean = prior.iter().copied().sum::<Decimal>() / Decimal::from(prior.len());
    (mean > Decimal::ZERO).then(|| first_bar_volume / mean)
}

/// The first completed bar after the range whose close is beyond it, as
/// (bar index, side). `bars` are today's bars keyed by session index; a bar
/// counts only if `completed(index)` says it has closed.
pub fn first_breakout(
    high: Decimal,
    low: Decimal,
    bars: &[(usize, Bar)],
    completed: impl Fn(usize) -> bool,
) -> Option<(usize, Side)> {
    for (index, bar) in bars {
        if *index == 0 || *index > LAST_SIGNAL_BAR || !completed(*index) {
            continue;
        }
        if bar.close > high {
            return Some((*index, Side::Buy));
        }
        if bar.close < low {
            return Some((*index, Side::Sell));
        }
    }
    None
}

/// Shares for an equal-dollar position. Shorts are whole shares (Alpaca
/// does not short fractions); longs keep fractions when the asset allows.
pub fn entry_quantity(
    instrument: &Instrument,
    notional: Decimal,
    price: Decimal,
    side: Side,
) -> Decimal {
    if price <= Decimal::ZERO || notional <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let raw = notional / price;
    if side == Side::Sell || !instrument.fractional {
        raw.floor()
    } else {
        instrument.round_qty(raw.round_dp_with_strategy(4, rust_decimal::RoundingStrategy::ToZero))
    }
}

// ---------------------------------------------------------------- market data

#[derive(Deserialize)]
struct BarsPage {
    #[serde(default)]
    bars: HashMap<String, Vec<RawBar>>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct RawBar {
    t: DateTime<Utc>,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    v: f64,
}

/// Multi-symbol 5-minute bars from Alpaca market data.
pub struct AlpacaBars {
    http: reqwest::Client,
    key: String,
    secret: String,
    /// "iex" on the free plan, which serves recent SIP bars only 15 minutes late.
    feed: String,
    base: String,
}

impl AlpacaBars {
    pub fn new(key: String, secret: String, feed: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            key,
            secret,
            feed,
            base: "https://data.alpaca.markets".to_string(),
        }
    }

    /// Regular-session 5-minute bars since `start` for each symbol, oldest first.
    pub async fn bars(
        &self,
        symbols: &[String],
        start: DateTime<Utc>,
    ) -> Result<HashMap<String, Vec<Bar>>> {
        let mut out: HashMap<String, Vec<Bar>> = HashMap::new();
        for chunk in symbols.chunks(50) {
            let mut token: Option<String> = None;
            loop {
                let mut query = vec![
                    ("symbols", chunk.join(",")),
                    ("timeframe", "5Min".to_string()),
                    (
                        "start",
                        start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    ),
                    ("limit", "10000".to_string()),
                    ("adjustment", "split".to_string()),
                    ("feed", self.feed.clone()),
                    ("sort", "asc".to_string()),
                ];
                if let Some(t) = &token {
                    query.push(("page_token", t.clone()));
                }
                let resp = self
                    .http
                    .get(format!("{}/v2/stocks/bars", self.base))
                    .header("APCA-API-KEY-ID", &self.key)
                    .header("APCA-API-SECRET-KEY", &self.secret)
                    .query(&query)
                    .send()
                    .await
                    .context("Alpaca bars request failed")?;
                if !resp.status().is_success() {
                    bail!("Alpaca bars returned {}", resp.status());
                }
                let page: BarsPage = resp.json().await.context("Unreadable Alpaca bars page")?;
                for (symbol, raw) in page.bars {
                    let list = out.entry(symbol).or_default();
                    for b in raw {
                        if session_slot(b.t).is_none() {
                            continue;
                        }
                        let d =
                            |x: f64| Decimal::from_f64_retain(x).unwrap_or_default().round_dp(4);
                        list.push(Bar {
                            ts: b.t,
                            open: d(b.o),
                            high: d(b.h),
                            low: d(b.l),
                            close: d(b.c),
                            volume: d(b.v),
                        });
                    }
                }
                token = page.next_page_token.filter(|t| !t.is_empty());
                if token.is_none() {
                    break;
                }
            }
        }
        for list in out.values_mut() {
            list.sort_by_key(|b| b.ts);
            list.dedup_by_key(|b| b.ts);
        }
        Ok(out)
    }
}

#[derive(Deserialize)]
struct NewsPage {
    #[serde(default)]
    news: Vec<RawNews>,
}

#[derive(Deserialize)]
struct RawNews {
    created_at: DateTime<Utc>,
    headline: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    source: String,
}

impl AlpacaBars {
    /// ORION's feed: one symbol's headlines since `start`, newest first (Alpaca/Benzinga).
    pub async fn news(&self, symbol: &str, start: DateTime<Utc>) -> Result<Vec<Headline>> {
        let resp = self
            .http
            .get(format!("{}/v1beta1/news", self.base))
            .header("APCA-API-KEY-ID", &self.key)
            .header("APCA-API-SECRET-KEY", &self.secret)
            .query(&[
                ("symbols", symbol.to_string()),
                (
                    "start",
                    start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                ),
                ("limit", "50".to_string()),
                ("sort", "desc".to_string()),
            ])
            .send()
            .await
            .context("Alpaca news request failed")?;
        if !resp.status().is_success() {
            bail!("Alpaca news returned {}", resp.status());
        }
        let page: NewsPage = resp.json().await.context("Unreadable Alpaca news page")?;
        Ok(page
            .news
            .into_iter()
            .map(|n| Headline {
                at: n.created_at,
                headline: n.headline,
                summary: n.summary,
                source: n.source,
            })
            .collect())
    }
}

// ---------------------------------------------------------------- the trader

/// Today's opening ranges for the stocks in play.
#[derive(Default)]
struct Day {
    date: Option<NaiveDate>,
    /// symbol -> (range high, range low, relative volume)
    candidates: HashMap<String, (Decimal, Decimal, Decimal)>,
    /// Entered, skipped as stale, or failed: no second attempt today.
    done: HashSet<String>,
    ranked: bool,
    /// Share of the universe whose first bar closed above its open, percent.
    breadth_up_pct: Decimal,
}

pub struct OrbTrader {
    data: AlpacaBars,
    day: Day,
}

impl OrbTrader {
    pub fn new(data: AlpacaBars) -> Self {
        Self {
            data,
            day: Day::default(),
        }
    }

    /// One pass: rank today's stocks in play once the first bar has closed,
    /// then enter any fresh breakout within the limits.
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &mut self,
        registry: &VenueRegistry,
        store: &Store,
        cfg: &UsOrbConfig,
        capital: Decimal,
        us_max_trades_per_day: Option<u32>,
        now: DateTime<Utc>,
        cycle: i64,
    ) -> Result<usize> {
        let Some((today, slot)) = session_slot(now) else {
            return Ok(0);
        };
        if self.day.date != Some(today) {
            self.day = Day {
                date: Some(today),
                ..Day::default()
            };
        }
        // The 9:30 bar closes at 9:35.
        if slot < 1 {
            return Ok(0);
        }
        if !self.day.ranked {
            self.rank(cfg, today, now).await?;
        }
        if self.day.candidates.is_empty() {
            return Ok(0);
        }

        // Limits shared with the rest of the U.S. book, read fresh each pass.
        let entered_today = store
            .count_venue_equity_entries_opened_on(VENUE, now.date_naive())
            .await?;
        let cap = us_max_trades_per_day
            .unwrap_or(cfg.max_trades_per_day)
            .min(cfg.max_trades_per_day);
        if entered_today >= cap {
            return Ok(0);
        }
        let day_pnl = store
            .strategy_day_pnl(ORB_STRATEGY, now.date_naive())
            .await?;
        if day_pnl <= -(capital * cfg.daily_loss_pct) {
            info!(%day_pnl, "ORB daily loss limit reached; no new entries today");
            return Ok(0);
        }
        for s in store
            .strategy_symbols_entered_on(ORB_STRATEGY, now.date_naive())
            .await?
        {
            self.day.done.insert(s);
        }
        let busy: HashSet<String> = store
            .get_unresolved_orders()
            .await?
            .into_iter()
            .map(|o| o.symbol)
            .chain(
                store
                    .get_open_venue_trades()
                    .await?
                    .into_iter()
                    .map(|t| t.symbol),
            )
            .collect();

        let watch: Vec<String> = self
            .day
            .candidates
            .keys()
            .filter(|s| !self.day.done.contains(*s) && !busy.contains(*s))
            .cloned()
            .collect();
        if watch.is_empty() {
            return Ok(0);
        }
        // Eight hours back always covers today's session so far. SPY rides
        // along for LUNA's read of the market.
        let mut fetch = watch.clone();
        if !fetch.iter().any(|s| s == "SPY") {
            fetch.push("SPY".to_string());
        }
        let bars = self.data.bars(&fetch, now - Duration::hours(8)).await?;
        let market_move_pct = bars
            .get("SPY")
            .and_then(|list| {
                let today_bars: Vec<&Bar> = list
                    .iter()
                    .filter(
                        |b| matches!(session_slot(b.ts), Some((d, i)) if d == today && i < slot),
                    )
                    .collect();
                let open = today_bars.first()?.open;
                let last = today_bars.last()?.close;
                (open > Decimal::ZERO).then(|| (last / open - Decimal::ONE) * Decimal::from(100))
            })
            .unwrap_or(Decimal::ZERO);

        // Signals in the order they fired, strongest relative volume first on a tie.
        let mut signals = Vec::new();
        for symbol in &watch {
            let (high, low, rvol) = self.day.candidates[symbol];
            let today_bars: Vec<(usize, Bar)> = bars
                .get(symbol)
                .map(|list| {
                    list.iter()
                        .filter_map(|b| {
                            session_slot(b.ts)
                                .filter(|(d, _)| *d == today)
                                .map(|(_, i)| (i, *b))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let completed = |i: usize| i < slot;
            if let Some((index, side)) = first_breakout(high, low, &today_bars, completed) {
                // Filled during the next bar, as tested; later is a different trade.
                if slot > index + 1 {
                    info!(
                        symbol,
                        index, "ORB signal is stale (missed its entry bar); skipping today"
                    );
                    self.day.done.insert(symbol.clone());
                    continue;
                }
                let stop = if side == Side::Buy { low } else { high };
                let trigger_close = today_bars
                    .iter()
                    .find(|(i, _)| *i == index)
                    .map(|(_, b)| b.close)
                    .unwrap_or(stop);
                let range = high - low;
                let past = if side == Side::Buy {
                    trigger_close - high
                } else {
                    low - trigger_close
                };
                let close_minutes = 9 * 60 + 30 + (index as i64 + 1) * BAR_MINUTES;
                let vega = Vega {
                    side: if side == Side::Buy { "LONG" } else { "SHORT" }.to_string(),
                    setup: "5-minute opening-range breakout".to_string(),
                    signal_at: format!("{:02}:{:02} ET", close_minutes / 60, close_minutes % 60),
                    trigger_close,
                    stop,
                    breakout_pct_of_range: if range > Decimal::ZERO {
                        (past / range * Decimal::from(100)).round_dp(1)
                    } else {
                        Decimal::ZERO
                    },
                };
                signals.push((
                    index,
                    std::cmp::Reverse(rvol),
                    symbol.clone(),
                    side,
                    stop,
                    vega,
                ));
            }
        }
        signals.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));

        let enforce = cfg.desk_mode.eq_ignore_ascii_case("enforce");
        let in_play_today = self.day.candidates.len();
        let breadth = self.day.breadth_up_pct;
        let mut placed = 0usize;
        let mut remaining = cap.saturating_sub(entered_today) as usize;
        for (_, std::cmp::Reverse(rvol), symbol, side, stop, vega) in signals {
            self.day.done.insert(symbol.clone());
            let (high, low, _) = self.day.candidates[&symbol];
            let atlas = Atlas {
                relative_volume: rvol.round_dp(2),
                range_high: high,
                range_low: low,
                in_play_today,
            };

            // TITAN: every limit, then the order it would allow.
            let prepared = if remaining == 0 {
                Err("daily trade limit reached".to_string())
            } else if side == Side::Sell && !cfg.allow_short {
                Err("short selling is off".to_string())
            } else {
                match titan_prepare(registry, cfg, capital, &symbol, side, stop, now).await {
                    Ok(p) => p,
                    Err(e) => Err(format!("could not price the order: {e}")),
                }
            };
            let titan = match &prepared {
                Ok(p) => Titan {
                    allowed: true,
                    reason: format!(
                        "within limits: {} of {} trades left, day P&L {}",
                        remaining,
                        cap,
                        day_pnl.round_dp(2)
                    ),
                    qty: p.qty,
                    notional_usd: (p.qty * p.limit).round_dp(2),
                    risk_usd: (p.qty * (p.limit - stop).abs()).round_dp(2),
                },
                Err(reason) => Titan {
                    allowed: false,
                    reason: reason.clone(),
                    qty: Decimal::ZERO,
                    notional_usd: Decimal::ZERO,
                    risk_usd: Decimal::ZERO,
                },
            };

            // ORION and LUNA: the stock's news and the market's mood.
            let headlines = match self.data.news(&symbol, now - Duration::hours(24)).await {
                Ok(h) => h,
                Err(e) => {
                    warn!(symbol, error = %e, "ORION could not read the news; treating as no news");
                    Vec::new()
                }
            };
            let orion = desk::orion(&headlines, now);
            let recent: Vec<Headline> = headlines
                .into_iter()
                .filter(|h| now.signed_duration_since(h.at) <= Duration::hours(24))
                .collect();
            let luna = desk::luna(&recent, market_move_pct, breadth, side == Side::Buy);

            // NOVA: the call.
            let nova = desk::nova(&titan, &orion, &luna, enforce);
            let card = DecisionCard {
                at: now,
                market: "US".to_string(),
                strategy: ORB_STRATEGY.to_string(),
                symbol: symbol.clone(),
                atlas,
                vega,
                orion,
                luna,
                titan,
                nova: nova.clone(),
            };
            info!(
                symbol,
                action = ?nova.action,
                advisory = ?nova.advisory_action,
                reasons = ?nova.reasons,
                "NOVA decision"
            );
            if let Err(e) = store.insert_desk_decision(&card).await {
                warn!(symbol, error = %e, "Could not record the decision card");
            }

            let Ok(mut order) = prepared else { continue };
            match nova.action {
                desk::Action::Skip => continue,
                desk::Action::TradeHalf => {
                    order.qty = if side == Side::Sell || !order.instrument.fractional {
                        (order.qty / Decimal::from(2)).floor()
                    } else {
                        order
                            .instrument
                            .round_qty((order.qty / Decimal::from(2)).round_dp(4))
                    };
                    if order.qty <= Decimal::ZERO {
                        continue;
                    }
                }
                desk::Action::Trade => {}
            }
            match submit(
                registry, store, order, &symbol, side, stop, rvol, capital, now, cycle,
            )
            .await
            {
                Ok(true) => {
                    placed += 1;
                    remaining -= 1;
                }
                Ok(false) => {}
                Err(e) => warn!(symbol, error = %e, "ORB entry failed"),
            }
        }
        Ok(placed)
    }

    /// Opening ranges and relative volume for the whole universe, once a day.
    async fn rank(
        &mut self,
        cfg: &UsOrbConfig,
        today: NaiveDate,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let history = self
            .data
            .bars(&cfg.symbols, now - Duration::days(30))
            .await?;
        let mut ranked = Vec::new();
        let (mut up, mut seen) = (0u32, 0u32);
        for symbol in &cfg.symbols {
            let Some(list) = history.get(symbol) else {
                continue;
            };
            let mut prior = Vec::new();
            let mut first_today = None;
            for b in list {
                match session_slot(b.ts) {
                    Some((d, 0)) if d == today => first_today = Some(*b),
                    Some((d, 0)) if d < today => prior.push(b.volume),
                    _ => {}
                }
            }
            let Some(first) = first_today else { continue };
            seen += 1;
            if first.close > first.open {
                up += 1;
            }
            let prior = &prior[prior.len().saturating_sub(14)..];
            let Some(rvol) = relative_volume(first.volume, prior) else {
                continue;
            };
            if first.open >= cfg.min_price && first.high > first.low && rvol >= cfg.rvol_min {
                ranked.push((symbol.clone(), rvol));
                self.day
                    .candidates
                    .insert(symbol.clone(), (first.high, first.low, rvol));
            }
        }
        self.day.ranked = true;
        if seen > 0 {
            self.day.breadth_up_pct = Decimal::from(up * 100) / Decimal::from(seen);
        }
        info!(
            stocks_in_play = ?ranked,
            universe = cfg.symbols.len(),
            breadth_up_pct = %self.day.breadth_up_pct.round_dp(1),
            "ATLAS: opening ranges ranked"
        );
        Ok(())
    }
}

/// An order TITAN has allowed: the instrument, the quote it was priced off,
/// the marketable limit and the size.
struct Prepared {
    instrument: Instrument,
    quote: Quote,
    limit: Decimal,
    qty: Decimal,
}

/// TITAN's order checks for one signal: the order it allows, or why not.
async fn titan_prepare(
    registry: &VenueRegistry,
    cfg: &UsOrbConfig,
    capital: Decimal,
    symbol: &str,
    side: Side,
    stop: Decimal,
    now: DateTime<Utc>,
) -> Result<std::result::Result<Prepared, String>> {
    let filter = ScanFilter {
        asset_classes: vec![AssetClass::Equity],
        symbols: vec![symbol.to_string()],
        min_volume_24h: None,
        max_days_to_resolution: None,
        max_results: Some(1),
    };
    let Some(instrument) = registry
        .list_tradeable_instruments(now, &filter)
        .await
        .into_iter()
        .find(|i| i.venue().as_str() == VENUE && i.symbol().eq_ignore_ascii_case(symbol))
    else {
        return Ok(Err(
            "not a tradeable Alpaca instrument (add it to the alpaca venue's symbols)".to_string(),
        ));
    };
    let venue = registry
        .get(instrument.venue())
        .context("Alpaca venue missing")?;
    let quote = venue.quote(&instrument.id).await?;
    let touch = quote.taker_price(side);
    // Never enter on the wrong side of the stop: the breakout has already failed.
    if touch <= Decimal::ZERO
        || (side == Side::Buy && touch <= stop)
        || (side == Side::Sell && touch >= stop)
    {
        return Ok(Err(format!(
            "price {touch} is back through the stop {stop}: the breakout already failed"
        )));
    }
    // A marketable limit: crosses the spread, but caps how far a fast market can fill us.
    let limit = instrument.round_price(
        if side == Side::Buy {
            touch * (Decimal::ONE + cfg.max_entry_slippage_pct)
        } else {
            touch * (Decimal::ONE - cfg.max_entry_slippage_pct)
        },
        side,
    );
    let notional = capital * cfg.position_pct;
    let qty = entry_quantity(&instrument, notional, limit, side);
    if qty <= Decimal::ZERO {
        return Ok(Err(format!(
            "${} buys no whole share at {limit}{}",
            notional.round_dp(2),
            if side == Side::Sell {
                " (shorts must be whole shares)"
            } else {
                ""
            }
        )));
    }
    Ok(Ok(Prepared {
        instrument,
        quote,
        limit,
        qty,
    }))
}

/// Place one entry and record it like every other venue entry: the order row
/// before the submit, then the outcome, then the trade (PENDING until filled).
#[allow(clippy::too_many_arguments)]
async fn submit(
    registry: &VenueRegistry,
    store: &Store,
    order: Prepared,
    symbol: &str,
    side: Side,
    stop: Decimal,
    rvol: Decimal,
    capital: Decimal,
    now: DateTime<Utc>,
    cycle: i64,
) -> Result<bool> {
    let Prepared {
        instrument,
        quote,
        limit,
        qty,
    } = order;
    let venue = registry
        .get(instrument.venue())
        .context("Alpaca venue missing")?;
    let client_order_id = Uuid::new_v4().to_string();
    store
        .insert_order(&OrderRecord {
            id: None,
            client_order_id: client_order_id.clone(),
            venue_order_id: None,
            venue_id: VENUE.to_string(),
            symbol: symbol.to_string(),
            side: side.to_string(),
            intent: "ENTRY".to_string(),
            trade_id: None,
            limit_price: Some(limit.to_string()),
            qty: qty.to_string(),
            filled_qty: "0".to_string(),
            avg_fill_price: None,
            state: "PENDING".to_string(),
            reject_reason: None,
            cycle: Some(cycle),
            mid_at_submit: Some(quote.mid.to_string()),
            submitted_at: None,
            updated_at: None,
            expires_at: Some((now + Duration::seconds(120)).to_rfc3339()),
        })
        .await?;

    let request = OrderRequest {
        instrument: instrument.clone(),
        side,
        kind: OrderKind::Limit { price: limit },
        qty,
        tif: TimeInForce::Day,
        extended_hours: false,
        client_order_id: client_order_id.clone(),
    };
    let ack = match venue.place_order(&request).await {
        Ok(ack) => ack,
        Err(e) => {
            let msg = e.to_string();
            let state = if msg.contains("HTTP 4") || msg.contains("rejected") {
                "REJECTED"
            } else {
                "UNKNOWN"
            };
            store
                .update_order_state(&client_order_id, state, None, "0", None, Some(&msg))
                .await?;
            return Err(e);
        }
    };
    let state = match &ack.state {
        OrderState::Accepted => "ACCEPTED",
        OrderState::PartiallyFilled => "PARTIALLY_FILLED",
        OrderState::Filled => "FILLED",
        OrderState::Cancelled => "CANCELLED",
        OrderState::Expired => "EXPIRED",
        OrderState::Rejected(_) => "REJECTED",
        OrderState::Unknown => "UNKNOWN",
    };
    store
        .update_order_state(
            &client_order_id,
            state,
            Some(&ack.venue_order_id),
            &ack.filled_qty.to_string(),
            ack.avg_fill_price.map(|p| p.to_string()).as_deref(),
            match &ack.state {
                OrderState::Rejected(r) => Some(r.as_str()),
                _ => None,
            },
        )
        .await?;
    if let (Some(order_id), Some(avg)) = (
        store.order_id(&client_order_id).await.ok().flatten(),
        ack.avg_fill_price,
    ) {
        if let Err(e) = store
            .record_fill_increment(
                order_id,
                Some(&ack.venue_order_id),
                ack.filled_qty,
                avg,
                Decimal::ZERO,
                None,
                Some(ack.fees),
                Some(quote.mid),
                &side.to_string(),
                None,
                now,
            )
            .await
        {
            warn!(symbol, error = %e, "Could not record an execution filled at submission");
        }
    }

    let filled = ack.filled_qty;
    let fill_price = ack.avg_fill_price.unwrap_or(limit);
    let status = if filled > Decimal::ZERO {
        if filled >= qty {
            "OPEN"
        } else {
            "PARTIAL"
        }
    } else if crate::agent::reconcile::is_terminal(&ack.state) {
        "CANCELLED"
    } else {
        "PENDING"
    };
    let units = if filled > Decimal::ZERO { filled } else { qty };
    let stop_pct = ((fill_price - stop).abs() / fill_price).round_dp(6);
    let trade_id = store
        .insert_venue_trade(&VenueTradeRecord {
            cycle,
            venue_id: VENUE.to_string(),
            symbol: symbol.to_string(),
            asset_class: "equity".to_string(),
            display_name: Some(instrument.display_name.clone()),
            side: side.to_string(),
            entry_price: fill_price.to_string(),
            quantity: units.to_string(),
            notional: (units * fill_price).round_dp(2).to_string(),
            avg_fill_price: (filled > Decimal::ZERO).then(|| fill_price.to_string()),
            edge_at_entry: "0".to_string(),
            fair_value: "0".to_string(),
            confidence: "0".to_string(),
            risk_pct: (units * (fill_price - stop).abs() / capital)
                .round_dp(6)
                .to_string(),
            stop_pct: stop_pct.to_string(),
            status: status.to_string(),
            stop_price: Some(stop.to_string()),
            target_price: None,
            horizon_hours: Some(7),
            client_order_id: Some(client_order_id.clone()),
            venue_order_id: Some(ack.venue_order_id.clone()),
            strategy: Some(ORB_STRATEGY.to_string()),
        })
        .await?;
    store
        .link_order_to_trade(&client_order_id, trade_id)
        .await?;
    info!(
        symbol,
        side = %side,
        qty = %qty,
        limit = %limit,
        stop = %stop,
        rvol = %rvol.round_dp(2),
        state,
        trade_id,
        "ORB entry submitted to the Alpaca paper account"
    );
    Ok(!matches!(ack.state, OrderState::Rejected(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::types::{InstrumentId, InstrumentMeta, VenueId};
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        New_York
            .with_ymd_and_hms(2026, 10, 19, h, m, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn bar(h: u32, m: u32, close: Decimal) -> Bar {
        Bar {
            ts: at(h, m),
            open: close,
            high: close + dec!(0.1),
            low: close - dec!(0.1),
            close,
            volume: dec!(1000),
        }
    }

    fn stock(fractional: bool) -> Instrument {
        Instrument {
            id: InstrumentId::new(VenueId::new("alpaca"), "TSLA"),
            asset_class: AssetClass::Equity,
            display_name: "Tesla".to_string(),
            quote_ccy: "USD".to_string(),
            tick_size: Some(dec!(0.01)),
            lot_size: None,
            min_notional: Some(dec!(1)),
            min_qty: None,
            fractional,
            meta: InstrumentMeta::Equity {
                exchange: "NASDAQ".to_string(),
            },
        }
    }

    #[test]
    fn session_slots_follow_new_york_time() {
        assert_eq!(session_slot(at(9, 30)).map(|s| s.1), Some(0));
        assert_eq!(session_slot(at(9, 35)).map(|s| s.1), Some(1));
        assert_eq!(session_slot(at(15, 25)).map(|s| s.1), Some(71));
        assert_eq!(session_slot(at(9, 29)), None);
        assert_eq!(session_slot(at(16, 0)), None);
    }

    #[test]
    fn relative_volume_needs_ten_prior_sessions() {
        let prior = vec![dec!(100); 14];
        assert_eq!(relative_volume(dec!(250), &prior), Some(dec!(2.5)));
        assert_eq!(relative_volume(dec!(250), &prior[..9]), None);
        assert_eq!(relative_volume(dec!(250), &[dec!(0); 12]), None);
    }

    #[test]
    fn first_completed_close_beyond_the_range_wins() {
        let bars = vec![
            (0, bar(9, 30, dec!(100))),
            (1, bar(9, 35, dec!(100.5))), // closes above the 100.1 high
            (2, bar(9, 40, dec!(99.0))),  // a later close below does not count
        ];
        assert_eq!(
            first_breakout(dec!(100.1), dec!(99.9), &bars, |_| true),
            Some((1, Side::Buy))
        );
        // The 9:35 bar has not closed yet: no signal from it.
        assert_eq!(
            first_breakout(dec!(100.1), dec!(99.9), &bars[..2], |i| i < 1),
            None
        );
        let down = vec![(0, bar(9, 30, dec!(100))), (1, bar(9, 35, dec!(99.5)))];
        assert_eq!(
            first_breakout(dec!(100.1), dec!(99.9), &down, |_| true),
            Some((1, Side::Sell))
        );
    }

    #[test]
    fn no_signal_after_the_last_entry_bar() {
        let late = vec![(71, bar(15, 25, dec!(105)))];
        assert_eq!(
            first_breakout(dec!(100.1), dec!(99.9), &late, |_| true),
            None
        );
        let last = vec![(70, bar(15, 20, dec!(105)))];
        assert_eq!(
            first_breakout(dec!(100.1), dec!(99.9), &last, |_| true),
            Some((70, Side::Buy))
        );
    }

    #[test]
    fn shorts_are_whole_shares_longs_may_be_fractional() {
        assert_eq!(
            entry_quantity(&stock(true), dec!(200), dec!(364.18), Side::Buy),
            dec!(0.5491)
        );
        assert_eq!(
            entry_quantity(&stock(true), dec!(200), dec!(364.18), Side::Sell),
            dec!(0)
        );
        assert_eq!(
            entry_quantity(&stock(true), dec!(200), dec!(45), Side::Sell),
            dec!(4)
        );
        assert_eq!(
            entry_quantity(&stock(false), dec!(200), dec!(45), Side::Buy),
            dec!(4)
        );
        assert_eq!(
            entry_quantity(&stock(true), dec!(0), dec!(45), Side::Buy),
            dec!(0)
        );
    }
}
