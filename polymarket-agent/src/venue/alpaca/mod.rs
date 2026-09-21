//! Alpaca Markets venue adapter.
//!
//! Alpaca serves US equities and US crypto spot from one account and one key
//! pair, split across two hosts: a trading API (paper or live) and a market
//! data API. This module implements [`Venue`] over both.
//!
//! Three behaviours are worth knowing before reading the code:
//!
//! * **Asset class is inferred from the symbol.** Alpaca spells crypto pairs
//!   `BTC/USD` and equities `AAPL`, so the slash is the discriminator, and it
//!   decides which data endpoint, which time-in-force set and which session
//!   rules apply.
//! * **Extended and overnight equity sessions are limit-only.** An order sent
//!   outside 09:30–16:00 ET needs `extended_hours: true`, a limit price and a
//!   `day`/`gtc` time-in-force. Violations are refused here rather than being
//!   bounced by Alpaca after the fact.
//! * **Money never touches a float.** Alpaca sends decimals as JSON strings on
//!   the trading API and as JSON numbers on the data API; both are decoded by
//!   [`models::value_to_decimal`], which never calls `Decimal::from_f64`.

pub mod models;
pub mod rest;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

use crate::market::models::{OrderBookSnapshot, PriceLevel};
use crate::venue::session::{SessionKind, SessionState, TradingSession};
use crate::venue::types::{
    AssetClass, Balance, Candle, CandleInterval, Instrument, InstrumentId, InstrumentMeta,
    OrderAck, OrderKind, OrderRef, OrderRequest, Position, Quote, ScanFilter, Settlement,
    TimeInForce, VenueCapabilities, VenueId,
};
use crate::venue::Venue;

use self::models::{
    is_crypto_symbol, order_state, side_str, split_pair, tif_str, AlpacaAccount, AlpacaAsset,
    AlpacaClock, AlpacaOrder, AlpacaPosition, BarsResponse, CancelAllEntry,
    CryptoOrderbooksResponse, LatestQuotesResponse, NewOrder,
};
use self::rest::{AlpacaRest, Api};

/// Paper trading host — the default, because an agent should have to opt in to
/// spending real money.
pub const PAPER_TRADING_BASE_URL: &str = "https://paper-api.alpaca.markets";
/// Live trading host.
pub const LIVE_TRADING_BASE_URL: &str = "https://api.alpaca.markets";
/// Market data host, shared by paper and live accounts.
pub const DATA_BASE_URL: &str = "https://data.alpaca.markets";

const DEFAULT_VENUE_ID: &str = "alpaca";
/// `iex` is free on every plan; `sip` needs a paid subscription and returns
/// 403 without one, so it is opt-in.
const DEFAULT_FEED: &str = "iex";

const ASSET_CLASS_EQUITY: &str = "us_equity";
const ASSET_CLASS_CRYPTO: &str = "crypto";

/// Alpaca caps a single crypto order at $200,000 notional.
/// Alpaca caps `/v2/orders` at 500 rows per request.
const ORDER_PAGE_SIZE: usize = 500;

const CRYPTO_MAX_NOTIONAL: Decimal = dec!(200000);
/// US equities quote in whole cents at and above $1.00. Sub-dollar names are
/// allowed four decimals, so this is the conservative increment: rounding a
/// $0.50 stock to the cent is legal, the reverse is not.
const EQUITY_TICK_SIZE: Decimal = dec!(0.01);
/// Alpaca's minimum notional for a fractional or notional equity order.
const EQUITY_MIN_NOTIONAL: Decimal = dec!(1);
/// Alpaca's hard cap on bars per request.
const MAX_BARS: usize = 10_000;

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The clock only changes at session boundaries; one fetch per cycle is
/// plenty and keeps `/v2/clock` off the per-call path.
const DEFAULT_CLOCK_TTL: Duration = Duration::from_secs(60);
/// How long an account snapshot is reused.
///
/// Cash and equity come from one payload, and several callers read one or the
/// other within a single cycle — the bankroll, the survival ladder, `shutdown`,
/// the reconciler. Without this the split turned one read per cycle into five,
/// and worse, let `available` and equity come from *different* snapshots: a
/// fill between the two reads makes free cash exceed the cash inside the
/// equity figure, which one response made structurally impossible.
const DEFAULT_ACCOUNT_TTL: Duration = Duration::from_secs(2);
/// The asset universe changes on the order of days.
const DEFAULT_ASSETS_TTL: Duration = Duration::from_secs(3600);

/// Connection and universe settings for [`AlpacaVenue`].
///
/// Both hosts are fields rather than constants so tests can point them at a
/// mock server, and so paper/live is a configuration decision rather than a
/// recompile.
#[derive(Clone)]
pub struct AlpacaConfig {
    pub venue_id: VenueId,
    pub trading_base_url: String,
    pub data_base_url: String,
    key_id: String,
    secret_key: String,
    /// The symbol universe this venue trades: `AAPL`, `BTC/USD`, …
    pub symbols: Vec<String>,
    /// Market data feed for equities (`iex` or `sip`).
    pub feed: String,
    pub request_timeout: Duration,
    pub clock_ttl: Duration,
    pub assets_ttl: Duration,
    pub account_ttl: Duration,
}

/// Hand-written so credentials can never reach a log line or panic message.
impl std::fmt::Debug for AlpacaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlpacaConfig")
            .field("venue_id", &self.venue_id)
            .field("trading_base_url", &self.trading_base_url)
            .field("data_base_url", &self.data_base_url)
            .field("key_id", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field("symbols", &self.symbols)
            .field("feed", &self.feed)
            .finish_non_exhaustive()
    }
}

impl AlpacaConfig {
    fn base(
        trading_base_url: &str,
        key_id: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        Self {
            venue_id: VenueId::new(DEFAULT_VENUE_ID),
            trading_base_url: trading_base_url.to_string(),
            data_base_url: DATA_BASE_URL.to_string(),
            key_id: key_id.into(),
            secret_key: secret_key.into(),
            symbols: Vec::new(),
            feed: DEFAULT_FEED.to_string(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            clock_ttl: DEFAULT_CLOCK_TTL,
            assets_ttl: DEFAULT_ASSETS_TTL,
            account_ttl: DEFAULT_ACCOUNT_TTL,
        }
    }

    /// Paper trading against `paper-api.alpaca.markets`.
    pub fn paper(key_id: impl Into<String>, secret_key: impl Into<String>) -> Self {
        Self::base(PAPER_TRADING_BASE_URL, key_id, secret_key)
    }

    /// Live trading against `api.alpaca.markets`. Real money.
    pub fn live(key_id: impl Into<String>, secret_key: impl Into<String>) -> Self {
        Self::base(LIVE_TRADING_BASE_URL, key_id, secret_key)
    }

    /// Override both hosts — used by tests to aim at a mock server, and by
    /// anyone fronting Alpaca with a proxy.
    pub fn with_urls(mut self, trading: impl Into<String>, data: impl Into<String>) -> Self {
        self.trading_base_url = trading.into();
        self.data_base_url = data.into();
        self
    }

    pub fn with_symbols<I, S>(mut self, symbols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.symbols = symbols.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_venue_id(mut self, id: impl Into<String>) -> Self {
        self.venue_id = VenueId::new(id);
        self
    }

    pub fn with_feed(mut self, feed: impl Into<String>) -> Self {
        self.feed = feed.into();
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn with_cache_ttls(mut self, clock: Duration, assets: Duration) -> Self {
        self.clock_ttl = clock;
        self.assets_ttl = assets;
        self
    }
}

/// Symbol-keyed asset metadata for one Alpaca asset class.
type AssetListing = Arc<HashMap<String, AlpacaAsset>>;

/// A value with the instant it was fetched, for the per-cycle caches.
struct Cached<T> {
    value: T,
    fetched_at: Instant,
}

impl<T: Clone> Cached<T> {
    fn new(value: T) -> Self {
        Self {
            value,
            fetched_at: Instant::now(),
        }
    }

    fn get(&self, ttl: Duration) -> Option<T> {
        (self.fetched_at.elapsed() < ttl).then(|| self.value.clone())
    }
}

/// Alpaca as a [`Venue`].
pub struct AlpacaVenue {
    id: VenueId,
    rest: AlpacaRest,
    caps: VenueCapabilities,
    session: TradingSession,
    symbols: Vec<String>,
    feed: String,
    clock_ttl: Duration,
    assets_ttl: Duration,
    clock: RwLock<Option<Cached<AlpacaClock>>>,
    assets: RwLock<HashMap<&'static str, Cached<AssetListing>>>,
    account_ttl: Duration,
    account: RwLock<Option<Cached<AlpacaAccount>>>,
}

impl std::fmt::Debug for AlpacaVenue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlpacaVenue")
            .field("id", &self.id)
            .field("rest", &self.rest)
            .field("symbols", &self.symbols)
            .field("feed", &self.feed)
            .finish_non_exhaustive()
    }
}

impl AlpacaVenue {
    pub fn new(config: AlpacaConfig) -> Result<Self> {
        let rest = AlpacaRest::new(
            &config.trading_base_url,
            &config.data_base_url,
            &config.key_id,
            &config.secret_key,
            config.request_timeout,
        )?;

        let caps = capabilities_for(&config.symbols);
        // Equities carry the restrictive session, so a venue serving both asset
        // classes reports the equity windows. Per-asset-class scheduling is the
        // caller's job: it knows from `capabilities()` that `CryptoSpot` is in
        // the list, and crypto instruments remain tradeable 24/7 even while the
        // equity session says closed.
        let session = if caps.supports(AssetClass::Equity) {
            TradingSession::us_equity_extended()
        } else {
            TradingSession::Always
        };

        info!(
            venue = %config.venue_id,
            trading_base_url = %config.trading_base_url,
            data_base_url = %config.data_base_url,
            symbols = config.symbols.len(),
            feed = %config.feed,
            "Alpaca venue configured"
        );

        Ok(Self {
            id: config.venue_id,
            rest,
            caps,
            session,
            symbols: config.symbols,
            feed: config.feed,
            clock_ttl: config.clock_ttl,
            assets_ttl: config.assets_ttl,
            clock: RwLock::new(None),
            account_ttl: config.account_ttl,
            account: RwLock::new(None),
            assets: RwLock::new(HashMap::new()),
        })
    }

