//! The `Venue` implementation for Coinbase Advanced Trade.
//!
//! Spot crypto only, so the session is `Always` and there is no clock call, no
//! extended-hours flag and no equity-style lot arithmetic.
//!
//! Two shapes differ from Alpaca in ways that matter:
//!
//! * **`POST /orders` returns HTTP 200 for a rejected order.** Success and
//!   failure are distinguished by a `success` boolean and which of two nested
//!   objects is populated — so an adapter that trusts the status code records
//!   a rejection as an accepted order and waits forever for a fill.
//! * **Sizes are quoted in the base currency**, and `base_size` is a string.
//!   Sending a number loses scale on small crypto quantities, which is the
//!   whole point of `Decimal` here.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use tracing::{info, instrument, warn};

use super::auth::CdpSigner;
use super::models::{
    money, Account, AccountsResponse, BatchCancelResponse, CandlesResponse, CreateOrderResponse,
    Order, OrderResponse, OrdersResponse, Product, ProductBookResponse, ProductsResponse,
};
use super::rest::CoinbaseRest;
use crate::market::models::{OrderBookSnapshot, PriceLevel};
use crate::venue::session::TradingSession;
use crate::venue::types::{
    AssetClass, Balance, Candle, CandleInterval, Instrument, InstrumentId, InstrumentMeta,
    OrderAck, OrderKind, OrderRef, OrderRequest, OrderState, Position, Quote, ScanFilter,
    Settlement, Side, TimeInForce, VenueCapabilities, VenueId,
};
use crate::venue::Venue;

pub const DEFAULT_VENUE_ID: &str = "coinbase";
pub const BASE_URL: &str = "https://api.coinbase.com";
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Coinbase's page cap for `/products`.
const PRODUCT_PAGE_LIMIT: usize = 1000;
/// Pages of `/products` to walk before giving up and saying so. Coinbase pages
/// this endpoint by offset, and a configured symbol past the first page was
/// dropped with only a "does not list this product" warning — which reads as a
/// delisting rather than as truncation.
const MAX_PRODUCT_PAGES: usize = 20;
/// Coinbase's page cap for the order endpoints.
const ORDER_PAGE_LIMIT: usize = 250;
/// Pages of orders to walk. The live set is small; this exists so a stuck
/// cursor cannot spin forever.
const MAX_ORDER_PAGES: usize = 20;
/// Coinbase's page cap for `/accounts`. It creates one row per supported
/// currency, so real accounts run past a single page.
const ACCOUNT_PAGE_LIMIT: usize = 250;
const MAX_ACCOUNT_PAGES: usize = 20;
/// Most candles Coinbase returns for one request.
const MAX_CANDLES_PER_REQUEST: i64 = 350;
/// How long a fetched account snapshot is reused.
///
/// `balance()` and `positions()` are separate trait methods that the reconciler
/// calls back to back, and each one otherwise pages the same rate-limited
/// endpoint, minting a JWT per request. A window this short cannot outlive one
/// pass, and it makes the cash and the holdings two projections of a *single*
/// snapshot rather than two that can disagree with each other.
const ACCOUNTS_CACHE_TTL: Duration = Duration::from_secs(2);

const ORDERS_PATH: &str = "/api/v3/brokerage/orders";
const ORDER_HISTORY_PATH: &str = "/api/v3/brokerage/orders/historical/batch";
const BATCH_CANCEL_PATH: &str = "/api/v3/brokerage/orders/batch_cancel";
const ACCOUNTS_PATH: &str = "/api/v3/brokerage/accounts";

/// Coinbase refuses `OPEN` combined with any other status, so the live set
/// takes two queries rather than one.
const OPEN_STATUS: [&str; 1] = ["OPEN"];
/// `to_order_state` treats these as accepted — the order exists at the venue
/// and can fill — so the kill switch has to be able to see them.
const PENDING_STATUSES: [&str; 2] = ["PENDING", "QUEUED"];

pub struct CoinbaseConfig {
    pub venue_id: VenueId,
    pub base_url: String,
    key_name: String,
    private_key_pem: String,
    pub symbols: Vec<String>,
    pub request_timeout: Duration,
    pub accounts_cache_ttl: Duration,
}

/// Hand-written so the key can never reach a log line or panic message.
impl std::fmt::Debug for CoinbaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoinbaseConfig")
            .field("venue_id", &self.venue_id)
            .field("base_url", &self.base_url)
            .field("key_name", &self.key_name)
            .field("private_key_pem", &"<redacted>")
            .field("symbols", &self.symbols)
            .finish_non_exhaustive()
    }
}

impl CoinbaseConfig {
    pub fn new(key_name: impl Into<String>, private_key_pem: impl Into<String>) -> Self {
        Self {
            venue_id: VenueId::new(DEFAULT_VENUE_ID),
            base_url: BASE_URL.to_string(),
            key_name: key_name.into(),
            private_key_pem: private_key_pem.into(),
            symbols: Vec::new(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            accounts_cache_ttl: ACCOUNTS_CACHE_TTL,
        }
    }

    pub fn with_venue_id(mut self, id: impl Into<String>) -> Self {
        self.venue_id = VenueId::new(id);
        self
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn with_symbols(mut self, symbols: Vec<String>) -> Self {
        self.symbols = symbols;
        self
    }

    /// Reuse window for the `/accounts` snapshot. Zero disables reuse.
    pub fn with_accounts_cache_ttl(mut self, ttl: Duration) -> Self {
        self.accounts_cache_ttl = ttl;
        self
    }
}

pub struct CoinbaseVenue {
    id: VenueId,
    caps: VenueCapabilities,
    session: TradingSession,
    symbols: Vec<String>,
    rest: CoinbaseRest,
    accounts_cache_ttl: Duration,
    accounts_cache: tokio::sync::Mutex<Option<(Instant, Arc<Vec<Account>>)>>,
}

impl std::fmt::Debug for CoinbaseVenue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoinbaseVenue")
            .field("id", &self.id)
            .field("symbols", &self.symbols)
            .field("rest", &self.rest)
            .finish_non_exhaustive()
    }
}

impl CoinbaseVenue {
    pub fn new(config: CoinbaseConfig) -> Result<Self> {
        let signer = CdpSigner::new(&config.key_name, &config.private_key_pem)?;
        let rest = CoinbaseRest::new(&config.base_url, signer, config.request_timeout)?;

        info!(
            venue = %config.venue_id,
            base_url = %config.base_url,
            symbols = config.symbols.len(),
            "Coinbase venue configured"
        );

        Ok(Self {
            id: config.venue_id,
            caps: VenueCapabilities {
                asset_classes: vec![AssetClass::CryptoSpot],
                // Crypto has no sessions, so the flag never applies.
                limit_only_outside_regular: false,
                supports_client_order_id: true,
                supports_candles: true,
            },
            // Spot crypto: 24/7, every day, no holidays.
            session: TradingSession::Always,
            symbols: config.symbols,
            rest,
            accounts_cache_ttl: config.accounts_cache_ttl,
            accounts_cache: tokio::sync::Mutex::new(None),
        })
    }

    /// Coinbase spells pairs `BTC-USD`; the rest of this codebase uses
    /// `BTC/USD`, which is what Alpaca reports and what the config carries.
    ///
    /// Normalising in both directions at the edge means a symbol configured
    /// once works on either venue, and `venue_symbols_for` does not need to
    /// know which venue it is answering for.
    fn to_product_id(symbol: &str) -> String {
        symbol.trim().to_uppercase().replace('/', "-")
    }

    fn to_symbol(product_id: &str) -> String {
        product_id.trim().to_uppercase().replace('-', "/")
    }

    fn instrument_from(&self, product: &Product) -> Result<Instrument> {
        let symbol = Self::to_symbol(&product.product_id);
        let base = product
            .base_currency_id
            .clone()
            .unwrap_or_else(|| symbol.split('/').next().unwrap_or_default().to_string());

        Ok(Instrument {
            id: InstrumentId::new(self.id.clone(), symbol.clone()),
            asset_class: AssetClass::CryptoSpot,
            display_name: product.display_name.clone().unwrap_or(symbol),
            quote_ccy: product
                .quote_currency_id
                .clone()
                .unwrap_or_else(|| "USD".to_string()),
            tick_size: product
                .quote_increment
                .as_deref()
                .map(|v| money(v, "quote_increment"))
                .transpose()?,
            lot_size: product
                .base_increment
                .as_deref()
                .map(|v| money(v, "base_increment"))
                .transpose()?,
            min_notional: product
                .quote_min_size
                .as_deref()
                .map(|v| money(v, "quote_min_size"))
                .transpose()?,
            min_qty: product
                .base_min_size
                .as_deref()
                .map(|v| money(v, "base_min_size"))
                .transpose()?,
            // Crypto is divisible to `base_increment`.
            fractional: true,
            meta: InstrumentMeta::Spot { base },
        })
    }