    /// The configured symbol universe.
    pub fn symbols(&self) -> &[String] {
        &self.symbols
    }

    // === Account ==========================================================

    /// Raw `/v2/account`.
    /// The account, reused for a moment.
    ///
    /// Cash and equity are two projections of one payload, so a shared
    /// snapshot keeps them consistent as well as cheap — see
    /// `DEFAULT_ACCOUNT_TTL`.
    pub async fn account(&self) -> Result<AlpacaAccount> {
        if let Some(cached) = self
            .account
            .read()
            .await
            .as_ref()
            .and_then(|c| c.get(self.account_ttl))
        {
            return Ok(cached);
        }

        let fresh: AlpacaAccount = self
            .rest
            .get(Api::Trading, "/v2/account", &[])
            .await
            .context("Failed to fetch the Alpaca account")?;
        *self.account.write().await = Some(Cached::new(fresh.clone()));
        Ok(fresh)
    }

    /// Pre-flight guard: fail loudly if the account cannot place orders.
    ///
    /// Alpaca keeps a live account in a state where market data still works but
    /// every order is rejected — `account_blocked` after a compliance hold,
    /// `trading_blocked` for a PDT violation, `trade_suspended_by_user` after a
    /// manual pause. Discovering that one order at a time wastes a cycle and
    /// fills the log with rejections, so callers check once up front.
    #[instrument(skip(self))]
    pub async fn assert_tradable(&self) -> Result<()> {
        let account = self.account().await?;
        if let Some(reason) = account.blocked_reason() {
            bail!("Alpaca account {} cannot trade: {reason}", account.id);
        }
        debug!(account = %account.id, "Alpaca account is clear to trade");
        Ok(())
    }

    // === Clock ============================================================

    /// `/v2/clock`, cached for `clock_ttl` so a cycle costs one request rather
    /// than one per instrument.
    pub async fn clock(&self) -> Result<AlpacaClock> {
        if let Some(cached) = self
            .clock
            .read()
            .await
            .as_ref()
            .and_then(|c| c.get(self.clock_ttl))
        {
            return Ok(cached);
        }

        let fresh: AlpacaClock = self
            .rest
            .get(Api::Trading, "/v2/clock", &[])
            .await
            .context("Failed to fetch the Alpaca market clock")?;
        *self.clock.write().await = Some(Cached::new(fresh.clone()));
        Ok(fresh)
    }

    /// Whether Alpaca will accept an order right now, cross-checking the static
    /// session windows against the venue clock.
    ///
    /// The static calendar knows nothing about holidays and early closes, so
    /// during the regular session the answer comes from `/v2/clock`. Outside it
    /// the clock is not usable: `is_open` describes the *regular* session only,
    /// and `next_open`/`next_close` cannot distinguish "Thursday evening's
    /// overnight window" from "the day before a holiday". Extended and
    /// overnight windows therefore fall back to the static calendar, which can
    /// report open on a holiday's pre-market. Alpaca rejects such an order, so
    /// the failure mode is a rejection rather than a bad fill.
    pub async fn accepts_orders_now_at(&self, at: DateTime<Utc>) -> Result<bool> {
        if !self.caps.supports(AssetClass::Equity) {
            // Crypto only: 24/7, nothing to check.
            return Ok(true);
        }
        match self.session.state_at(at) {
            SessionState::Closed { .. } => Ok(false),
            SessionState::Open(SessionKind::Regular) => Ok(self.clock().await?.is_open),
            SessionState::Open(_) => Ok(true),
        }
    }

    /// [`Self::accepts_orders_now_at`] at the current instant.
    pub async fn accepts_orders_now(&self) -> Result<bool> {
        self.accepts_orders_now_at(Utc::now()).await
    }

    /// Drop the clock and asset caches — call between cycles if a cycle can
    /// outlive the configured TTLs.
    pub async fn invalidate_caches(&self) {
        *self.clock.write().await = None;
        self.assets.write().await.clear();
    }

    // === Assets ===========================================================

    /// `/v2/assets` for one class, cached for `assets_ttl`.
    ///
    /// Alpaca has no symbol filter on this endpoint, so the class listing is
    /// fetched whole and cached; at ~11k equities that is one large response an
    /// hour rather than one request per symbol per cycle.
    async fn assets_for_class(&self, class: &'static str) -> Result<AssetListing> {
        if let Some(cached) = self
            .assets
            .read()
            .await
            .get(class)
            .and_then(|c| c.get(self.assets_ttl))
        {
            return Ok(cached);
        }

        let listing: Vec<AlpacaAsset> = self
            .rest
            .get(
                Api::Trading,
                "/v2/assets",
                &[
                    ("status", "active".to_string()),
                    ("asset_class", class.to_string()),
                ],
            )
            .await
            .with_context(|| format!("Failed to list Alpaca {class} assets"))?;

        let map: AssetListing =
            Arc::new(listing.into_iter().map(|a| (a.symbol.clone(), a)).collect());
        debug!(class, count = map.len(), "Cached Alpaca asset listing");
        self.assets
            .write()
            .await
            .insert(class, Cached::new(Arc::clone(&map)));
        Ok(map)
    }

    /// Asset metadata for exactly the symbols asked for, fetching only the
    /// classes those symbols belong to.
    async fn assets_for(&self, symbols: &[String]) -> Result<HashMap<String, AlpacaAsset>> {
        let wants_equity = symbols.iter().any(|s| !is_crypto_symbol(s));
        let wants_crypto = symbols.iter().any(|s| is_crypto_symbol(s));

        let mut out = HashMap::new();
        for (wanted, class) in [
            (wants_equity, ASSET_CLASS_EQUITY),
            (wants_crypto, ASSET_CLASS_CRYPTO),
        ] {
            if !wanted {
                continue;
            }
            let listing = self.assets_for_class(class).await?;
            for symbol in symbols {
                if let Some(asset) = listing.get(symbol) {
                    out.insert(symbol.clone(), asset.clone());
                }
            }
        }
        Ok(out)
    }

    fn instrument_from_asset(&self, symbol: &str, asset: &AlpacaAsset) -> Instrument {
        let id = InstrumentId::new(self.id.clone(), symbol);
        let display_name = asset.name.clone().unwrap_or_else(|| symbol.to_string());

        if is_crypto_symbol(symbol) {
            let (base, quote_ccy) = split_pair(symbol).unwrap_or((symbol, "USD"));
            Instrument {
                id,
                asset_class: AssetClass::CryptoSpot,
                display_name,
                quote_ccy: quote_ccy.to_string(),
                tick_size: asset.price_increment,
                lot_size: asset.min_trade_increment,
                // Alpaca gates crypto on a minimum *quantity* (`min_order_size`,
                // e.g. 0.000026 BTC) rather than on notional value, which is
                // why the constraint lives in `min_qty`: stating it as a
                // notional would need a price discovery does not have.
                min_notional: None,
                min_qty: asset.min_order_size,
                fractional: true,
                meta: InstrumentMeta::Spot {
                    base: base.to_string(),
                },
            }
        } else {
            Instrument {
                id,
                asset_class: AssetClass::Equity,
                display_name,
                quote_ccy: "USD".to_string(),
                tick_size: Some(EQUITY_TICK_SIZE),
                // Whole shares only unless Alpaca marks the name fractionable.
                lot_size: if asset.fractionable {
                    None
                } else {
                    Some(Decimal::ONE)
                },
                min_notional: Some(EQUITY_MIN_NOTIONAL),
                min_qty: None,
                fractional: asset.fractionable,
                meta: InstrumentMeta::Equity {
                    exchange: asset.exchange.clone(),
                },
            }
        }
    }