    /// The configured symbol whose base is `base`, if the agent trades one.
    ///
    /// A spot balance names only the currency — `BTC` — and the pair it
    /// belongs to is a *choice*. Guessing `/USD` against a `BTC/USDC`
    /// universe reconciles against nothing, which reads as two mismatches at
    /// once and halts a healthy agent `UntilResume`.
    ///
    /// `None` means the agent does not trade this currency at all. The caller
    /// reports it rather than inventing a symbol for it.
    fn symbol_for_base(&self, base: &str) -> Option<String> {
        let mut matches = self
            .symbols
            .iter()
            .map(|s| s.trim().to_uppercase())
            .filter(|s| {
                s.split('/')
                    .next()
                    .is_some_and(|b| b.eq_ignore_ascii_case(base))
            });

        let first = matches.next()?;
        if matches.next().is_some() {
            // One balance cannot be split across two ledgers: Coinbase does
            // not say which pair bought the coin. Attributing it to the first
            // is a choice worth making loudly.
            warn!(
                venue = %self.id,
                base = base,
                symbol = %first,
                "Several configured symbols share this base — a spot balance names only \
                 the currency, so the whole holding is attributed to the first"
            );
        }
        Some(first)
    }

    async fn fetch_order(&self, venue_order_id: &str) -> Result<Order> {
        let path = format!(
            "/api/v3/brokerage/orders/historical/{}",
            urlencoding::encode(venue_order_id)
        );
        let response: OrderResponse = self
            .rest
            .get(&path, &[])
            .await
            .with_context(|| format!("Failed to fetch Coinbase order {venue_order_id}"))?;
        Ok(response.order)
    }
}

/// Whether a currency is the cash side rather than a position.
///
/// Both are quote currencies on Coinbase and both spend as dollars here;
/// treating USDC as a position would report the cash balance as exposure and
/// have reconciliation hunt for a matching trade that does not exist.
fn is_cash(currency: &str) -> bool {
    currency.eq_ignore_ascii_case("USD") || currency.eq_ignore_ascii_case("USDC")
}

/// Whether an account's balance is actually spendable.
///
/// Coinbase omits `active` on some account types, and absence is not a signal.
/// An explicit `false` is: a suspended or restricted sub-account's cash cannot
/// be spent, and counting it sizes positions against money the agent will
/// discover it does not have one rejected order at a time.
fn is_usable(account: &Account) -> bool {
    account.active != Some(false)
}

/// `to_ack`, but a malformed number costs that field rather than the whole
/// list.
///
/// `open_orders` feeds `cancel_all`, so one unparseable `total_fees` anywhere
/// in the book would make the kill switch return before posting a single
/// cancel and leave every resting order live. The order id and status are what
/// cancelling and the orphan audit need, and both survive. Nothing books a
/// fill from this path — `get_order` stays strict for that.
fn to_ack_lossy(order: &Order) -> OrderAck {
    to_ack(order).unwrap_or_else(|e| {
        warn!(
            order_id = %order.order_id,
            error = %format!("{e:#}"),
            "Coinbase sent an unparseable order field — keeping the order so it can \
             still be cancelled, with its numeric fields zeroed"
        );
        OrderAck {
            venue_order_id: order.order_id.clone(),
            client_order_id: order.client_order_id.clone().unwrap_or_default(),
            state: to_order_state(order),
            filled_qty: Decimal::ZERO,
            avg_fill_price: None,
            fees: Decimal::ZERO,
        }
    })
}

/// Fold a series into wider buckets, aligned to the epoch.
///
/// Coinbase has no four-hour granularity, so an H4 series is built from pairs
/// of two-hour bars. The alternative — labelling `SIX_HOUR` data as four-hour,
/// which is what this adapter did first — produces a plausible number from the
/// wrong bars rather than an error.
fn fold_candles(source: Vec<Candle>, bucket_seconds: i64) -> Vec<Candle> {
    let mut out: Vec<Candle> = Vec::new();
    for candle in source {
        let offset = candle.ts.timestamp().rem_euclid(bucket_seconds);
        let bucket_start = candle.ts - chrono::Duration::seconds(offset);
        match out.last_mut() {
            Some(last) if last.ts == bucket_start => {
                last.high = last.high.max(candle.high);
                last.low = last.low.min(candle.low);
                // Oldest first, so the last bar seen closes the bucket.
                last.close = candle.close;
                last.volume += candle.volume;
            }
            _ => out.push(Candle {
                ts: bucket_start,
                ..candle
            }),
        }
    }
    out
}

/// Coinbase's order status vocabulary, mapped to ours.
///
/// `PENDING` and `QUEUED` are *accepted* — the order exists at the venue and
/// may fill. Treating them as unknown would have reconciliation re-query
/// something that is behaving normally; treating them as rejected would be
/// worse, since the order is live.
pub fn to_order_state(order: &Order) -> OrderState {
    let filled = order
        .filled_size
        .as_deref()
        .and_then(|v| money(v, "filled_size").ok())
        .unwrap_or(Decimal::ZERO);

    match order
        .status
        .as_deref()
        .unwrap_or_default()
        .to_uppercase()
        .as_str()
    {
        "FILLED" => OrderState::Filled,
        "OPEN" | "PENDING" | "QUEUED" => {
            if filled > Decimal::ZERO {
                OrderState::PartiallyFilled
            } else {
                OrderState::Accepted
            }
        }
        "CANCELLED" | "CANCEL_QUEUED" => {
            // A cancel that caught a partial fill is not the same as one that
            // caught nothing: the filled part is a real position.
            if filled > Decimal::ZERO {
                OrderState::PartiallyFilled
            } else {
                OrderState::Cancelled
            }
        }
        "EXPIRED" => {
            if filled > Decimal::ZERO {
                OrderState::PartiallyFilled
            } else {
                OrderState::Expired
            }
        }
        "FAILED" | "REJECTED" => OrderState::Rejected(
            order
                .reject_message
                .clone()
                .or_else(|| order.reject_reason.clone())
                .unwrap_or_else(|| "Coinbase rejected the order".to_string()),
        ),
        // An unrecognised status is not evidence of anything. Guessing here
        // is how a live order gets abandoned or duplicated.
        other => {
            warn!(status = other, order_id = %order.order_id, "Unrecognised Coinbase order status");
            OrderState::Unknown
        }
    }
}

pub fn to_ack(order: &Order) -> Result<OrderAck> {
    Ok(OrderAck {
        venue_order_id: order.order_id.clone(),
        client_order_id: order.client_order_id.clone().unwrap_or_default(),
        state: to_order_state(order),
        filled_qty: order
            .filled_size
            .as_deref()
            .map(|v| money(v, "filled_size"))
            .transpose()?
            .unwrap_or(Decimal::ZERO),
        avg_fill_price: order
            .average_filled_price
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            // Coinbase sends "0" before anything fills; a zero average is not
            // a price, and recording it would book a fill at zero.
            .map(|v| money(v, "average_filled_price"))
            .transpose()?
            .filter(|p| *p > Decimal::ZERO),
        fees: order
            .total_fees
            .as_deref()
            .map(|v| money(v, "total_fees"))
            .transpose()?
            .unwrap_or(Decimal::ZERO),
    })
}

#[async_trait]
impl Venue for CoinbaseVenue {
    fn id(&self) -> &VenueId {
        &self.id
    }

    fn capabilities(&self) -> &VenueCapabilities {
        &self.caps
    }

    fn session(&self) -> &TradingSession {
        &self.session
    }

    #[instrument(skip(self, filter), fields(venue = %self.id))]
    async fn list_instruments(&self, filter: &ScanFilter) -> Result<Vec<Instrument>> {
        let requested: &[String] = if filter.symbols.is_empty() {
            &self.symbols
        } else {
            &filter.symbols
        };
        if requested.is_empty() {
            return Ok(Vec::new());
        }

        // Crypto only. A filter that excludes it excludes this venue entirely.
        if !filter.asset_classes.is_empty()
            && !filter.asset_classes.contains(&AssetClass::CryptoSpot)
        {
            return Ok(Vec::new());
        }

        let wanted: HashSet<String> = requested.iter().map(|s| Self::to_product_id(s)).collect();

        let mut out = Vec::new();
        // Every wanted id Coinbase named, tradeable or not. Built once, so the
        // "not listed" pass below is a lookup per configured symbol rather
        // than a scan of the whole product list per configured symbol.
        let mut seen: HashSet<String> = HashSet::new();
        let mut offset = 0usize;
        let mut truncated = true;

        for _ in 0..MAX_PRODUCT_PAGES {
            let mut params = vec![
                ("product_type", "SPOT".to_string()),
                ("limit", PRODUCT_PAGE_LIMIT.to_string()),
                ("offset", offset.to_string()),
            ];
            // Ask for exactly the products wanted. When Coinbase honours this
            // the answer is one short page; when it does not, the local
            // `wanted` check and the offset walk below reach the same result,
            // so neither behaviour can truncate the universe silently.
            for id in &wanted {
                params.push(("product_ids", id.clone()));
            }

            let response: ProductsResponse = self
                .rest
                .get("/api/v3/brokerage/products", &params)
                .await
                .context("Failed to list Coinbase products")?;

            let page_len = response.products.len();
            for product in &response.products {
                let id = product.product_id.to_uppercase();
                if !wanted.contains(&id) || !seen.insert(id) {
                    continue;
                }
                if !product.tradeable() {
                    warn!(
                        product = %product.product_id,
                        "Coinbase lists this product as not tradeable — skipping"
                    );
                    continue;
                }
                out.push(self.instrument_from(product)?);
            }

            if seen.len() == wanted.len() || page_len < PRODUCT_PAGE_LIMIT {
                truncated = false;
                break;
            }
            offset += page_len;
        }

        if truncated {
            // Not the same as a delisting, and saying so matters: the operator
            // would otherwise go looking for a symbol Coinbase still trades.
            warn!(
                venue = %self.id,
                pages = MAX_PRODUCT_PAGES,
                "Stopped paging Coinbase products before finding every configured symbol"
            );
        }

        // Configured symbols Coinbase did not return at all. Silently trading
        // a smaller universe than configured is a quiet way to under-trade.
        for symbol in requested {
            if !seen.contains(&Self::to_product_id(symbol)) {
                warn!(symbol = %symbol, "Coinbase does not list this product — skipping");
            }
        }

        if let Some(max) = filter.max_results {
            out.truncate(max);
        }
        Ok(out)
    }

    #[instrument(skip(self), fields(venue = %self.id, symbol = %id.symbol))]
    async fn quote(&self, id: &InstrumentId) -> Result<Quote> {
        let product_id = Self::to_product_id(&id.symbol);
        let response: ProductBookResponse = self
            .rest
            .get(
                "/api/v3/brokerage/product_book",
                &[
                    ("product_id", product_id.clone()),
                    ("limit", "10".to_string()),
                ],
            )
            .await
            .with_context(|| format!("Failed to fetch the Coinbase book for {product_id}"))?;

        let book = response.pricebook;
        let bids: Vec<PriceLevel> = book
            .bids
            .iter()
            .map(|l| {
                Ok(PriceLevel {
                    price: money(&l.price, "bid price")?,
                    size: money(&l.size, "bid size")?,
                })
            })
            .collect::<Result<_>>()?;
        let asks: Vec<PriceLevel> = book
            .asks
            .iter()
            .map(|l| {
                Ok(PriceLevel {
                    price: money(&l.price, "ask price")?,
                    size: money(&l.size, "ask size")?,
                })
            })
            .collect::<Result<_>>()?;

        // A one-sided book has no mid. Halving the side that exists invents a
        // price to trade against, which is the failure this codebase already
        // fixed once on Polymarket.
        let (Some(best_bid), Some(best_ask)) = (bids.first(), asks.first()) else {
            bail!("Coinbase returned a one-sided book for {product_id} — no mid exists");
        };

        let ts = book
            .time
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let (bid, ask) = (best_bid.price, best_ask.price);
        let mid = (bid + ask) / Decimal::TWO;

        Ok(Quote {
            instrument: id.clone(),
            bid,
            ask,
            mid,
            last: None,
            ts,
            book: Some(OrderBookSnapshot {
                token_id: product_id,
                bids,
                asks,
                spread: ask - bid,
                midpoint: mid,
                // Shaped for prediction markets. An implied probability is
                // meaningless for a spot pair, so it is zeroed rather than
                // filled with a mid a caller might read as one.
                implied_probability: Decimal::ZERO,
                timestamp: ts,
            }),
        })
    }

    #[instrument(skip(self), fields(venue = %self.id, symbol = %id.symbol))]
    async fn candles(
        &self,
        id: &InstrumentId,
        interval: CandleInterval,
        limit: usize,
    ) -> Result<Vec<Candle>> {
        let product_id = Self::to_product_id(&id.symbol);
        // Coinbase has no FOUR_HOUR granularity. `TWO_HOUR` is the widest that
        // divides four cleanly, so H4 is folded from pairs below rather than
        // served by the nearest-looking name — `SIX_HOUR` bars labelled
        // four-hour are wrong data that still charts.
        let (granularity, source_seconds, fold) = match interval {
            CandleInterval::H1 => ("ONE_HOUR", 3600i64, 1i64),
            CandleInterval::H4 => ("TWO_HOUR", 7200, 2),
            CandleInterval::D1 => ("ONE_DAY", 86400, 1),
        };

        // Coinbase requires an explicit window; it does not accept a bare
        // count. Asking for exactly `limit` periods back leaves no slack for
        // a missing bar, so this asks for a little more and truncates. Capped
        // at what one request can answer: asking for a wider window than that
        // returns a *truncated* series, and Coinbase returns the newest end of
        // it, so the count is short rather than the bars being wrong.
        let source_bars =
            ((limit as i64).saturating_add(2).saturating_mul(fold)).min(MAX_CANDLES_PER_REQUEST);
        let end = Utc::now();
        let start = end - chrono::Duration::seconds(source_seconds * source_bars);

        let path = format!(
            "/api/v3/brokerage/products/{}/candles",
            urlencoding::encode(&product_id)
        );
        let response: CandlesResponse = self
            .rest
            .get(
                &path,
                &[
                    ("start", start.timestamp().to_string()),
                    ("end", end.timestamp().to_string()),
                    ("granularity", granularity.to_string()),
                ],
            )
            .await
            .with_context(|| format!("Failed to fetch Coinbase candles for {product_id}"))?;

        let mut candles: Vec<Candle> = response
            .candles
            .iter()
            .map(|c| {
                let start: i64 = c.start.trim().parse().with_context(|| {
                    format!("Coinbase sent an unparseable candle start: {}", c.start)
                })?;
                Ok(Candle {
                    ts: Utc.timestamp_opt(start, 0).single().with_context(|| {
                        format!("Coinbase sent an impossible candle time: {start}")
                    })?,
                    open: money(&c.open, "candle open")?,
                    high: money(&c.high, "candle high")?,
                    low: money(&c.low, "candle low")?,
                    close: money(&c.close, "candle close")?,
                    volume: money(&c.volume, "candle volume")?,
                })
            })
            .collect::<Result<_>>()?;

        // Coinbase returns newest first; ATR and every other indicator here
        // expects oldest first, and a reversed series produces a plausible
        // number from the wrong data rather than an error.
        candles.sort_by_key(|c| c.ts);

        if fold > 1 {
            candles = fold_candles(candles, interval.hours() * 3600);
        }

        if candles.len() > limit {
            candles.drain(..candles.len() - limit);
        }
        Ok(candles)
    }