    /// Alpaca reports crypto *positions* as `BTCUSD` while every other
    /// endpoint spells the pair `BTC/USD`. Recover the slash so a position
    /// reconciles against the instrument the order was placed for.
    fn normalise_position_symbol(&self, raw: &str, asset_class: &str) -> String {
        if raw.contains('/') || !asset_class.eq_ignore_ascii_case(ASSET_CLASS_CRYPTO) {
            return raw.to_string();
        }
        if let Some(configured) = self
            .symbols
            .iter()
            .find(|s| s.replace('/', "").eq_ignore_ascii_case(raw))
        {
            return configured.clone();
        }
        // Longest quote currencies first, so BTCUSDT does not split on USD.
        for quote in ["USDT", "USDC", "USD", "BTC", "ETH"] {
            if raw.len() > quote.len() && raw.ends_with(quote) {
                return format!("{}/{}", &raw[..raw.len() - quote.len()], quote);
            }
        }
        warn!(
            symbol = raw,
            "Could not recover the pair separator for an Alpaca crypto position — \
             reporting the symbol as given"
        );
        raw.to_string()
    }
}

/// Which asset classes this venue serves, derived from the symbol universe. An
/// empty universe means "whatever the account supports", which is both.
fn capabilities_for(symbols: &[String]) -> VenueCapabilities {
    let has_equity = symbols.is_empty() || symbols.iter().any(|s| !is_crypto_symbol(s));
    let has_crypto = symbols.is_empty() || symbols.iter().any(|s| is_crypto_symbol(s));

    let mut asset_classes = Vec::new();
    if has_equity {
        asset_classes.push(AssetClass::Equity);
    }
    if has_crypto {
        asset_classes.push(AssetClass::CryptoSpot);
    }

    VenueCapabilities {
        asset_classes,
        // Only equities are session-bound; crypto takes market orders at 3am.
        limit_only_outside_regular: has_equity,
        supports_client_order_id: true,
        supports_candles: true,
    }
}

/// Build the `POST /v2/orders` body, refusing anything Alpaca will bounce.
///
/// Validating here rather than letting Alpaca reject keeps the failure in the
/// caller's error path with a message that says which rule was broken.
fn build_new_order(request: &OrderRequest) -> Result<NewOrder> {
    let symbol = request.instrument.symbol().to_string();

    if request.client_order_id.trim().is_empty() {
        bail!("Alpaca orders require a client_order_id — without one a retry after a timeout can double-place {symbol}");
    }
    if request.qty <= Decimal::ZERO {
        bail!(
            "Alpaca order quantity must be positive, got {} for {symbol}",
            request.qty
        );
    }

    let crypto = match request.instrument.asset_class {
        AssetClass::CryptoSpot => true,
        AssetClass::Equity => false,
        AssetClass::PredictionBinary => {
            bail!("Alpaca does not list prediction markets (asked for {symbol})")
        }
    };

    let tif = tif_str(request.tif).with_context(|| {
        format!(
            "Alpaca has no equivalent for {:?}; it supports day, gtc, opg, cls, ioc and fok only",
            request.tif
        )
    })?;

    // Apply the instrument's own constraints before Alpaca does. This module's
    // header promises violations are refused here rather than bounced after the
    // fact, and `round_qty`/`round_price` existed to do it — nothing called
    // them, so `lot_size`, `tick_size` and `fractional` were decorative and
    // every rejection came back from the venue instead.
    let qty = {
        let lotted = request.instrument.round_qty(request.qty);
        // A non-fractionable name takes whole shares only. Rounding down is
        // the safe direction: up would spend more than was sized for.
        let whole = if request.instrument.fractional {
            lotted
        } else {
            lotted.floor()
        };
        if whole <= Decimal::ZERO {
            bail!(
                "Quantity {} for {symbol} rounds to zero against the venue's lot size and                  whole-share rule — the order would only be rejected",
                request.qty
            );
        }
        // Alpaca gates crypto on a minimum quantity, and rounding down to the
        // lot size can cross it. Checked after rounding rather than before,
        // because the size that gets submitted is the one that has to clear.
        if !request.instrument.meets_min_qty(whole) {
            bail!(
                "Quantity {whole} for {symbol} is below the venue's minimum order size of {}",
                request
                    .instrument
                    .min_qty
                    .unwrap_or(Decimal::ZERO)
                    .normalize()
            );
        }
        whole
    };

    let (order_type, limit_price) = match request.kind {
        OrderKind::Limit { price } => {
            if price <= Decimal::ZERO {
                bail!("Alpaca limit price must be positive, got {price} for {symbol}");
            }
            let ticked = request.instrument.round_price(price, request.side);
            if ticked <= Decimal::ZERO {
                bail!(
                    "Limit price {price} for {symbol} rounds to zero against the venue's                      tick size"
                );
            }
            ("limit", Some(ticked.normalize().to_string()))
        }
        OrderKind::Market => ("market", None),
    };

    let extended_hours = if crypto {
        if request.extended_hours {
            warn!(
                symbol = %symbol,
                "extended_hours requested on a crypto order — Alpaca crypto trades 24/7 and \
                 rejects the flag, so it is being dropped"
            );
        }
        false
    } else {
        request.extended_hours
    };

    if crypto {
        if !matches!(request.tif, TimeInForce::Gtc | TimeInForce::Ioc) {
            bail!(
                "Alpaca crypto orders accept time_in_force gtc or ioc only, got {tif} for {symbol}"
            );
        }
        if let Some(notional) = request.notional() {
            if notional > CRYPTO_MAX_NOTIONAL {
                bail!(
                    "Alpaca caps a single crypto order at {CRYPTO_MAX_NOTIONAL} notional; \
                     {symbol} was sized at {notional}"
                );
            }
        }
    } else if extended_hours {
        if order_type != "limit" {
            bail!("Alpaca accepts limit orders only outside regular hours; {symbol} was submitted as a market order");
        }
        if !matches!(request.tif, TimeInForce::Day | TimeInForce::Gtc) {
            bail!("Alpaca extended-hours orders require time_in_force day or gtc, got {tif} for {symbol}");
        }
    }

    Ok(NewOrder {
        symbol,
        qty: qty.normalize().to_string(),
        side: side_str(request.side),
        order_type,
        time_in_force: tif,
        limit_price,
        extended_hours,
        client_order_id: request.client_order_id.clone(),
    })
}

/// Alpaca's order object as an [`OrderAck`].
///
/// `fees` is always zero: equities are commission-free and Alpaca folds the
/// crypto spread fee into `filled_avg_price` rather than reporting it on the
/// order, so there is no per-order fee figure to read. Cost basis derived from
/// `avg_fill_price` is therefore already fee-inclusive for crypto.
fn to_ack(order: &AlpacaOrder) -> OrderAck {
    let reason = (order.status == "rejected")
        .then(|| format!("Alpaca rejected order {} for {}", order.id, order.symbol));
    OrderAck {
        venue_order_id: order.id.clone(),
        client_order_id: order.client_order_id.clone(),
        state: order_state(&order.status, reason.as_deref()),
        filled_qty: order.filled_qty.unwrap_or(Decimal::ZERO),
        avg_fill_price: order.filled_avg_price,
        fees: Decimal::ZERO,
    }
}

/// Refuse to synthesise a mid from half a book.
fn require_two_sided(symbol: &str, bid: Decimal, ask: Decimal) -> Result<()> {
    if bid <= Decimal::ZERO || ask <= Decimal::ZERO {
        bail!("Alpaca returned a one-sided quote for {symbol} (bid {bid}, ask {ask})");
    }
    if ask < bid {
        bail!("Alpaca returned a crossed quote for {symbol}: bid {bid} above ask {ask}");
    }
    Ok(())
}

fn timeframe_for(interval: CandleInterval) -> &'static str {
    match interval {
        CandleInterval::H1 => "1Hour",
        CandleInterval::H4 => "4Hour",
        CandleInterval::D1 => "1Day",
    }
}

#[async_trait]
impl Venue for AlpacaVenue {
    fn id(&self) -> &VenueId {
        &self.id
    }

    fn capabilities(&self) -> &VenueCapabilities {
        &self.caps
    }

    fn session(&self) -> &TradingSession {
        &self.session
    }

    /// Instruments for the requested universe.
    ///
    /// `ScanFilter::min_volume_24h` and `max_days_to_resolution` are ignored:
    /// `/v2/assets` publishes neither, and equities never resolve. Filtering on
    /// volume is the caller's job once it has quotes or bars.
    #[instrument(skip(self, filter), fields(venue = %self.id))]
    async fn list_instruments(&self, filter: &ScanFilter) -> Result<Vec<Instrument>> {
        let requested: &[String] = if filter.symbols.is_empty() {
            &self.symbols
        } else {
            &filter.symbols
        };

        let mut seen = HashSet::new();
        let wanted: Vec<String> = requested
            .iter()
            .filter(|symbol| {
                if filter.asset_classes.is_empty() {
                    return true;
                }
                let class = if is_crypto_symbol(symbol) {
                    AssetClass::CryptoSpot
                } else {
                    AssetClass::Equity
                };
                filter.asset_classes.contains(&class)
            })
            .filter(|symbol| seen.insert(symbol.as_str()))
            .cloned()
            .collect();

        if wanted.is_empty() {
            return Ok(Vec::new());
        }

        let assets = self.assets_for(&wanted).await?;
        let mut instruments = Vec::with_capacity(wanted.len());
        for symbol in &wanted {
            match assets.get(symbol) {
                Some(asset) if asset.tradable => {
                    instruments.push(self.instrument_from_asset(symbol, asset));
                }
                Some(_) => {
                    warn!(symbol = %symbol, "Alpaca lists the asset but it is not tradable — skipping")
                }
                None => {
                    warn!(symbol = %symbol, "Alpaca does not list this symbol as an active asset — skipping")
                }
            }
        }

        if let Some(max) = filter.max_results {
            instruments.truncate(max);
        }
        Ok(instruments)
    }

    /// Top of book.
    ///
    /// Crypto comes from `/v1beta3/crypto/us/latest/orderbooks`, which carries
    /// real depth. Equities come from `/v2/stocks/quotes/latest`, which is
    /// top-of-book only, so `Quote::book` is `None` there rather than a
    /// one-level book: the endpoint's `bs`/`as` sizes are round lots on the SIP
    /// feed and share counts on IEX, and publishing a size whose unit depends
    /// on the subscription would let a liquidity check be wrong by 100x.
    #[instrument(skip(self), fields(venue = %self.id, symbol = %id.symbol))]
    async fn quote(&self, id: &InstrumentId) -> Result<Quote> {
        let symbol = id.symbol.clone();

        if is_crypto_symbol(&symbol) {
            let response: CryptoOrderbooksResponse = self
                .rest
                .get(
                    Api::Data,
                    "/v1beta3/crypto/us/latest/orderbooks",
                    &[("symbols", symbol.clone())],
                )
                .await
                .with_context(|| format!("Failed to fetch the Alpaca orderbook for {symbol}"))?;

            let book = response
                .orderbooks
                .get(&symbol)
                .with_context(|| format!("Alpaca returned no orderbook for {symbol}"))?;
            let best_bid = book
                .bids
                .first()
                .with_context(|| format!("Alpaca orderbook for {symbol} has no bids"))?;
            let best_ask = book
                .asks
                .first()
                .with_context(|| format!("Alpaca orderbook for {symbol} has no asks"))?;

            let (bid, ask) = (best_bid.price, best_ask.price);
            require_two_sided(&symbol, bid, ask)?;
            let mid = (bid + ask) / dec!(2);

            let snapshot = OrderBookSnapshot {
                token_id: symbol.clone(),
                bids: book
                    .bids
                    .iter()
                    .map(|l| PriceLevel {
                        price: l.price,
                        size: l.size,
                    })
                    .collect(),
                asks: book
                    .asks
                    .iter()
                    .map(|l| PriceLevel {
                        price: l.price,
                        size: l.size,
                    })
                    .collect(),
                spread: ask - bid,
                midpoint: mid,
                // `OrderBookSnapshot` is still shaped for prediction markets.
                // An implied probability is meaningless for a spot pair, so it
                // is zeroed rather than filled with a mid that a caller might
                // read as a probability.
                implied_probability: Decimal::ZERO,
                timestamp: book.ts,
            };

            return Ok(Quote {
                instrument: id.clone(),
                bid,
                ask,
                mid,
                last: None,
                ts: book.ts,
                book: Some(snapshot),
            });
        }

        let response: LatestQuotesResponse = self
            .rest
            .get(
                Api::Data,
                "/v2/stocks/quotes/latest",
                &[("symbols", symbol.clone()), ("feed", self.feed.clone())],
            )
            .await
            .with_context(|| format!("Failed to fetch the Alpaca quote for {symbol}"))?;

        let quote = response
            .quotes
            .get(&symbol)
            .with_context(|| format!("Alpaca returned no quote for {symbol}"))?;
        let (bid, ask) = (quote.bid_price, quote.ask_price);
        require_two_sided(&symbol, bid, ask)?;

        Ok(Quote {
            instrument: id.clone(),
            bid,
            ask,
            mid: (bid + ask) / dec!(2),
            last: None,
            ts: quote.ts,
            book: None,
        })
    }

    #[instrument(skip(self), fields(venue = %self.id, symbol = %id.symbol))]
    async fn candles(
        &self,
        id: &InstrumentId,
        interval: CandleInterval,
        limit: usize,
    ) -> Result<Vec<Candle>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = limit.min(MAX_BARS);
        let symbol = id.symbol.clone();
        let crypto = is_crypto_symbol(&symbol);

        // Three times the nominal window, so weekends and holidays cannot
        // starve an equity request of bars.
        let lookback = ChronoDuration::hours(interval.hours() * limit as i64 * 3);
        let mut query = vec![
            ("symbols", symbol.clone()),
            ("timeframe", timeframe_for(interval).to_string()),
            ("limit", limit.to_string()),
            ("start", (Utc::now() - lookback).to_rfc3339()),
            // Newest first, so `limit` selects the most recent bars; reversed
            // below into the chronological order callers expect.
            ("sort", "desc".to_string()),
        ];

        let path = if crypto {
            "/v1beta3/crypto/us/bars"
        } else {
            query.push(("feed", self.feed.clone()));
            "/v2/stocks/bars"
        };

        let response: BarsResponse = self
            .rest
            .get(Api::Data, path, &query)
            .await
            .with_context(|| format!("Failed to fetch Alpaca bars for {symbol}"))?;

        let Some(bars) = response.bars.get(&symbol) else {
            debug!(symbol = %symbol, "Alpaca returned no bars for this symbol");
            return Ok(Vec::new());
        };