    #[instrument(
        skip(self, request),
        fields(venue = %self.id, symbol = %request.instrument.symbol())
    )]
    async fn place_order(&self, request: &OrderRequest) -> Result<OrderAck> {
        let product_id = Self::to_product_id(request.instrument.symbol());
        let side = match request.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };

        // Sizes and prices as strings. A JSON number loses scale on small
        // crypto quantities, which is the entire reason this codebase is
        // Decimal end to end.
        let base_size = request.qty.normalize().to_string();
        let configuration = match (request.kind, request.side) {
            (OrderKind::Limit { price }, _) => {
                let limit_price = price.normalize().to_string();
                match request.tif {
                    TimeInForce::Gtc => serde_json::json!({
                        "limit_limit_gtc": {
                            "base_size": base_size,
                            "limit_price": limit_price,
                            "post_only": false,
                        }
                    }),
                    TimeInForce::Gtd(end) => serde_json::json!({
                        "limit_limit_gtd": {
                            "base_size": base_size,
                            "limit_price": limit_price,
                            "end_time": end.to_rfc3339(),
                            "post_only": false,
                        }
                    }),
                    // Coinbase's only immediate limit configuration is
                    // fill-or-kill, which is not IOC — it refuses a partial
                    // fill rather than taking it — and spot crypto has no
                    // trading day for `Day` to end at. Rewriting either into
                    // GTC, which is what this adapter did first, turns "fill
                    // now or be gone" into a resting order that can fill
                    // minutes later at a price the decision no longer
                    // supports. Refused instead, as Alpaca refuses its own
                    // unsupported set.
                    other => bail!(
                        "Coinbase Advanced Trade cannot express {other:?} for a limit order; \
                         use Gtc or Gtd"
                    ),
                }
            }
            // Coinbase sizes a market BUY in the *quote* currency and a market
            // SELL in the base. Sending base_size on a buy comes back as an
            // HTTP 200 with `success: false`. Converting would need a price,
            // and a market buy sized from a quote fetched a moment earlier
            // fills a quantity the risk sizing never approved — so the caller
            // is told to price it instead.
            (OrderKind::Market, Side::Buy) => bail!(
                "Coinbase sizes market buys in the quote currency, not the base; \
                 submit {} as a limit order",
                request.instrument.symbol()
            ),
            (OrderKind::Market, Side::Sell) => serde_json::json!({
                "market_market_ioc": { "base_size": base_size }
            }),
        };

        let payload = serde_json::json!({
            "client_order_id": request.client_order_id,
            "product_id": product_id,
            "side": side,
            "order_configuration": configuration,
        });

        let response: CreateOrderResponse = self
            .rest
            .post(ORDERS_PATH, &payload)
            .await
            .context("Failed to submit the Coinbase order")?;

        // HTTP 200 does not mean accepted. Coinbase answers a refused order
        // with a 200 and `success: false`, so trusting the status code
        // records a rejection as a working order that never fills.
        if !response.success {
            let reason = response
                .error_response
                .as_ref()
                .map(|e| e.reason())
                .unwrap_or_else(|| "Coinbase rejected the order without a reason".to_string());
            return Ok(OrderAck {
                venue_order_id: response.order_id.clone().unwrap_or_default(),
                client_order_id: request.client_order_id.clone(),
                state: OrderState::Rejected(reason),
                filled_qty: Decimal::ZERO,
                avg_fill_price: None,
                fees: Decimal::ZERO,
            });
        }

        let success = response
            .success_response
            .as_ref()
            .context("Coinbase reported success with no order id")?;

        Ok(OrderAck {
            venue_order_id: success.order_id.clone(),
            client_order_id: success
                .client_order_id
                .clone()
                .unwrap_or_else(|| request.client_order_id.clone()),
            // Accepted, not filled: the create response carries no fill
            // information, and inventing one here is the "an order id is not
            // a fill" mistake this codebase was built to stop making.
            state: OrderState::Accepted,
            filled_qty: Decimal::ZERO,
            avg_fill_price: None,
            fees: Decimal::ZERO,
        })
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn get_order(&self, order: &OrderRef) -> Result<OrderAck> {
        match order {
            OrderRef::Venue(id) => to_ack(&self.fetch_order(id).await?),
            OrderRef::Client(client_order_id) => {
                // Coinbase has no lookup by client id, so this lists and
                // filters. It has to page: a miss here is not "no such order",
                // it is what the reconciler turns, once the TTL elapses, into
                // "never acknowledged by the venue" — writing off a row for an
                // order that may be live or already filled.
                let mut cursor: Option<String> = None;
                for _ in 0..MAX_ORDER_PAGES {
                    let mut params = vec![("limit", ORDER_PAGE_LIMIT.to_string())];
                    if let Some(c) = &cursor {
                        params.push(("cursor", c.clone()));
                    }
                    let response: OrdersResponse = self
                        .rest
                        .get(ORDER_HISTORY_PATH, &params)
                        .await
                        .context("Failed to list Coinbase orders")?;

                    if let Some(found) = response
                        .orders
                        .iter()
                        .find(|o| o.client_order_id.as_deref() == Some(client_order_id.as_str()))
                    {
                        return to_ack(found);
                    }

                    if !response.has_next {
                        break;
                    }
                    match response.cursor {
                        // A cursor that does not advance would page forever.
                        Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
                        _ => break,
                    }
                }
                bail!("Coinbase has no order with client id {client_order_id}")
            }
        }
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn cancel_order(&self, venue_order_id: &str) -> Result<()> {
        let ids = [venue_order_id.to_string()];
        self.batch_cancel(&ids)
            .await
            .with_context(|| format!("Failed to cancel Coinbase order {venue_order_id}"))
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn cancel_all(&self) -> Result<()> {
        let open = self.open_orders().await?;
        if open.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = open.iter().map(|o| o.venue_order_id.clone()).collect();
        let count = ids.len();
        self.batch_cancel(&ids)
            .await
            .context("Failed to cancel all Coinbase orders")?;
        info!(orders = count, "Cancelled resting Coinbase orders");
        Ok(())
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn open_orders(&self) -> Result<Vec<OrderAck>> {
        let mut acks = self
            .list_orders(&OPEN_STATUS)
            .await
            .context("Failed to list open Coinbase orders")?;

        // Best-effort, deliberately. `cancel_all` starts here, and returning
        // an error would have it stop before posting a single cancel — which
        // leaves every resting order live. Cancelling what was found beats
        // cancelling nothing.
        match self.list_orders(&PENDING_STATUSES).await {
            Ok(pending) => acks.extend(pending),
            Err(e) => warn!(
                venue = %self.id,
                error = %format!("{e:#}"),
                "Could not list pending Coinbase orders — the live set may be incomplete"
            ),
        }

        // An order can move from PENDING to OPEN between the two queries.
        let mut seen = HashSet::new();
        acks.retain(|a| seen.insert(a.venue_order_id.clone()));
        Ok(acks)
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn positions(&self) -> Result<Vec<Position>> {
        // Spot crypto has no position endpoint: a position *is* a non-zero
        // balance in the base currency. Reported against the quote currency
        // the agent trades, so it lines up with the symbols in the ledger.
        let accounts = self.accounts().await?;
        let mut out = Vec::new();
        let mut outside = Vec::new();

        for account in accounts.iter().filter(|a| is_usable(a)) {
            let Some(currency) = account.currency.as_deref() else {
                continue;
            };
            // The quote currency is cash, not a position.
            if is_cash(currency) {
                continue;
            }
            let Some(amount) = &account.available_balance else {
                continue;
            };
            let qty = money(&amount.value, "available_balance")?;
            if qty <= Decimal::ZERO {
                continue;
            }

            // Only what the agent actually trades. A Coinbase account is a
            // personal wallet as well as an agent account: staked ETH, an old
            // SOL bag, dust from a manual trade. Reporting those as positions
            // makes the reconciler see a holding it has no record of, which is
            // an `UntilResume` halt needing a human — so anyone enabling this
            // venue on an existing account would halt on the first cycle.
            match self.symbol_for_base(currency) {
                Some(symbol) => out.push(Position {
                    instrument: InstrumentId::new(self.id.clone(), symbol),
                    qty,
                    // Coinbase does not report a cost basis on the accounts
                    // endpoint. Zero would claim the position was free and make
                    // every P&L calculation wrong; the reconciler compares
                    // quantities, which is what this is for.
                    avg_entry: Decimal::ZERO,
                }),
                None => outside.push(format!("{currency} {}", qty.normalize())),
            }
        }

        if !outside.is_empty() {
            // Visible without being actionable by the reconciler: it is real
            // money, but it is not drift, and halting on it would train an
            // operator to resume without reading.
            warn!(
                venue = %self.id,
                holdings = %outside.join(", "),
                "Coinbase holds balances outside the configured universe — reported here \
                 only, since the reconciler would otherwise read them as drift and halt"
            );
        }

        Ok(out)
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn balance(&self) -> Result<Balance> {
        let accounts = self.accounts().await?;

        let mut cash = Decimal::ZERO;
        for account in accounts.iter().filter(|a| is_usable(a)) {
            let Some(currency) = account.currency.as_deref() else {
                continue;
            };
            if !is_cash(currency) {
                continue;
            }
            if let Some(amount) = &account.available_balance {
                cash += money(&amount.value, "available_balance")?;
            }
        }

        Ok(Balance {
            ccy: "USD".to_string(),
            available: cash,
            // Deliberately `None`.
            //
            // Account *equity* would be cash plus the marked value of every
            // crypto balance, and the accounts endpoint reports quantities
            // without prices. Computing it would mean a quote per holding,
            // and reporting cash here instead would read every entry as an
            // instant loss of the full notional — tripping the drawdown
            // breaker on a flat book. `None` makes the agent say it cannot
            // evaluate the breakers rather than evaluate them wrongly.
            total: None,
        })
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn settlement(&self, _id: &InstrumentId) -> Result<Option<Settlement>> {
        // Spot crypto never settles; positions are closed by trading out.
        Ok(None)
    }
}

impl CoinbaseVenue {
    /// Every account row, paged, reused for a moment.
    ///
    /// Coinbase creates one row per supported currency and pages by cursor, so
    /// reading the first page alone drops both cash and holdings — and a
    /// dropped holding is a `missing_on_venue` orphan, which halts the agent
    /// for a human with a perfectly correct book.
    async fn accounts(&self) -> Result<Arc<Vec<Account>>> {
        let mut cache = self.accounts_cache.lock().await;
        if let Some((fetched, accounts)) = cache.as_ref() {
            if fetched.elapsed() < self.accounts_cache_ttl {
                return Ok(accounts.clone());
            }
        }

        let mut accounts = Vec::new();
        let mut cursor: Option<String> = None;
        let mut truncated = true;

        for _ in 0..MAX_ACCOUNT_PAGES {
            let mut params = vec![("limit", ACCOUNT_PAGE_LIMIT.to_string())];
            if let Some(c) = &cursor {
                params.push(("cursor", c.clone()));
            }
            let response: AccountsResponse = self
                .rest
                .get(ACCOUNTS_PATH, &params)
                .await
                .context("Failed to fetch Coinbase accounts")?;
            accounts.extend(response.accounts);

            if !response.has_next {
                truncated = false;
                break;
            }
            match response.cursor {
                // A cursor that does not advance — or a `has_next` with no
                // cursor at all — cannot finish the list. Breaking out of the
                // loop without saying so would report the partial answer as
                // the whole one, which is the bug this paging exists to fix.
                Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
                _ => break,
            }
        }

        if truncated {
            // A short account list is a wrong balance and a missing position,
            // both of which read as something else entirely downstream.
            bail!(
                "Coinbase account listing did not finish within {MAX_ACCOUNT_PAGES} pages — \
                 refusing to report a partial balance"
            );
        }

        let accounts = Arc::new(accounts);
        *cache = Some((Instant::now(), accounts.clone()));
        Ok(accounts)
    }

    /// Cancel, and check what Coinbase actually did.
    ///
    /// `batch_cancel` answers HTTP 200 with a per-order result: the envelope
    /// says nothing about whether anything was cancelled. Reading only the
    /// status code has the reconciler write a still-resting order off as
    /// EXPIRED and abandon it, leaving it live to fill into a position the
    /// ledger no longer carries.
    async fn batch_cancel(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let payload = serde_json::json!({ "order_ids": ids });
        let response: BatchCancelResponse = self.rest.post(BATCH_CANCEL_PATH, &payload).await?;

        if response.results.is_empty() {
            bail!(
                "Coinbase returned no cancel result for {} order(s) — whether they are \
                 still resting is unknown",
                ids.len()
            );
        }

        let mut failed = Vec::new();
        for result in &response.results {
            if result.success {
                continue;
            }
            let id = result.order_id.as_deref().unwrap_or("<unknown>");
            if result.is_already_resolved() {
                warn!(
                    order_id = id,
                    reason = result.reason(),
                    "Coinbase would not cancel an order that is already gone — \
                     nothing is left resting under that id"
                );
                continue;
            }
            failed.push(format!("{id} ({})", result.reason()));
        }

        if !failed.is_empty() {
            bail!(
                "Coinbase could not cancel {} order(s): {}",
                failed.len(),
                failed.join(", ")
            );
        }
        Ok(())
    }

    /// Orders in the given statuses, paged.
    ///
    /// The first page is not the whole answer: `cancel_all` builds its id list
    /// from this, and the orphan audit concludes that an order it cannot see
    /// does not exist.
    async fn list_orders(&self, statuses: &[&str]) -> Result<Vec<OrderAck>> {
        let mut acks = Vec::new();
        let mut cursor: Option<String> = None;

        for _ in 0..MAX_ORDER_PAGES {
            let mut params: Vec<(&str, String)> = statuses
                .iter()
                .map(|s| ("order_status", (*s).to_string()))
                .collect();
            params.push(("limit", ORDER_PAGE_LIMIT.to_string()));
            if let Some(c) = &cursor {
                params.push(("cursor", c.clone()));
            }

            let response: OrdersResponse = self.rest.get(ORDER_HISTORY_PATH, &params).await?;
            acks.extend(response.orders.iter().map(to_ack_lossy));

            if !response.has_next {
                return Ok(acks);
            }
            match response.cursor {
                Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
                _ => break,
            }
        }

        // Reported rather than raised: `cancel_all` starts here, and an error
        // would have it cancel nothing at all.
        warn!(
            venue = %self.id,
            orders = acks.len(),
            "Could not page Coinbase orders to the end — the live set may be incomplete"
        );
        Ok(acks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use serde_json::{json, Value};
    use wiremock::matchers::{
        body_partial_json, header_exists, method, path, query_param, query_param_is_missing,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A throwaway P-256 key. Generated for these tests and used nowhere else.
    const PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgevZzL1gdAFr88hb2\n\
OF/2NxApJCzGCEDdfSp6VQO30hyhRANCAAQRWz+jn65BtOMvdyHKcvjBeBSDZH2r\n\
1RTwjmYSi9R/zpBnuQ4EiMnCqfMPWiZqB4QdbAd0E7oH50VpuZ1P087G\n\
-----END PRIVATE KEY-----";

    fn venue(server: &MockServer, symbols: &[&str]) -> CoinbaseVenue {
        CoinbaseVenue::new(
            CoinbaseConfig::new("organizations/o/apiKeys/k", PEM)
                .with_base_url(server.uri())
                .with_symbols(symbols.iter().map(|s| s.to_string()).collect()),
        )
        .expect("venue builds")
    }

    fn btc_product() -> Value {
        json!({
            "product_id": "BTC-USD",
            "base_currency_id": "BTC",
            "quote_currency_id": "USD",
            "quote_increment": "0.01",
            "base_increment": "0.00000001",
            "quote_min_size": "1",
            "base_min_size": "0.000016",
            "status": "online",
            "product_type": "SPOT",
            "display_name": "BTC-USD"
        })
    }

    fn instrument() -> Instrument {
        Instrument {
            id: InstrumentId::new(VenueId::new(DEFAULT_VENUE_ID), "BTC/USD"),
            asset_class: AssetClass::CryptoSpot,
            display_name: "BTC-USD".to_string(),
            quote_ccy: "USD".to_string(),
            tick_size: Some(dec!(0.01)),
            lot_size: Some(dec!(0.00000001)),
            min_notional: Some(dec!(1)),
            min_qty: Some(dec!(0.000016)),
            fractional: true,
            meta: InstrumentMeta::Spot {
                base: "BTC".to_string(),
            },
        }
    }

    #[test]
    fn pair_symbols_round_trip_between_the_two_spellings() {
        // Coinbase says BTC-USD; the config and Alpaca say BTC/USD. A symbol
        // configured once has to work on either venue.
        assert_eq!(CoinbaseVenue::to_product_id("BTC/USD"), "BTC-USD");
        assert_eq!(CoinbaseVenue::to_product_id("btc/usd"), "BTC-USD");
        assert_eq!(CoinbaseVenue::to_symbol("BTC-USD"), "BTC/USD");
    }

    #[tokio::test]
    async fn every_request_carries_a_bearer_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/products"))
            // The JWT itself is asserted in auth.rs; here the point is that
            // the REST layer attaches one at all.
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"products": []})))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        venue
            .list_instruments(&ScanFilter::default())
            .await
            .expect("an authenticated request");
    }

    #[tokio::test]
    async fn list_instruments_maps_coinbase_increments_onto_instrument_limits() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/products"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"products": [btc_product()]})),
            )
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let instruments = venue
            .list_instruments(&ScanFilter::default())
            .await
            .unwrap();

        assert_eq!(instruments.len(), 1);
        let i = &instruments[0];
        assert_eq!(i.symbol(), "BTC/USD", "normalised out of BTC-USD");
        assert_eq!(i.tick_size, Some(dec!(0.01)));
        assert_eq!(i.lot_size, Some(dec!(0.00000001)));
        // Both floors matter at micro capital: Coinbase gates on value *and*
        // size, and a $6 slice of BTC can clear one and not the other.
        assert_eq!(i.min_notional, Some(dec!(1)));
        assert_eq!(i.min_qty, Some(dec!(0.000016)));
        assert!(i.fractional);
    }

    #[tokio::test]
    async fn a_product_coinbase_will_not_trade_is_skipped() {
        let server = MockServer::start().await;
        let mut disabled = btc_product();
        disabled["trading_disabled"] = json!(true);
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"products": [disabled]})))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        assert!(
            venue
                .list_instruments(&ScanFilter::default())
                .await
                .unwrap()
                .is_empty(),
            "an order against a disabled product is a guaranteed rejection"
        );
    }

    #[tokio::test]
    async fn only_configured_symbols_are_returned() {
        let server = MockServer::start().await;
        let mut doge = btc_product();
        doge["product_id"] = json!("DOGE-USD");
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/products"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"products": [btc_product(), doge]})),
            )
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let found = venue
            .list_instruments(&ScanFilter::default())
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].symbol(), "BTC/USD");
    }

    #[tokio::test]
    async fn a_quote_carries_both_sides_and_the_depth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/product_book"))
            .and(query_param("product_id", "BTC-USD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "pricebook": {
                    "product_id": "BTC-USD",
                    "bids": [{"price": "60000.00", "size": "0.5"},
                             {"price": "59999.00", "size": "1.25"}],
                    "asks": [{"price": "60010.00", "size": "0.3"}],
                    "time": "2026-09-21T10:00:00Z"
                }
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let quote = venue.quote(&instrument().id).await.unwrap();

        assert_eq!(quote.bid, dec!(60000.00));
        assert_eq!(quote.ask, dec!(60010.00));
        assert_eq!(quote.mid, dec!(60005.00));
        let book = quote.book.as_ref().expect("Coinbase publishes depth");
        assert_eq!(book.bids.len(), 2);
        assert_eq!(book.bids[1].size, dec!(1.25));
    }

    /// Halving the side that exists invents a price to trade against. This
    /// codebase has already been bitten by exactly that on Polymarket.
    #[tokio::test]
    async fn a_one_sided_book_is_an_error_not_a_halved_mid() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/product_book"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "pricebook": {"product_id": "BTC-USD",
                              "bids": [{"price": "60000.00", "size": "0.5"}],
                              "asks": []}
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let err = venue.quote(&instrument().id).await.unwrap_err();
        assert!(format!("{err:#}").contains("one-sided"), "{err:#}");
    }

    #[tokio::test]
    async fn candles_come_back_oldest_first() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/products/BTC-USD/candles"))
            .and(query_param("granularity", "ONE_HOUR"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                // Coinbase returns newest first.
                "candles": [
                    {"start": "1758448800", "low": "1", "high": "4", "open": "2", "close": "3", "volume": "10"},
                    {"start": "1758445200", "low": "0", "high": "3", "open": "1", "close": "2", "volume": "9"}
                ]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let candles = venue
            .candles(&instrument().id, CandleInterval::H1, 10)
            .await
            .unwrap();

        assert_eq!(candles.len(), 2);
        assert!(
            candles[0].ts < candles[1].ts,
            "ATR and every other indicator expects oldest first; reversed \
             input produces a plausible number from the wrong data"
        );
        assert_eq!(candles[0].close, dec!(2));
    }

    #[tokio::test]
    async fn place_order_sends_strings_for_size_and_price() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/brokerage/orders"))
            // Every field here is one Coinbase rejects or misreads if wrong.
            // A JSON *number* for base_size loses scale on small crypto
            // quantities, which is the reason this codebase is Decimal.
            .and(body_partial_json(json!({
                "client_order_id": "cid-1",
                "product_id": "BTC-USD",
                "side": "BUY",
                "order_configuration": {
                    "limit_limit_gtc": {
                        "base_size": "0.001",
                        "limit_price": "60000",
                        "post_only": false
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "success_response": {"order_id": "cb-1", "client_order_id": "cid-1"}
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let ack = venue
            .place_order(&OrderRequest {
                instrument: instrument(),
                side: Side::Buy,
                kind: OrderKind::Limit { price: dec!(60000) },
                qty: dec!(0.001),
                tif: crate::venue::types::TimeInForce::Gtc,
                extended_hours: false,
                client_order_id: "cid-1".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(ack.venue_order_id, "cb-1");
        assert_eq!(
            ack.state,
            OrderState::Accepted,
            "an order id is not a fill — the create response carries none"
        );
        assert_eq!(ack.filled_qty, Decimal::ZERO);
        assert_eq!(ack.avg_fill_price, None);
    }

    /// The shape that makes Coinbase different from Alpaca: a *refused* order
    /// still answers HTTP 200. An adapter that trusts the status code records
    /// it as working and waits forever for a fill.
    #[tokio::test]
    async fn a_rejection_arrives_as_http_200_and_is_still_a_rejection() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/brokerage/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": false,
                "error_response": {
                    "error": "INSUFFICIENT_FUND",
                    "message": "Insufficient balance in source account"
                }
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let ack = venue
            .place_order(&OrderRequest {
                instrument: instrument(),
                side: Side::Buy,
                kind: OrderKind::Limit { price: dec!(60000) },
                qty: dec!(0.001),
                tif: crate::venue::types::TimeInForce::Gtc,
                extended_hours: false,
                client_order_id: "cid-1".to_string(),
            })
            .await
            .unwrap();

        match ack.state {
            OrderState::Rejected(reason) => {
                assert!(reason.contains("Insufficient balance"), "{reason}")
            }
            other => panic!("a refused order must not read as {other:?}"),
        }
    }

    #[tokio::test]
    async fn balance_sums_the_cash_accounts_and_reports_no_equity() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [
                    {"currency": "USD",  "available_balance": {"value": "120.50", "currency": "USD"}},
                    {"currency": "USDC", "available_balance": {"value": "30.00",  "currency": "USDC"}},
                    {"currency": "BTC",  "available_balance": {"value": "0.01",   "currency": "BTC"}}
                ]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let balance = venue.balance().await.unwrap();

        assert_eq!(balance.available, dec!(150.50), "USD + USDC, not the BTC");
        assert_eq!(
            balance.total, None,
            "equity needs a price per holding, and reporting cash here would \
             read every entry as an instant loss of the full notional"
        );
    }

    #[tokio::test]
    async fn positions_are_the_non_cash_balances() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [
                    {"currency": "USD", "available_balance": {"value": "120.50"}},
                    {"currency": "BTC", "available_balance": {"value": "0.01"}},
                    {"currency": "ETH", "available_balance": {"value": "0"}}
                ]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let positions = venue.positions().await.unwrap();

        assert_eq!(
            positions.len(),
            1,
            "cash is not a position, nor is a zero balance"
        );
        assert_eq!(positions[0].instrument.symbol, "BTC/USD");
        assert_eq!(positions[0].qty, dec!(0.01));
    }

    #[test]
    fn a_cancel_that_caught_a_partial_fill_is_not_simply_cancelled() {
        // The filled part is a real position and must stay exitable.
        let order = Order {
            order_id: "cb-1".to_string(),
            client_order_id: Some("cid-1".to_string()),
            product_id: Some("BTC-USD".to_string()),
            side: Some("BUY".to_string()),
            status: Some("CANCELLED".to_string()),
            filled_size: Some("0.0004".to_string()),
            average_filled_price: Some("60000".to_string()),
            total_fees: Some("0.01".to_string()),
            reject_reason: None,
            reject_message: None,
        };
        assert_eq!(to_order_state(&order), OrderState::PartiallyFilled);
    }

    #[test]
    fn a_queued_order_is_accepted_not_unknown() {
        // It exists at the venue and may fill. Unknown would have
        // reconciliation re-query something behaving perfectly normally.
        let order = Order {
            order_id: "cb-1".to_string(),
            client_order_id: None,
            product_id: None,
            side: None,
            status: Some("QUEUED".to_string()),
            filled_size: Some("0".to_string()),
            average_filled_price: None,
            total_fees: None,
            reject_reason: None,
            reject_message: None,
        };
        assert_eq!(to_order_state(&order), OrderState::Accepted);
    }

    #[test]
    fn an_unrecognised_status_is_unknown_rather_than_a_guess() {
        let order = Order {
            order_id: "cb-1".to_string(),
            client_order_id: None,
            product_id: None,
            side: None,
            status: Some("SOMETHING_NEW".to_string()),
            filled_size: None,
            average_filled_price: None,
            total_fees: None,
            reject_reason: None,
            reject_message: None,
        };
        assert_eq!(
            to_order_state(&order),
            OrderState::Unknown,
            "guessing is how a live order gets abandoned or duplicated"
        );
    }

    /// Coinbase sends "0" for the average price before anything fills.
    /// Recording that would book a fill at zero and turn a loss into a
    /// reported profit.
    #[test]
    fn a_zero_average_price_is_not_a_price() {
        let order = Order {
            order_id: "cb-1".to_string(),
            client_order_id: None,
            product_id: None,
            side: None,
            status: Some("OPEN".to_string()),
            filled_size: Some("0".to_string()),
            average_filled_price: Some("0".to_string()),
            total_fees: None,
            reject_reason: None,
            reject_message: None,
        };
        assert_eq!(to_ack(&order).unwrap().avg_fill_price, None);
    }

    /// The position symbol has to match the configured one, not a guess.
    ///
    /// A `BTC/USDC` universe reporting positions as `BTC/USD` makes
    /// reconciliation see two mismatches at once — one held at the venue and
    /// absent locally, one the reverse — and halt a perfectly healthy agent
    /// `UntilResume`.
    #[tokio::test]
    async fn positions_use_the_configured_quote_currency() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [
                    {"currency": "USDC", "available_balance": {"value": "500"}},
                    {"currency": "BTC",  "available_balance": {"value": "0.01"}}
                ]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USDC"]);
        let positions = venue.positions().await.unwrap();

        assert_eq!(positions.len(), 1);
        assert_eq!(
            positions[0].instrument.symbol, "BTC/USDC",
            "a guessed /USD suffix reconciles against nothing"
        );
    }

    /// A Coinbase account is a personal wallet as well as an agent account.
    /// Staked ETH or an old SOL bag is real money, but it is not *drift*: the
    /// reconciler reads a holding it has no record of as `missing_locally`,
    /// which is an `UntilResume` halt needing a human. Reporting these would
    /// halt anyone who enables this venue on an account they already use.
    #[tokio::test]
    async fn a_holding_outside_the_configured_universe_is_not_a_position() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [
                    {"currency": "SOL", "available_balance": {"value": "2"}},
                    {"currency": "BTC", "available_balance": {"value": "0.01"}}
                ]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let positions = venue.positions().await.unwrap();

        assert_eq!(positions.len(), 1, "only what the agent trades");
        assert_eq!(positions[0].instrument.symbol, "BTC/USD");
    }

    /// An explicit `active: false` is a suspended sub-account. Counting its
    /// cash sizes positions against money the agent will find out it cannot
    /// spend one rejected order at a time.
    #[tokio::test]
    async fn a_deactivated_account_counts_for_neither_cash_nor_positions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [
                    {"currency": "USD", "available_balance": {"value": "100"}, "active": true},
                    {"currency": "USD", "available_balance": {"value": "900"}, "active": false},
                    {"currency": "BTC", "available_balance": {"value": "0.5"}, "active": false}
                ]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        assert_eq!(venue.balance().await.unwrap().available, dec!(100));
        assert!(venue.positions().await.unwrap().is_empty());
    }

    /// Coinbase creates a row per supported currency and pages by cursor. The
    /// first page is not the answer: cash on page two reads as a zero bankroll,
    /// and a holding on page two reads as an orphan and halts the agent.
    #[tokio::test]
    async fn accounts_are_paged_to_the_end() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/accounts"))
            .and(query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [{"currency": "ETH", "available_balance": {"value": "1"}}],
                "has_next": true,
                "cursor": "page2"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/accounts"))
            .and(query_param("cursor", "page2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [
                    {"currency": "USD", "available_balance": {"value": "250"}},
                    {"currency": "BTC", "available_balance": {"value": "0.02"}}
                ],
                "has_next": false
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        assert_eq!(
            venue.balance().await.unwrap().available,
            dec!(250),
            "cash on the second page is still cash"
        );
        let positions = venue.positions().await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].instrument.symbol, "BTC/USD");
    }

    /// A truncated account list is a wrong balance and a missing position,
    /// both of which read as something else entirely downstream — so it is an
    /// error rather than a short answer.
    #[tokio::test]
    async fn an_account_listing_that_never_ends_is_an_error_not_a_partial_balance() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [{"currency": "USD", "available_balance": {"value": "1"}}],
                "has_next": true,
                "cursor": "always-more"
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let err = venue.balance().await.unwrap_err();
        assert!(format!("{err:#}").contains("partial balance"), "{err:#}");
    }

    /// `balance()` and `positions()` are two projections of one snapshot. Each
    /// paging the same rate-limited endpoint, and minting a JWT per request,
    /// also lets the two disagree with each other within a single pass.
    #[tokio::test]
    async fn balance_and_positions_share_one_account_fetch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [{"currency": "USD", "available_balance": {"value": "10"}}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        venue.balance().await.unwrap();
        venue.positions().await.unwrap();
        // `expect(1)` is asserted on drop.
    }

    #[tokio::test]
    async fn the_account_snapshot_is_refetched_once_it_expires() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accounts": [{"currency": "USD", "available_balance": {"value": "10"}}]
            })))
            .expect(2)
            .mount(&server)
            .await;

        let venue = CoinbaseVenue::new(
            CoinbaseConfig::new("organizations/o/apiKeys/k", PEM)
                .with_base_url(server.uri())
                .with_symbols(vec!["BTC/USD".to_string()])
                .with_accounts_cache_ttl(Duration::ZERO),
        )
        .unwrap();
        venue.balance().await.unwrap();
        venue.positions().await.unwrap();
    }

    // ---- orders ---------------------------------------------------------

    fn open_order(id: &str) -> Value {
        json!({
            "order_id": id,
            "client_order_id": format!("cid-{id}"),
            "product_id": "BTC-USD",
            "status": "OPEN",
            "filled_size": "0",
            "total_fees": "0"
        })
    }

    async fn mount_no_pending(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/orders/historical/batch"))
            .and(query_param("order_status", "PENDING"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"orders": []})))
            .mount(server)
            .await;
    }

    /// `batch_cancel` answers HTTP 200 with a per-order result. Reading only
    /// the status code has the reconciler write a still-resting order off as
    /// EXPIRED and abandon it — leaving it live to fill into a position the
    /// ledger no longer carries.
    #[tokio::test]
    async fn a_refused_cancel_is_an_error_not_a_silent_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/brokerage/orders/batch_cancel"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "order_id": "cb-1",
                    "success": false,
                    "failure_reason": "UNKNOWN_CANCEL_FAILURE_REASON"
                }]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let err = venue.cancel_order("cb-1").await.unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains("cb-1"), "{rendered}");
        assert!(
            rendered.contains("UNKNOWN_CANCEL_FAILURE_REASON"),
            "{rendered}"
        );
    }

    /// An order that is already gone cannot be cancelled and does not need to
    /// be. Treating that as a failure would have the reconciler retry forever.
    #[tokio::test]
    async fn a_cancel_for_an_order_that_is_already_gone_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/brokerage/orders/batch_cancel"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "order_id": "cb-1",
                    "success": false,
                    "failure_reason": "DUPLICATE_CANCEL_REQUEST"
                }]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        venue
            .cancel_order("cb-1")
            .await
            .expect("nothing is resting");
    }

    #[tokio::test]
    async fn a_cancel_coinbase_answers_with_nothing_is_not_a_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/brokerage/orders/batch_cancel"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let err = venue.cancel_order("cb-1").await.unwrap_err();
        assert!(
            format!("{err:#}").contains("still resting is unknown"),
            "{err:#}"
        );
    }

    /// The kill switch builds its id list from `open_orders`. An order past
    /// the first page is one it never cancels.
    #[tokio::test]
    async fn open_orders_pages_past_the_first_page() {
        let server = MockServer::start().await;
        mount_no_pending(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/orders/historical/batch"))
            .and(query_param("order_status", "OPEN"))
            .and(query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "orders": [open_order("cb-1")], "has_next": true, "cursor": "next"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/orders/historical/batch"))
            .and(query_param("order_status", "OPEN"))
            .and(query_param("cursor", "next"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "orders": [open_order("cb-2")], "has_next": false
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let open = venue.open_orders().await.unwrap();
        let ids: Vec<&str> = open.iter().map(|o| o.venue_order_id.as_str()).collect();
        assert_eq!(ids, vec!["cb-1", "cb-2"], "page two is not optional");
    }

    /// `to_order_state` calls PENDING and QUEUED accepted — the order exists
    /// at the venue and can fill — so a kill switch that only asks for OPEN
    /// leaves a just-submitted order resting through a halt.
    #[tokio::test]
    async fn open_orders_includes_pending_and_queued() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/orders/historical/batch"))
            .and(query_param("order_status", "OPEN"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"orders": []})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/orders/historical/batch"))
            .and(query_param("order_status", "PENDING"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "orders": [{
                    "order_id": "cb-9", "status": "PENDING",
                    "filled_size": "0", "total_fees": "0"
                }]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let open = venue.open_orders().await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].venue_order_id, "cb-9");
        assert_eq!(open[0].state, OrderState::Accepted);
    }

    /// One unparseable number must not cost the whole list: `cancel_all`
    /// starts with `open_orders()?`, so an error there means the kill switch
    /// posts no cancel at all and every resting order survives it.
    #[tokio::test]
    async fn one_malformed_order_does_not_stop_the_kill_switch() {
        let server = MockServer::start().await;
        mount_no_pending(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/orders/historical/batch"))
            .and(query_param("order_status", "OPEN"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "orders": [
                    {"order_id": "cb-bad", "status": "OPEN", "total_fees": ""},
                    open_order("cb-ok")
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v3/brokerage/orders/batch_cancel"))
            .and(body_partial_json(json!({"order_ids": ["cb-bad", "cb-ok"]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {"order_id": "cb-bad", "success": true},
                    {"order_id": "cb-ok", "success": true}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        venue.cancel_all().await.expect("both orders are cancelled");
    }

    /// A miss here is not "no such order": the reconciler turns it, once the
    /// TTL elapses, into "never acknowledged by the venue" and writes off a
    /// row for an order that may be live or already filled.
    #[tokio::test]
    async fn a_client_id_lookup_pages_before_giving_up() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/orders/historical/batch"))
            .and(query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "orders": [open_order("cb-1")], "has_next": true, "cursor": "next"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/orders/historical/batch"))
            .and(query_param("cursor", "next"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "orders": [open_order("cb-2")], "has_next": false
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let ack = venue
            .get_order(&OrderRef::Client("cid-cb-2".to_string()))
            .await
            .expect("the order is on page two, not absent");
        assert_eq!(ack.venue_order_id, "cb-2");
    }

    // ---- order configuration --------------------------------------------

    fn request(kind: OrderKind, side: Side, tif: TimeInForce) -> OrderRequest {
        OrderRequest {
            instrument: instrument(),
            side,
            kind,
            qty: dec!(0.001),
            tif,
            extended_hours: false,
            client_order_id: "cid-1".to_string(),
        }
    }

    async fn accepting_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/brokerage/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "success_response": {"order_id": "cb-1", "client_order_id": "cid-1"}
            })))
            .mount(&server)
            .await;
        server
    }

    /// An IOC order's whole contract is "fill now or be gone". Sending it as
    /// `limit_limit_gtc` — which is what this adapter did first — leaves a
    /// resting order that can fill minutes later at a price the decision no
    /// longer supports.
    #[tokio::test]
    async fn an_unsupported_time_in_force_is_refused_not_rewritten() {
        let server = accepting_server().await;
        let venue = venue(&server, &["BTC/USD"]);

        for tif in [TimeInForce::Ioc, TimeInForce::Day] {
            let err = venue
                .place_order(&request(
                    OrderKind::Limit { price: dec!(60000) },
                    Side::Buy,
                    tif,
                ))
                .await
                .unwrap_err();
            assert!(
                format!("{err:#}").contains("cannot express"),
                "{err:#} for {tif:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_good_till_date_order_carries_its_expiry() {
        let server = MockServer::start().await;
        let expiry = Utc.with_ymd_and_hms(2026, 9, 21, 12, 0, 0).unwrap();
        Mock::given(method("POST"))
            .and(path("/api/v3/brokerage/orders"))
            .and(body_partial_json(json!({
                "order_configuration": {
                    "limit_limit_gtd": {
                        "base_size": "0.001",
                        "limit_price": "60000",
                        "end_time": "2026-09-21T12:00:00+00:00"
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "success_response": {"order_id": "cb-1"}
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        venue
            .place_order(&request(
                OrderKind::Limit { price: dec!(60000) },
                Side::Buy,
                TimeInForce::Gtd(expiry),
            ))
            .await
            .expect("a GTD order is expressible");
    }

    /// Coinbase sizes a market BUY in the quote currency. Sending `base_size`
    /// comes back as an HTTP 200 with `success: false`, and converting would
    /// need a price — filling a quantity the risk sizing never approved.
    #[tokio::test]
    async fn a_market_buy_is_refused_because_coinbase_sizes_it_in_quote() {
        let server = accepting_server().await;
        let venue = venue(&server, &["BTC/USD"]);
        let err = venue
            .place_order(&request(OrderKind::Market, Side::Buy, TimeInForce::Ioc))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("quote currency"), "{err:#}");
    }

    #[tokio::test]
    async fn a_market_sell_is_sized_in_the_base_currency() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/brokerage/orders"))
            .and(body_partial_json(json!({
                "side": "SELL",
                "order_configuration": {"market_market_ioc": {"base_size": "0.001"}}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "success_response": {"order_id": "cb-1"}
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        venue
            .place_order(&request(OrderKind::Market, Side::Sell, TimeInForce::Ioc))
            .await
            .expect("a market sell is sized in base");
    }

    // ---- candles and products -------------------------------------------

    /// Coinbase has no FOUR_HOUR granularity. Sending `SIX_HOUR` and computing
    /// the window from four hours — which is what this adapter did first —
    /// returns six-hour bars labelled four-hour, and about two thirds of the
    /// count asked for. Both halves produce a plausible number from the wrong
    /// data rather than an error.
    #[tokio::test]
    async fn four_hour_candles_are_folded_from_two_hour_bars() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/products/BTC-USD/candles"))
            .and(query_param("granularity", "TWO_HOUR"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                // 08:00 and 10:00 fold into one 08:00 bucket; 12:00 opens the next.
                // 1758441600 is 4h-aligned. It and 1758448800 fold into one
                // bucket; 1758456000 opens the next.
                "candles": [
                    {"start": "1758456000", "low": "5", "high": "9", "open": "6", "close": "7", "volume": "3"},
                    {"start": "1758448800", "low": "2", "high": "8", "open": "3", "close": "4", "volume": "2"},
                    {"start": "1758441600", "low": "1", "high": "6", "open": "2", "close": "3", "volume": "1"}
                ]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let candles = venue
            .candles(&instrument().id, CandleInterval::H4, 10)
            .await
            .unwrap();

        assert_eq!(
            candles.len(),
            2,
            "three two-hour bars make two four-hour ones"
        );
        let first = &candles[0];
        assert_eq!(
            first.ts.timestamp() % 14400,
            0,
            "buckets align to the interval"
        );
        assert_eq!(first.open, dec!(2), "the oldest bar in the bucket opens it");
        assert_eq!(first.close, dec!(4), "the newest closes it");
        assert_eq!(first.high, dec!(8), "the highest high across both");
        assert_eq!(first.low, dec!(1), "the lowest low across both");
        assert_eq!(first.volume, dec!(3), "volume sums");
        assert_eq!(candles[1].open, dec!(6), "the next bucket starts fresh");
    }

    /// `/products` pages by offset. A configured symbol past the first page
    /// was dropped with a "does not list this product" warning — which reads
    /// as a delisting rather than as truncation.
    #[tokio::test]
    async fn products_are_paged_until_every_configured_symbol_is_found() {
        let server = MockServer::start().await;
        let filler: Vec<Value> = (0..PRODUCT_PAGE_LIMIT)
            .map(|i| {
                let mut p = btc_product();
                p["product_id"] = json!(format!("FILL{i}-USD"));
                p
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/products"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"products": filler})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/brokerage/products"))
            .and(query_param("offset", PRODUCT_PAGE_LIMIT.to_string()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"products": [btc_product()]})),
            )
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let found = venue
            .list_instruments(&ScanFilter::default())
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].symbol(), "BTC/USD");
    }
}