        let mut candles: Vec<Candle> = bars
            .iter()
            .map(|b| Candle {
                ts: b.ts,
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                volume: b.volume.unwrap_or(Decimal::ZERO),
            })
            .collect();
        candles.reverse();
        Ok(candles)
    }

    #[instrument(
        skip(self, request),
        fields(
            venue = %self.id,
            symbol = %request.instrument.symbol(),
            side = %request.side,
            qty = %request.qty,
            client_order_id = %request.client_order_id,
        )
    )]
    async fn place_order(&self, request: &OrderRequest) -> Result<OrderAck> {
        let payload = build_new_order(request)?;

        if !payload.extended_hours
            && request.instrument.asset_class == AssetClass::Equity
            && !matches!(
                self.session.state_at(Utc::now()),
                SessionState::Open(SessionKind::Regular)
            )
        {
            warn!(
                symbol = %payload.symbol,
                "Equity order placed outside regular hours without extended_hours — \
                 Alpaca will queue it until the next regular session"
            );
        }

        let order: AlpacaOrder = self
            .rest
            .post(Api::Trading, "/v2/orders", &payload)
            .await
            .with_context(|| {
                format!(
                    "Failed to place Alpaca order for {} (client_order_id {})",
                    payload.symbol, payload.client_order_id
                )
            })?;

        let ack = to_ack(&order);
        info!(
            venue_order_id = %ack.venue_order_id,
            state = ?ack.state,
            "Alpaca accepted the order"
        );
        Ok(ack)
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn get_order(&self, order: &OrderRef) -> Result<OrderAck> {
        let fetched: AlpacaOrder = match order {
            OrderRef::Venue(id) => {
                let path = format!("/v2/orders/{}", urlencoding::encode(id));
                self.rest
                    .get(Api::Trading, &path, &[])
                    .await
                    .with_context(|| format!("Failed to fetch Alpaca order {id}"))?
            }
            OrderRef::Client(client_order_id) => self
                .rest
                .get(
                    Api::Trading,
                    "/v2/orders:by_client_order_id",
                    &[("client_order_id", client_order_id.clone())],
                )
                .await
                .with_context(|| {
                    format!("Failed to fetch Alpaca order by client_order_id {client_order_id}")
                })?,
        };
        Ok(to_ack(&fetched))
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn cancel_order(&self, venue_order_id: &str) -> Result<()> {
        let path = format!("/v2/orders/{}", urlencoding::encode(venue_order_id));
        self.rest
            .delete_raw(Api::Trading, &path, &[])
            .await
            .with_context(|| format!("Failed to cancel Alpaca order {venue_order_id}"))?;
        info!(venue_order_id, "Cancelled Alpaca order");
        Ok(())
    }

    /// Kill switch. `DELETE /v2/orders` answers `207 Multi-Status` with a
    /// per-order result, so a 2xx on the envelope is not enough — an order that
    /// failed to cancel is reported, not swallowed.
    #[instrument(skip(self), fields(venue = %self.id))]
    async fn cancel_all(&self) -> Result<()> {
        let body = self
            .rest
            .delete_raw(Api::Trading, "/v2/orders", &[])
            .await
            .context("Failed to cancel all Alpaca orders")?;

        if body.trim().is_empty() {
            return Ok(());
        }

        let entries: Vec<CancelAllEntry> = serde_json::from_str(&body)
            .with_context(|| format!("Alpaca cancel-all returned an unexpected body: {body}"))?;

        let mut failed = Vec::new();
        for entry in &entries {
            match entry.status {
                200..=299 => {}
                // 404: already gone. 422: no longer cancelable (filled or
                // already done). Neither leaves a live order behind.
                404 | 422 => warn!(
                    order_id = %entry.id,
                    status = entry.status,
                    "Order was no longer cancelable — treating it as already resolved"
                ),
                other => failed.push(format!("{} (HTTP {other})", entry.id)),
            }
        }

        if !failed.is_empty() {
            bail!(
                "Alpaca could not cancel {} order(s): {}",
                failed.len(),
                failed.join(", ")
            );
        }

        info!(orders = entries.len(), "Cancelled all open Alpaca orders");
        Ok(())
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn open_orders(&self) -> Result<Vec<OrderAck>> {
        // Paged, because /v2/orders caps `limit` at 500 and returning the
        // first page as if it were the whole answer is the failure this
        // adapter exists to avoid: reconciliation and the kill switch both
        // conclude that an order they cannot see does not exist, and stop
        // tracking it. Alpaca pages by submission time, not offset.
        let mut acks = Vec::new();
        let mut after: Option<DateTime<Utc>> = None;

        loop {
            let mut params = vec![
                ("status", "open".to_string()),
                ("limit", ORDER_PAGE_SIZE.to_string()),
                ("nested", "false".to_string()),
                ("direction", "asc".to_string()),
            ];
            if let Some(cursor) = after {
                params.push(("after", cursor.to_rfc3339()));
            }

            let page: Vec<AlpacaOrder> = self
                .rest
                .get(Api::Trading, "/v2/orders", &params)
                .await
                .context("Failed to list open Alpaca orders")?;

            let full_page = page.len() >= ORDER_PAGE_SIZE;
            let newest = page.iter().filter_map(|o| o.submitted_at).max();
            acks.extend(page.iter().map(to_ack));

            if !full_page {
                break;
            }
            // Without a cursor that advances, the next request returns the
            // same page forever. A full page whose orders carry no usable
            // timestamp is reported rather than looped on, because silently
            // truncating here is the very thing being fixed.
            match newest {
                Some(ts) if Some(ts) != after => after = Some(ts),
                _ => {
                    warn!(
                        venue = %self.id,
                        orders = acks.len(),
                        "Open-order pagination cannot advance — the list may be incomplete"
                    );
                    break;
                }
            }
        }

        Ok(acks)
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn positions(&self) -> Result<Vec<Position>> {
        let positions: Vec<AlpacaPosition> = self
            .rest
            .get(Api::Trading, "/v2/positions", &[])
            .await
            .context("Failed to fetch Alpaca positions")?;

        Ok(positions
            .iter()
            .map(|p| Position {
                instrument: InstrumentId::new(
                    self.id.clone(),
                    self.normalise_position_symbol(&p.symbol, &p.asset_class),
                ),
                qty: p.qty,
                avg_entry: p.avg_entry_price,
            })
            .collect())
    }

    /// Cash available to deploy. Account value is `equity`, which reads the
    /// same cached snapshot.
    ///
    /// `available` deliberately uses `non_marginable_buying_power` (cash that
    /// can buy crypto and fractional shares) rather than `buying_power`, which
    /// on a margin account is a multiple of cash. An agent sizing against
    /// leverage it did not ask for is a solvency bug, not a feature.
    /// `assert_tradable` reached through the trait.
    ///
    /// It was written, tested and never called by anything — so a
    /// `trading_blocked` account passed every startup check and then had
    /// every order rejected.
    async fn trading_readiness(&self) -> Result<()> {
        self.assert_tradable().await
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn balance(&self) -> Result<Balance> {
        let account = self.account().await?;
        let available = account
            .non_marginable_buying_power
            .unwrap_or(account.cash)
            .max(Decimal::ZERO);
        Ok(Balance {
            ccy: account.currency,
            available,
        })
    }

    /// Alpaca reports account equity directly, positions included — no
    /// per-holding quote, and off the same cached snapshot as `balance`.
    #[instrument(skip(self), fields(venue = %self.id))]
    async fn equity(&self) -> Result<Option<Decimal>> {
        Ok(Some(self.account().await?.equity))
    }

    /// Neither equities nor crypto spot settle — a position is closed by
    /// trading out of it, so there is never a settlement to report.
    async fn settlement(&self, _id: &InstrumentId) -> Result<Option<Settlement>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::types::{OrderState, Side};
    use chrono::TimeZone;
    use serde_json::{json, Value};
    use wiremock::matchers::{body_partial_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const KEY: &str = "test-key-id";
    const SECRET: &str = "test-secret-key";

    fn venue(server: &MockServer, symbols: &[&str]) -> AlpacaVenue {
        AlpacaVenue::new(
            AlpacaConfig::paper(KEY, SECRET)
                .with_urls(server.uri(), server.uri())
                .with_symbols(symbols.iter().map(|s| s.to_string())),
        )
        .expect("venue builds")
    }

    fn equity_instrument() -> Instrument {
        Instrument {
            id: InstrumentId::new(VenueId::new(DEFAULT_VENUE_ID), "AAPL"),
            asset_class: AssetClass::Equity,
            display_name: "Apple Inc. Common Stock".to_string(),
            quote_ccy: "USD".to_string(),
            tick_size: Some(EQUITY_TICK_SIZE),
            lot_size: None,
            min_notional: Some(EQUITY_MIN_NOTIONAL),
            min_qty: None,
            fractional: true,
            meta: InstrumentMeta::Equity {
                exchange: "NASDAQ".to_string(),
            },
        }
    }

    fn crypto_instrument() -> Instrument {
        Instrument {
            id: InstrumentId::new(VenueId::new(DEFAULT_VENUE_ID), "BTC/USD"),
            asset_class: AssetClass::CryptoSpot,
            display_name: "Bitcoin / US Dollar".to_string(),
            quote_ccy: "USD".to_string(),
            tick_size: Some(dec!(1)),
            lot_size: Some(dec!(0.000000001)),
            min_notional: None,
            min_qty: None,
            fractional: true,
            meta: InstrumentMeta::Spot {
                base: "BTC".to_string(),
            },
        }
    }

    fn order(instrument: Instrument, kind: OrderKind, tif: TimeInForce, ext: bool) -> OrderRequest {
        OrderRequest {
            instrument,
            side: Side::Buy,
            kind,
            qty: dec!(2.5),
            tif,
            extended_hours: ext,
            client_order_id: "cid-abc-123".to_string(),
        }
    }

    /// Finding 6: `lot_size`, `tick_size` and `fractional` were carried on the
    /// instrument and never applied, so `round_qty`/`round_price` were dead
    /// code and Alpaca bounced orders this module claims to validate locally.
    #[test]
    fn a_non_fractionable_name_is_sized_in_whole_shares() {
        let mut inst = equity_instrument();
        inst.fractional = false;

        let built = build_new_order(&order(
            inst,
            OrderKind::Limit {
                price: dec!(191.50),
            },
            TimeInForce::Day,
            false,
        ))
        .expect("2.5 shares floors to 2");

        assert_eq!(built.qty, "2");
    }

    #[test]
    fn a_fractionable_name_keeps_its_fraction() {
        let built = build_new_order(&order(
            equity_instrument(),
            OrderKind::Limit {
                price: dec!(191.50),
            },
            TimeInForce::Day,
            false,
        ))
        .expect("fractional equities take 2.5");

        assert_eq!(built.qty, "2.5");
    }

    #[test]
    fn quantity_is_floored_to_the_lot_size() {
        let mut inst = crypto_instrument();
        inst.lot_size = Some(dec!(0.001));
        let mut req = order(
            inst,
            OrderKind::Limit { price: dec!(64000) },
            TimeInForce::Gtc,
            false,
        );
        req.qty = dec!(1.23456);

        let built = build_new_order(&req).expect("floors to the lot");
        assert_eq!(built.qty, "1.234");
    }

    /// Rounding down can reach zero, and a zero-quantity order is a guaranteed
    /// rejection. Refusing locally says why; the venue's error would not.
    #[test]
    fn a_quantity_that_rounds_away_is_refused_locally() {
        let mut inst = equity_instrument();
        inst.fractional = false;
        let mut req = order(
            inst,
            OrderKind::Limit {
                price: dec!(191.50),
            },
            TimeInForce::Day,
            false,
        );
        req.qty = dec!(0.4);

        let err = build_new_order(&req).expect_err("0.4 whole shares is no shares");
        assert!(err.to_string().contains("rounds to zero"), "{err}");
    }

    /// Finding 12: Alpaca publishes `min_order_size` for crypto and it was
    /// parsed and thrown away, so a sub-minimum size was only refused by the
    /// venue at submission. At micro capital this is where it bites — a few
    /// dollars of BTC sits near the floor.
    #[test]
    fn a_crypto_order_below_the_venue_minimum_is_refused_locally() {
        let mut inst = crypto_instrument();
        inst.min_qty = Some(dec!(0.000026));
        inst.lot_size = Some(dec!(0.000000001));

        let mut req = order(
            inst,
            OrderKind::Limit { price: dec!(64000) },
            TimeInForce::Gtc,
            false,
        );
        req.qty = dec!(0.00001);

        let err = build_new_order(&req).expect_err("below Alpaca's minimum");
        assert!(err.to_string().contains("minimum order size"), "{err}");
    }

    #[test]
    fn a_crypto_order_at_the_venue_minimum_is_accepted() {
        let mut inst = crypto_instrument();
        inst.min_qty = Some(dec!(0.000026));
        inst.lot_size = Some(dec!(0.000001));

        let mut req = order(
            inst,
            OrderKind::Limit { price: dec!(64000) },
            TimeInForce::Gtc,
            false,
        );
        req.qty = dec!(0.000026);

        let built = build_new_order(&req).expect("exactly at the minimum clears");
        assert_eq!(built.qty, "0.000026");
    }

    /// The ordering matters: rounding down to the lot size can push a size
    /// that cleared the minimum below it, so the check has to come after the
    /// rounding — the submitted size is the one that has to clear.
    #[test]
    fn rounding_down_to_the_lot_can_cross_the_minimum_and_is_caught() {
        let mut inst = crypto_instrument();
        inst.lot_size = Some(dec!(0.01));
        inst.min_qty = Some(dec!(0.015));

        let mut req = order(
            inst,
            OrderKind::Limit { price: dec!(64000) },
            TimeInForce::Gtc,
            false,
        );
        // 0.019 clears the 0.015 minimum, but floors to 0.01, which does not.
        req.qty = dec!(0.019);

        let err = build_new_order(&req).expect_err("the rounded size is below the minimum");
        assert!(err.to_string().contains("minimum order size"), "{err}");
    }

    #[test]
    fn a_limit_price_is_rounded_to_the_tick_toward_the_safe_side() {
        let mut buy = order(
            equity_instrument(),
            OrderKind::Limit {
                price: dec!(191.5049),
            },
            TimeInForce::Day,
            false,
        );
        buy.side = Side::Buy;
        // Buying: never round up into paying more than was sized for.
        assert_eq!(build_new_order(&buy).unwrap().limit_price.unwrap(), "191.5");

        let mut sell = buy.clone();
        sell.side = Side::Sell;
        // Selling: never round down into receiving less.
        assert_eq!(
            build_new_order(&sell).unwrap().limit_price.unwrap(),
            "191.51"
        );
    }

    fn order_response(status: &str) -> Value {
        json!({
            "id": "904837e3-3b76-47ec-b432-046db621571b",
            "client_order_id": "cid-abc-123",
            "symbol": "AAPL",
            "status": status,
            "qty": "2.5",
            "filled_qty": "1.5",
            "filled_avg_price": "191.365",
            "limit_price": "191.50",
            "side": "buy",
            "type": "limit",
            "time_in_force": "day",
            "extended_hours": true
        })
    }

    /// The exact JSON body of the last request wiremock recorded.
    async fn last_body(server: &MockServer) -> Value {
        let requests = server
            .received_requests()
            .await
            .expect("wiremock records requests");
        let last = requests.last().expect("at least one request");
        serde_json::from_slice(&last.body).expect("request body is JSON")
    }

    #[tokio::test]
    async fn place_order_sends_the_exact_alpaca_body_including_client_order_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/orders"))
            .and(header("APCA-API-KEY-ID", KEY))
            .and(header("APCA-API-SECRET-KEY", SECRET))
            // Assert the real body. Every field here is one Alpaca rejects or
            // misinterprets if it is wrong: a numeric qty loses scale, a
            // missing client_order_id makes a retry double-place, and
            // extended_hours decides whether the order trades at all at 18:00.
            .and(body_partial_json(json!({
                "symbol": "AAPL",
                "qty": "2.5",
                "side": "buy",
                "type": "limit",
                "time_in_force": "day",
                "limit_price": "191.5",
                "extended_hours": true,
                "client_order_id": "cid-abc-123"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(order_response("accepted")))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let ack = venue
            .place_order(&order(
                equity_instrument(),
                OrderKind::Limit {
                    price: dec!(191.50),
                },
                TimeInForce::Day,
                true,
            ))
            .await
            .unwrap();

        assert_eq!(ack.client_order_id, "cid-abc-123");
        assert_eq!(ack.state, OrderState::Accepted);
        assert_eq!(ack.filled_qty, dec!(1.5));
        assert_eq!(ack.avg_fill_price, Some(dec!(191.365)));

        // The idempotency key must be on the wire verbatim, not regenerated.
        let body = last_body(&server).await;
        assert_eq!(body["client_order_id"], json!("cid-abc-123"));
    }

    #[tokio::test]
    async fn market_order_omits_limit_price_entirely() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/orders"))
            .and(body_partial_json(json!({"type": "market", "qty": "2.5"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(order_response("new")))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        venue
            .place_order(&order(
                equity_instrument(),
                OrderKind::Market,
                TimeInForce::Day,
                false,
            ))
            .await
            .unwrap();

        // A null limit_price is rejected by Alpaca on a market order, so the
        // key must be absent rather than present-and-null.
        let body = last_body(&server).await;
        assert!(body.get("limit_price").is_none(), "body was {body}");
        assert_eq!(body["extended_hours"], json!(false));
    }

    #[tokio::test]
    async fn extended_hours_rejects_market_orders_and_bad_time_in_force() {
        let server = MockServer::start().await;
        let venue = venue(&server, &["AAPL"]);

        let err = venue
            .place_order(&order(
                equity_instrument(),
                OrderKind::Market,
                TimeInForce::Day,
                true,
            ))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("limit orders only"), "{err}");

        let err = venue
            .place_order(&order(
                equity_instrument(),
                OrderKind::Limit { price: dec!(191.5) },
                TimeInForce::Ioc,
                true,
            ))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("day or gtc"), "{err}");

        // Nothing should have reached the network.
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn crypto_orders_enforce_tif_and_the_notional_cap() {
        let server = MockServer::start().await;
        let venue = venue(&server, &["BTC/USD"]);

        let err = venue
            .place_order(&order(
                crypto_instrument(),
                OrderKind::Limit { price: dec!(64000) },
                TimeInForce::Day,
                false,
            ))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("gtc or ioc"), "{err}");

        // 2.5 BTC at $100,000 is $250,000 — over Alpaca's $200k per-order cap.
        let mut big = order(
            crypto_instrument(),
            OrderKind::Limit {
                price: dec!(100000),
            },
            TimeInForce::Gtc,
            false,
        );
        big.qty = dec!(2.5);
        let err = venue.place_order(&big).await.unwrap_err();
        assert!(err.to_string().contains("200000"), "{err}");
    }

    #[tokio::test]
    async fn gtd_is_refused_rather_than_silently_downgraded() {
        let server = MockServer::start().await;
        let venue = venue(&server, &["AAPL"]);
        let expiry = Utc.with_ymd_and_hms(2026, 12, 31, 20, 0, 0).unwrap();
        let err = venue
            .place_order(&order(
                equity_instrument(),
                OrderKind::Limit { price: dec!(191.5) },
                TimeInForce::Gtd(expiry),
                false,
            ))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no equivalent"), "{err}");
    }

    #[tokio::test]
    async fn get_order_by_client_id_uses_the_by_client_order_id_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/orders:by_client_order_id"))
            .and(query_param("client_order_id", "cid-abc-123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(order_response("filled")))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let ack = venue
            .get_order(&OrderRef::Client("cid-abc-123".to_string()))
            .await
            .unwrap();
        assert_eq!(ack.state, OrderState::Filled);
        assert_eq!(ack.venue_order_id, "904837e3-3b76-47ec-b432-046db621571b");
    }

    #[tokio::test]
    async fn get_order_by_venue_id_uses_the_path_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/orders/904837e3-3b76-47ec-b432-046db621571b"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(order_response("partially_filled")),
            )
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let ack = venue
            .get_order(&OrderRef::Venue(
                "904837e3-3b76-47ec-b432-046db621571b".to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(ack.state, OrderState::PartiallyFilled);
    }

    #[tokio::test]
    async fn an_unrecognised_status_becomes_unknown_not_a_wrong_state() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/orders:by_client_order_id"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(order_response("some_new_alpaca_status")),
            )
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let ack = venue
            .get_order(&OrderRef::Client("cid-abc-123".to_string()))
            .await
            .unwrap();
        assert_eq!(ack.state, OrderState::Unknown);
        assert!(!ack.state.is_open());
        assert!(!ack.state.is_terminal());
    }

    #[tokio::test]
    async fn equity_quote_parses_top_of_book_from_json_numbers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/stocks/quotes/latest"))
            .and(query_param("symbols", "AAPL"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "quotes": {
                    "AAPL": {
                        "t": "2026-09-17T15:30:00.123456789Z",
                        "ax": "V", "ap": 191.38, "as": 3,
                        "bx": "V", "bp": 191.36, "bs": 2,
                        "c": ["R"], "z": "C"
                    }
                }
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let quote = venue
            .quote(&InstrumentId::new(VenueId::new("alpaca"), "AAPL"))
            .await
            .unwrap();

        assert_eq!(quote.bid, dec!(191.36));
        assert_eq!(quote.ask, dec!(191.38));
        assert_eq!(quote.mid, dec!(191.37));
        assert_eq!(quote.spread(), dec!(0.02));
        // Top-of-book endpoint: no depth is published, and the sizes it does
        // return are feed-dependent units, so `book` stays None.
        assert!(quote.book.is_none());
    }

    #[tokio::test]
    async fn crypto_quote_parses_the_orderbook_with_depth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1beta3/crypto/us/latest/orderbooks"))
            .and(query_param("symbols", "BTC/USD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "orderbooks": {
                    "BTC/USD": {
                        "t": "2026-09-17T15:30:00Z",
                        "b": [{"p": 64000.12, "s": 0.5}, {"p": 63999.0, "s": 1.25}],
                        "a": [{"p": 64010.88, "s": 0.3}]
                    }
                }
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let quote = venue
            .quote(&InstrumentId::new(VenueId::new("alpaca"), "BTC/USD"))
            .await
            .unwrap();

        assert_eq!(quote.bid, dec!(64000.12));
        assert_eq!(quote.ask, dec!(64010.88));
        assert_eq!(quote.mid, dec!(64005.50));
        let book = quote
            .book
            .as_ref()
            .expect("crypto orderbooks carry real depth");
        assert_eq!(book.bids.len(), 2);
        assert_eq!(book.bids[1].size, dec!(1.25));
        assert_eq!(quote.levels_for_side(Side::Buy).len(), 1);
    }

    #[tokio::test]
    async fn a_one_sided_quote_is_an_error_not_a_halved_mid() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/stocks/quotes/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "quotes": {"AAPL": {"t": "2026-09-17T15:30:00Z", "ap": 191.38, "bp": 0}}
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let err = venue
            .quote(&InstrumentId::new(VenueId::new("alpaca"), "AAPL"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("one-sided"), "{err}");
    }

    #[tokio::test]
    async fn non_2xx_responses_carry_the_status_and_body_into_the_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/orders"))
            .respond_with(
                ResponseTemplate::new(422)
                    .set_body_string(r#"{"code":40310000,"message":"insufficient buying power"}"#),
            )
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let err = venue
            .place_order(&order(
                equity_instrument(),
                OrderKind::Limit { price: dec!(191.5) },
                TimeInForce::Day,
                false,
            ))
            .await
            .unwrap_err();

        let rendered = format!("{err:#}");
        assert!(rendered.contains("422"), "{rendered}");
        assert!(rendered.contains("insufficient buying power"), "{rendered}");
        // And the context still says which order failed.
        assert!(rendered.contains("cid-abc-123"), "{rendered}");
    }

    #[tokio::test]
    async fn list_instruments_maps_equity_and_crypto_constraints() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/assets"))
            .and(query_param("asset_class", "us_equity"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "symbol": "AAPL", "class": "us_equity", "exchange": "NASDAQ",
                    "name": "Apple Inc. Common Stock", "status": "active",
                    "tradable": true, "fractionable": true
                },
                {
                    "symbol": "BRK.A", "class": "us_equity", "exchange": "NYSE",
                    "name": "Berkshire Hathaway", "status": "active",
                    "tradable": true, "fractionable": false
                },
                {
                    "symbol": "HALT", "class": "us_equity", "exchange": "NYSE",
                    "status": "active", "tradable": false, "fractionable": false
                }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/assets"))
            .and(query_param("asset_class", "crypto"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "symbol": "BTC/USD", "class": "crypto", "exchange": "CRYPTO",
                "name": "Bitcoin / US Dollar", "status": "active",
                "tradable": true, "fractionable": true,
                "min_order_size": "0.000026",
                "min_trade_increment": "0.000000001",
                "price_increment": "1"
            }])))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL", "BRK.A", "HALT", "BTC/USD", "NOPE"]);
        let instruments = venue
            .list_instruments(&ScanFilter::default())
            .await
            .unwrap();

        // HALT is not tradable and NOPE is not listed; both are skipped.
        assert_eq!(instruments.len(), 3);

        let aapl = &instruments[0];
        assert_eq!(aapl.asset_class, AssetClass::Equity);
        assert_eq!(aapl.tick_size, Some(dec!(0.01)));
        assert_eq!(aapl.lot_size, None, "fractionable names have no lot size");
        assert_eq!(aapl.min_notional, Some(dec!(1)));
        assert!(aapl.fractional);

        let brk = &instruments[1];
        assert_eq!(brk.lot_size, Some(Decimal::ONE), "whole shares only");
        assert!(!brk.fractional);

        let btc = &instruments[2];
        assert_eq!(btc.asset_class, AssetClass::CryptoSpot);
        assert_eq!(btc.tick_size, Some(dec!(1)));
        assert_eq!(btc.lot_size, Some(dec!(0.000000001)));
        assert_eq!(btc.quote_ccy, "USD");
        // Alpaca's crypto floor is a quantity, not a notional, so it has to
        // survive discovery in min_qty or the constraint is lost before
        // anything can enforce it.
        assert_eq!(btc.min_qty, Some(dec!(0.000026)));
        assert_eq!(btc.min_notional, None);
        assert_eq!(aapl.min_qty, None, "equities have no size floor");
        assert_eq!(
            btc.meta,
            InstrumentMeta::Spot {
                base: "BTC".to_string()
            }
        );

        // The asset listing is cached: a second scan makes no further calls.
        let before = server.received_requests().await.unwrap().len();
        venue
            .list_instruments(&ScanFilter::default())
            .await
            .unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), before);
    }

    #[tokio::test]
    async fn candles_come_back_oldest_first() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/stocks/bars"))
            .and(query_param("timeframe", "1Hour"))
            .and(query_param("sort", "desc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "bars": {
                    "AAPL": [
                        {"t": "2026-09-17T15:00:00Z", "o": 191.1, "h": 191.9, "l": 190.8, "c": 191.4, "v": 1200},
                        {"t": "2026-09-17T14:00:00Z", "o": 190.5, "h": 191.2, "l": 190.4, "c": 191.1, "v": 900}
                    ]
                }
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let candles = venue
            .candles(
                &InstrumentId::new(VenueId::new("alpaca"), "AAPL"),
                CandleInterval::H1,
                2,
            )
            .await
            .unwrap();

        assert_eq!(candles.len(), 2);
        assert!(candles[0].ts < candles[1].ts, "bars must be chronological");
        assert_eq!(candles[0].open, dec!(190.5));
        assert_eq!(candles[1].close, dec!(191.4));
        assert_eq!(candles[1].volume, dec!(1200));
    }

    #[tokio::test]
    async fn candles_for_an_unknown_symbol_are_empty_not_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/stocks/bars"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"bars": {}})))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let candles = venue
            .candles(
                &InstrumentId::new(VenueId::new("alpaca"), "AAPL"),
                CandleInterval::D1,
                5,
            )
            .await
            .unwrap();
        assert!(candles.is_empty());
    }

    #[tokio::test]
    async fn positions_recover_the_pair_separator_alpaca_drops() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/positions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "symbol": "AAPL", "asset_class": "us_equity",
                    "qty": "3", "avg_entry_price": "190.25"
                },
                {
                    "symbol": "BTCUSD", "asset_class": "crypto",
                    "qty": "0.00123456", "avg_entry_price": "64000.5"
                }
            ])))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL", "BTC/USD"]);
        let positions = venue.positions().await.unwrap();

        assert_eq!(positions[0].instrument.symbol, "AAPL");
        assert_eq!(positions[0].qty, dec!(3));
        // Alpaca reports crypto positions without the slash; without this the
        // position never reconciles against the BTC/USD instrument we traded.
        assert_eq!(positions[1].instrument.symbol, "BTC/USD");
        assert_eq!(positions[1].qty, dec!(0.00123456));
        assert_eq!(positions[1].avg_entry, dec!(64000.5));
    }

    #[tokio::test]
    async fn balance_uses_non_marginable_buying_power_not_margin() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/account"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "acct-1", "currency": "USD", "status": "ACTIVE",
                "cash": "150.00", "equity": "275.50",
                "buying_power": "600.00",
                "non_marginable_buying_power": "150.00"
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let balance = venue.balance().await.unwrap();
        assert_eq!(balance.ccy, "USD");
        assert_eq!(balance.available, dec!(150.00));
        assert_eq!(venue.equity().await.unwrap(), Some(dec!(275.50)));
    }

    #[tokio::test]
    async fn assert_tradable_fails_on_a_blocked_account() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/account"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "acct-1", "currency": "USD", "status": "ACTIVE",
                "cash": "150.00", "equity": "275.50",
                "trading_blocked": true
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let err = venue.assert_tradable().await.unwrap_err();
        assert!(err.to_string().contains("trading_blocked"), "{err}");
    }

    #[tokio::test]
    async fn cancel_all_tolerates_dead_orders_but_reports_real_failures() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v2/orders"))
            .respond_with(ResponseTemplate::new(207).set_body_json(json!([
                {"id": "a", "status": 200},
                {"id": "b", "status": 422},
                {"id": "c", "status": 500}
            ])))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        // A 207 envelope is a 2xx: without inspecting the entries the kill
        // switch would report success while order "c" is still live.
        let err = venue.cancel_all().await.unwrap_err();
        assert!(err.to_string().contains("c (HTTP 500)"), "{err}");
        assert!(!err.to_string().contains("HTTP 422"), "{err}");
    }

    #[tokio::test]
    async fn cancel_order_accepts_an_empty_204_body() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v2/orders/order-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        venue.cancel_order("order-1").await.unwrap();
    }

    /// Finding 14: /v2/orders caps a page at 500, and returning the first
    /// page as the whole answer means reconciliation decides the 501st order
    /// does not exist and stops tracking it.
    #[tokio::test]
    async fn open_orders_pages_past_the_first_five_hundred() {
        let server = MockServer::start().await;

        // A full first page, oldest first, then a short second page.
        let first: Vec<Value> = (0..ORDER_PAGE_SIZE)
            .map(|i| {
                let mut o = order_response("new");
                o["id"] = json!(format!("first-{i}"));
                o["submitted_at"] = json!(format!("2026-09-19T10:{:02}:00Z", i % 60));
                o
            })
            .collect();

        Mock::given(method("GET"))
            .and(path("/v2/orders"))
            .and(query_param("direction", "asc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(first))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let mut last = order_response("new");
        last["id"] = json!("second-0");
        last["submitted_at"] = json!("2026-09-19T11:00:00Z");
        Mock::given(method("GET"))
            .and(path("/v2/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([last])))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let orders = venue.open_orders().await.expect("both pages");

        assert_eq!(orders.len(), ORDER_PAGE_SIZE + 1);
        assert_eq!(orders.last().unwrap().venue_order_id, "second-0");
    }

    /// A full page whose rows carry no timestamp cannot advance the cursor.
    /// Looping forever is worse than stopping, but stopping silently is what
    /// this fix is about, so it warns and returns what it has.
    #[tokio::test]
    async fn open_orders_stops_rather_than_looping_when_the_cursor_cannot_advance() {
        let server = MockServer::start().await;
        let page: Vec<Value> = (0..ORDER_PAGE_SIZE)
            .map(|i| {
                let mut o = order_response("new");
                o["id"] = json!(format!("o-{i}"));
                o["submitted_at"] = Value::Null;
                o
            })
            .collect();

        Mock::given(method("GET"))
            .and(path("/v2/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let orders = venue.open_orders().await.expect("returns the page it has");
        assert_eq!(orders.len(), ORDER_PAGE_SIZE);
    }

    #[tokio::test]
    async fn open_orders_maps_every_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/orders"))
            .and(query_param("status", "open"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                order_response("new"),
                order_response("partially_filled")
            ])))
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        let orders = venue.open_orders().await.unwrap();
        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].state, OrderState::Accepted);
        assert_eq!(orders[1].state, OrderState::PartiallyFilled);
    }

    #[tokio::test]
    async fn the_clock_is_fetched_once_per_cycle_not_once_per_call() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/clock"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "timestamp": "2026-09-17T13:00:00-04:00",
                "is_open": false,
                "next_open": "2026-09-18T09:30:00-04:00",
                "next_close": "2026-09-18T16:00:00-04:00"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let venue = venue(&server, &["AAPL"]);
        // Thursday 11:00 ET: inside the static regular window, but the clock
        // says shut — an unscheduled halt or a holiday.
        let at = chrono_tz::America::New_York
            .with_ymd_and_hms(2026, 9, 17, 11, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert!(venue.session().is_open_at(at));
        assert!(!venue.accepts_orders_now_at(at).await.unwrap());
        // Second call is served from the cache; `.expect(1)` enforces it.
        assert!(!venue.accepts_orders_now_at(at).await.unwrap());
    }

    #[tokio::test]
    async fn crypto_only_venues_are_always_open_and_skip_the_clock() {
        let server = MockServer::start().await;
        let venue = venue(&server, &["BTC/USD", "ETH/USD"]);
        assert_eq!(*venue.session(), TradingSession::Always);
        assert_eq!(
            venue.capabilities().asset_classes,
            vec![AssetClass::CryptoSpot]
        );
        assert!(!venue.capabilities().limit_only_outside_regular);

        // A Sunday at 3am ET.
        let at = chrono_tz::America::New_York
            .with_ymd_and_hms(2026, 9, 20, 3, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert!(venue.accepts_orders_now_at(at).await.unwrap());
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_mixed_venue_reports_the_equity_session_and_both_classes() {
        let server = MockServer::start().await;
        let venue = venue(&server, &["AAPL", "BTC/USD"]);
        assert_eq!(*venue.session(), TradingSession::us_equity_extended());
        assert_eq!(
            venue.capabilities().asset_classes,
            vec![AssetClass::Equity, AssetClass::CryptoSpot]
        );
        assert!(venue.capabilities().limit_only_outside_regular);
        assert!(venue.capabilities().supports_client_order_id);
        assert!(venue.capabilities().supports_candles);
    }

    #[tokio::test]
    async fn settlement_is_always_none() {
        let server = MockServer::start().await;
        let venue = venue(&server, &["AAPL", "BTC/USD"]);
        for symbol in ["AAPL", "BTC/USD"] {
            let settled = venue
                .settlement(&InstrumentId::new(VenueId::new("alpaca"), symbol))
                .await
                .unwrap();
            assert!(settled.is_none());
        }
    }

    #[tokio::test]
    async fn config_debug_never_leaks_credentials() {
        let config = AlpacaConfig::live("AKREALKEYID", "realsecretvalue").with_symbols(["AAPL"]);
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("AKREALKEYID"), "{rendered}");
        assert!(!rendered.contains("realsecretvalue"), "{rendered}");
        assert_eq!(config.trading_base_url, LIVE_TRADING_BASE_URL);
        assert_eq!(config.data_base_url, DATA_BASE_URL);
    }
}
