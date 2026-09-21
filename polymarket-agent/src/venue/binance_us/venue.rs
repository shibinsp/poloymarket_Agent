//! The `Venue` implementation for Binance.US.
//!
//! Spot crypto only, so the session is `Always` — no clock call, no
//! extended-hours flag, no equity-style lot arithmetic.
//!
//! Three shapes differ from the other adapters in ways that matter:
//!
//! * **Every order operation needs the symbol.** Binance identifies an order
//!   by `(symbol, orderId)`, not by id alone, but `Venue::cancel_order` and
//!   `OrderRef::Venue` carry only an id. So the id this adapter reports is
//!   `SYMBOL:orderId` — see [`OrderKey`].
//! * **Commissions are charged in whichever asset was received**, which may be
//!   the base, the quote or BNB. Summing them as one number would add bitcoin
//!   to dollars, so they are converted at the fill price and anything that
//!   cannot be converted is reported rather than silently dropped.
//! * **There is no average fill price**, only a cumulative quote value; the
//!   average is that divided by the filled quantity.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use reqwest::Method;
use rust_decimal::Decimal;
use tracing::{info, instrument, warn};

use super::auth::BinanceSigner;
use super::models::{money, AccountInfo, Depth, ExchangeInfo, Kline, Order, SymbolInfo, Trade};
use super::rest::BinanceRest;
use crate::market::models::{OrderBookSnapshot, PriceLevel};
use crate::venue::equity::{mark_equity, single_quote_currency, Holding};
use crate::venue::session::TradingSession;
use crate::venue::types::{
    AssetClass, Balance, Candle, CandleInterval, Instrument, InstrumentId, InstrumentMeta,
    OrderAck, OrderKind, OrderRef, OrderRequest, OrderState, Position, Quote, ScanFilter,
    Settlement, Side, TimeInForce, VenueCapabilities, VenueId,
};
use crate::venue::Venue;

pub const DEFAULT_VENUE_ID: &str = "binance_us";
pub const BASE_URL: &str = "https://api.binance.us";
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Depth levels to request. Binance bills by level count; ten is enough for a
/// mid and a slippage estimate.
const BOOK_DEPTH: usize = 10;
/// Binance's cap for `/klines`.
const MAX_KLINES: usize = 1000;

const EXCHANGE_INFO_PATH: &str = "/api/v3/exchangeInfo";
const DEPTH_PATH: &str = "/api/v3/depth";
const KLINES_PATH: &str = "/api/v3/klines";
const ORDER_PATH: &str = "/api/v3/order";
const OPEN_ORDERS_PATH: &str = "/api/v3/openOrders";
const ACCOUNT_PATH: &str = "/api/v3/account";
const MY_TRADES_PATH: &str = "/api/v3/myTrades";

/// How long an account snapshot is reused.
///
/// `balance`, `equity` and `positions` are three projections of one payload,
/// and a cycle reads them within moments of each other. Without this each one
/// mints a fresh signed request at weight 20 — and, worse, lets cash and
/// equity come from *different* snapshots: a fill landing between the two
/// makes free cash exceed the cash inside the equity figure, which a single
/// response made structurally impossible.
const ACCOUNT_CACHE_TTL: Duration = Duration::from_secs(2);

/// A Binance order reference: the symbol and the numeric id, together.
///
/// Binance identifies an order by `(symbol, orderId)` — `GET /order`,
/// `DELETE /order` and `GET /myTrades` all reject a bare id — but the `Venue`
/// trait passes a single string, and the ledger stores a single string. So the
/// symbol travels inside it.
///
/// The separator is `:` because Binance symbols are alphanumeric, so it cannot
/// occur in either half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderKey {
    pub symbol: String,
    pub order_id: i64,
}

impl OrderKey {
    pub fn new(symbol: impl Into<String>, order_id: i64) -> Self {
        Self {
            symbol: symbol.into(),
            order_id,
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let (symbol, id) = raw.split_once(':').with_context(|| {
            format!(
                "Binance.US order ids are {{SYMBOL}}:{{orderId}}, because Binance cannot look up \
                 an order without its symbol — got {raw:?}"
            )
        })?;
        let order_id: i64 = id
            .trim()
            .parse()
            .with_context(|| format!("Binance.US order id {id:?} in {raw:?} is not a number"))?;
        anyhow::ensure!(
            !symbol.trim().is_empty(),
            "Binance.US order reference {raw:?} has no symbol"
        );
        Ok(Self::new(symbol.trim().to_uppercase(), order_id))
    }
}

impl std::fmt::Display for OrderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.symbol, self.order_id)
    }
}

pub struct BinanceUsConfig {
    pub venue_id: VenueId,
    pub base_url: String,
    api_key: String,
    secret_key: String,
    pub symbols: Vec<String>,
    pub request_timeout: Duration,
    pub account_cache_ttl: Duration,
}

/// Hand-written so the secret can never reach a log line or panic message.
impl std::fmt::Debug for BinanceUsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BinanceUsConfig")
            .field("venue_id", &self.venue_id)
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field("symbols", &self.symbols)
            .finish_non_exhaustive()
    }
}

impl BinanceUsConfig {
    pub fn new(api_key: impl Into<String>, secret_key: impl Into<String>) -> Self {
        Self {
            venue_id: VenueId::new(DEFAULT_VENUE_ID),
            base_url: BASE_URL.to_string(),
            api_key: api_key.into(),
            secret_key: secret_key.into(),
            symbols: Vec::new(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            account_cache_ttl: ACCOUNT_CACHE_TTL,
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

    /// Reuse window for the account snapshot. Zero disables reuse.
    pub fn with_account_cache_ttl(mut self, ttl: Duration) -> Self {
        self.account_cache_ttl = ttl;
        self
    }
}

pub struct BinanceUsVenue {
    id: VenueId,
    caps: VenueCapabilities,
    session: TradingSession,
    symbols: Vec<String>,
    quote_ccy: String,
    rest: BinanceRest,
    account_cache_ttl: Duration,
    account_cache: tokio::sync::Mutex<Option<(Instant, Arc<AccountInfo>)>>,
}

impl std::fmt::Debug for BinanceUsVenue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BinanceUsVenue")
            .field("id", &self.id)
            .field("symbols", &self.symbols)
            .field("rest", &self.rest)
            .finish_non_exhaustive()
    }
}

impl BinanceUsVenue {
    pub fn new(config: BinanceUsConfig) -> Result<Self> {
        // One quote currency, or the balance cannot be reported faithfully:
        // BTC/USD and BTC/USDT hold cash in different assets, and a single
        // `available`/`total` pair can only describe one of them.
        let quote_ccy = single_quote_currency(&config.symbols).map_err(|found| {
            anyhow::anyhow!(
                "Binance.US venue {} has symbols quoting in {} — one venue reports one \
                 currency, so configure a venue entry per quote currency",
                config.venue_id,
                found.join(" and ")
            )
        })?;

        let signer = BinanceSigner::new(&config.api_key, &config.secret_key)?;
        let rest = BinanceRest::new(&config.base_url, signer, config.request_timeout)?;

        info!(
            venue = %config.venue_id,
            base_url = %config.base_url,
            symbols = config.symbols.len(),
            "Binance.US venue configured"
        );

        Ok(Self {
            id: config.venue_id,
            caps: VenueCapabilities {
                asset_classes: vec![AssetClass::CryptoSpot],
                limit_only_outside_regular: false,
                supports_client_order_id: true,
                supports_candles: true,
            },
            session: TradingSession::Always,
            symbols: config.symbols,
            quote_ccy: quote_ccy.unwrap_or_else(|| "USD".to_string()),
            rest,
            account_cache_ttl: config.account_cache_ttl,
            account_cache: tokio::sync::Mutex::new(None),
        })
    }

    /// Binance spells pairs `BTCUSD` with no separator; the rest of this
    /// codebase uses `BTC/USD`.
    ///
    /// The reverse direction cannot be done by string surgery — `BTCUSD` could
    /// split as `BTC/USD` or `BT/CUSD` — so it always comes from Binance's own
    /// `baseAsset`/`quoteAsset`, never from guessing.
    fn to_binance_symbol(symbol: &str) -> String {
        symbol.trim().to_uppercase().replace('/', "")
    }

    fn now_ms() -> Result<u64> {
        Ok(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_millis() as u64)
    }

    /// The single currency this venue's cash and equity are reported in.
    ///
    /// Derived from the configured pairs at construction and refused there if
    /// they span more than one: summing USD and USDT asserts they are
    /// interchangeable, and dropping one silently understates the account.
    fn cash_ccy(&self) -> &str {
        &self.quote_ccy
    }

    /// The configured symbol whose base asset is `asset`, if the agent trades
    /// one.
    ///
    /// A spot balance names only the asset — `BTC` — and the pair it belongs
    /// to is a choice. `None` means the agent does not trade this asset, and
    /// the caller reports it rather than inventing a symbol for it.
    /// The base and quote assets of a Binance symbol, from the configured
    /// pair rather than from splitting the concatenated name.
    ///
    /// `BTCUSD` could split as `BTC/USD` or `BT/CUSD`; this module's header
    /// says so and then the commission arithmetic guessed anyway. The
    /// configured symbol already carries the split, so it is read from there.
    fn assets_for(&self, binance_symbol: &str) -> Option<(String, String)> {
        self.symbols.iter().find_map(|s| {
            if Self::to_binance_symbol(s) != binance_symbol.to_uppercase() {
                return None;
            }
            let upper = s.trim().to_uppercase();
            let (base, quote) = upper.split_once('/')?;
            Some((base.to_string(), quote.to_string()))
        })
    }

    /// The configured symbol whose base asset is `asset`, if the agent trades
    /// one.
    ///
    /// A spot balance names only the asset — `BTC` — and the pair it belongs
    /// to is a choice. `None` means the agent does not trade this asset, and
    /// the caller reports it rather than inventing a symbol for it.
    fn symbol_for_base(&self, asset: &str) -> Option<String> {
        let mut matches = self
            .symbols
            .iter()
            .map(|s| s.trim().to_uppercase())
            .filter(|s| {
                s.split('/')
                    .next()
                    .is_some_and(|b| b.eq_ignore_ascii_case(asset))
            });

        let first = matches.next()?;
        if matches.next().is_some() {
            warn!(
                venue = %self.id,
                asset = asset,
                symbol = %first,
                "Several configured symbols share this base — a spot balance names only \
                 the asset, so the whole holding is attributed to the first"
            );
        }
        Some(first)
    }

    fn instrument_from(&self, info: &SymbolInfo) -> Result<Instrument> {
        let base = info
            .base_asset
            .clone()
            .with_context(|| format!("Binance.US listed {} with no baseAsset", info.symbol))?;
        let quote = info
            .quote_asset
            .clone()
            .with_context(|| format!("Binance.US listed {} with no quoteAsset", info.symbol))?;
        let symbol = format!("{}/{}", base.to_uppercase(), quote.to_uppercase());

        Ok(Instrument {
            id: InstrumentId::new(self.id.clone(), symbol.clone()),
            asset_class: AssetClass::CryptoSpot,
            display_name: symbol,
            quote_ccy: quote.to_uppercase(),
            tick_size: info.tick_size()?,
            lot_size: info.step_size()?,
            min_notional: info.min_notional()?,
            min_qty: info.min_qty()?,
            fractional: true,
            meta: InstrumentMeta::Spot {
                base: base.to_uppercase(),
            },
        })
    }

    /// Cash and holdings, split once, from one snapshot.
    ///
    /// `balance` and `equity` both have to find the cash row and decide what
    /// counts as a holding; writing those twice let a later change to either
    /// rule land in only one of them, with `available` and the cash inside
    /// equity describing different account sets.
    async fn cash_and_holdings(&self) -> Result<(Decimal, Decimal, Vec<Holding>)> {
        let account = self.account().await?;
        let ccy = self.cash_ccy().to_string();

        let cash = account
            .balances
            .iter()
            .find(|b| b.asset.eq_ignore_ascii_case(&ccy));
        // `free` only funds an order; what is locked is committed to a resting
        // one — but it is still equity.
        let available = cash
            .map(|b| b.free_amount())
            .transpose()?
            .unwrap_or(Decimal::ZERO);
        let cash_total = cash
            .map(|b| b.total_amount())
            .transpose()?
            .unwrap_or(Decimal::ZERO);

        let mut holdings: Vec<Holding> = Vec::new();
        for balance in &account.balances {
            let asset = balance.asset.to_uppercase();
            if asset == ccy {
                continue;
            }
            let qty = balance.total_amount()?;
            if qty <= Decimal::ZERO {
                continue;
            }
            holdings.push(Holding {
                asset: asset.clone(),
                qty,
                // Cash in another currency included: leaving it out
                // understates the account rather than reporting that it could
                // not be valued.
                symbol: self.symbol_for_base(&asset),
            });
        }

        Ok((available, cash_total, holdings))
    }

    async fn account(&self) -> Result<Arc<AccountInfo>> {
        let mut cache = self.account_cache.lock().await;
        if let Some((fetched, account)) = cache.as_ref() {
            if fetched.elapsed() < self.account_cache_ttl {
                return Ok(account.clone());
            }
        }

        let fresh: AccountInfo = self
            .rest
            .signed(Method::GET, ACCOUNT_PATH, &[], Self::now_ms()?)
            .await
            .context("Failed to fetch the Binance.US account")?;
        let fresh = Arc::new(fresh);
        *cache = Some((Instant::now(), fresh.clone()));
        Ok(fresh)
    }

    async fn fetch_order(&self, key: &OrderKey, params: Vec<(&str, String)>) -> Result<OrderAck> {
        let order: Order = self
            .rest
            .signed(Method::GET, ORDER_PATH, &params, Self::now_ms()?)
            .await
            .with_context(|| format!("Failed to fetch Binance.US order {key}"))?;
        self.to_ack_with_fees(&order, &key.symbol).await
    }

    /// Build an ack, fetching the commissions when the order has filled.
    ///
    /// `GET /order` does not report commissions — only the create response
    /// does, and only for the part that filled immediately. A limit order that
    /// fills later would otherwise be booked with **zero fees**, which
    /// understates the cost of every round trip the sizing gate is built to
    /// weigh.
    async fn to_ack_with_fees(&self, order: &Order, symbol: &str) -> Result<OrderAck> {
        let mut ack = to_ack(order, symbol)?;
        if ack.filled_qty <= Decimal::ZERO {
            return Ok(ack);
        }

        if !order.fills.is_empty() {
            ack.fees = self.commission_total(&order.fills, symbol);
            return Ok(ack);
        }

        let params = [
            ("symbol", symbol.to_string()),
            ("orderId", order.order_id.to_string()),
        ];
        match self
            .rest
            .signed::<Vec<Trade>>(Method::GET, MY_TRADES_PATH, &params, Self::now_ms()?)
            .await
        {
            Ok(trades) => ack.fees = self.commission_total(&trades, symbol),
            // Reported, not raised: the fill itself is real and must be
            // recorded. A missing fee is an understated cost; a dropped fill
            // is a position the ledger does not know about.
            Err(e) => warn!(
                venue = %self.id,
                order = %OrderKey::new(symbol, order.order_id),
                error = %format!("{e:#}"),
                "Could not read Binance.US commissions — the fill is recorded with zero fees"
            ),
        }
        Ok(ack)
    }

    /// Commissions in quote currency.
    ///
    /// Binance charges in whichever asset was received, so a buy is commissioned
    /// in the base asset and a sell in the quote. Adding them as one number
    /// would add bitcoin to dollars; base-asset commission is converted at the
    /// price of the trade that incurred it.
    fn commission_total(&self, trades: &[Trade], symbol: &str) -> Decimal {
        let mut total = Decimal::ZERO;
        let mut unconvertible: Vec<String> = Vec::new();

        // Without the pair's own split there is no way to tell a base-asset
        // commission from a quote-asset one, and guessing it wrong multiplies
        // a dollar fee by the fill price — a 60000x overstatement on a BTC
        // pair. Reporting zero and saying so is the honest alternative.
        let Some((base, quote)) = self.assets_for(symbol) else {
            if trades.iter().any(|t| t.commission_asset.is_some()) {
                warn!(
                    venue = %self.id,
                    symbol,
                    "Binance.US reported a commission on a symbol this venue does not have \
                     configured — it cannot be priced, so the recorded fee is zero"
                );
            }
            return Decimal::ZERO;
        };

        for trade in trades {
            let Some(asset) = trade.commission_asset.as_deref() else {
                continue;
            };
            let Ok(commission) = trade.commission_amount() else {
                warn!(
                    symbol,
                    "Binance.US sent an unparseable commission — treating it as zero"
                );
                continue;
            };
            if commission.is_zero() {
                continue;
            }

            let asset_upper = asset.to_uppercase();
            if asset_upper == quote {
                // Already the quote currency.
                total += commission;
            } else if asset_upper == base {
                match trade.price_amount() {
                    Ok(price) if price > Decimal::ZERO => total += commission * price,
                    _ => unconvertible.push(asset_upper),
                }
            } else {
                // BNB, typically. Converting needs a third quote this call
                // does not have.
                unconvertible.push(asset_upper);
            }
        }

        if !unconvertible.is_empty() {
            warn!(
                venue = %self.id,
                symbol,
                assets = %unconvertible.join(", "),
                "Binance.US charged commission in an asset this order cannot price — \
                 the recorded fee understates the real cost"
            );
        }
        total
    }
}

/// Whether a cancel failed only because there was nothing to cancel.
///
/// Binance answers `DELETE /openOrders` for an empty book with `-2011 Unknown
/// order sent`. Treating that as a failure would have the kill switch report a
/// book it did in fact clear.
fn is_no_orders_to_cancel(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("(-2011)")
}

/// Binance's order status vocabulary, mapped to ours.
pub fn to_order_state(order: &Order) -> OrderState {
    let filled = order
        .executed_qty
        .as_deref()
        .and_then(|v| money(v, "executedQty").ok())
        .unwrap_or(Decimal::ZERO);

    match order
        .status
        .as_deref()
        .unwrap_or_default()
        .to_uppercase()
        .as_str()
    {
        "FILLED" => OrderState::Filled,
        "PARTIALLY_FILLED" => OrderState::PartiallyFilled,
        // Still live at the venue: a pending cancel can still fill.
        "NEW" | "PENDING_CANCEL" => {
            if filled > Decimal::ZERO {
                OrderState::PartiallyFilled
            } else {
                OrderState::Accepted
            }
        }
        // A cancel or expiry that caught a partial fill is not the same as one
        // that caught nothing: the filled part is a real position.
        "CANCELED" => {
            if filled > Decimal::ZERO {
                OrderState::PartiallyFilled
            } else {
                OrderState::Cancelled
            }
        }
        "EXPIRED" | "EXPIRED_IN_MATCH" => {
            if filled > Decimal::ZERO {
                OrderState::PartiallyFilled
            } else {
                OrderState::Expired
            }
        }
        "REJECTED" => OrderState::Rejected("Binance.US rejected the order".to_string()),
        // An unrecognised status is not evidence of anything. Guessing here is
        // how a live order gets abandoned or duplicated.
        other => {
            warn!(
                status = other,
                order_id = order.order_id,
                "Unrecognised Binance.US order status"
            );
            OrderState::Unknown
        }
    }
}

/// An ack with no commission information. See `to_ack_with_fees`.
pub fn to_ack(order: &Order, symbol: &str) -> Result<OrderAck> {
    let filled_qty = order
        .executed_qty
        .as_deref()
        .map(|v| money(v, "executedQty"))
        .transpose()?
        .unwrap_or(Decimal::ZERO);

    // Binance reports no average price, only the cumulative quote value.
    // Dividing by zero filled quantity would be a price for nothing.
    let avg_fill_price = if filled_qty > Decimal::ZERO {
        order
            .cummulative_quote_qty
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .map(|v| money(v, "cummulativeQuoteQty"))
            .transpose()?
            .map(|quote| quote / filled_qty)
            .filter(|p| *p > Decimal::ZERO)
    } else {
        None
    };

    Ok(OrderAck {
        venue_order_id: OrderKey::new(symbol.to_uppercase(), order.order_id).to_string(),
        client_order_id: order.client_order_id.clone().unwrap_or_default(),
        state: to_order_state(order),
        filled_qty,
        avg_fill_price,
        fees: Decimal::ZERO,
    })
}

#[async_trait]
impl Venue for BinanceUsVenue {
    fn id(&self) -> &VenueId {
        &self.id
    }

    fn capabilities(&self) -> &VenueCapabilities {
        &self.caps
    }

    fn session(&self) -> &TradingSession {
        &self.session
    }

    /// Refuse to trade an account Binance will not let trade.
    ///
    /// A restricted account still answers `/account` with balances, so without
    /// this every order is rejected one at a time, each having already spent a
    /// valuation.
    async fn trading_readiness(&self) -> Result<()> {
        let account = self.account().await?;
        if account.can_trade == Some(false) {
            bail!(
                "Binance.US reports this account cannot trade — check the API key's \
                 permissions and the account's status"
            );
        }
        Ok(())
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

        let wanted: Vec<String> = requested
            .iter()
            .map(|s| Self::to_binance_symbol(s))
            .collect();
        let wanted_set: HashSet<&String> = wanted.iter().collect();

        // Deliberately unfiltered, and filtered locally instead.
        //
        // `exchangeInfo?symbols=[...]` fails the *whole* request with
        // `-1121 Invalid symbol` if any one of them is unknown, so a single
        // delisted pair would take the rest of the universe down with it —
        // and the per-symbol "does not list this symbol" warning below could
        // never fire, because Binance never sends a partial answer. One
        // unfiltered call costs the same weight and cannot fail this way.
        let info: ExchangeInfo = self
            .rest
            .public(EXCHANGE_INFO_PATH, &[])
            .await
            .context("Failed to list Binance.US symbols")?;

        let mut out = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for symbol_info in &info.symbols {
            let upper = symbol_info.symbol.to_uppercase();
            if !wanted_set.contains(&upper) {
                continue;
            }
            if !seen.insert(upper) {
                continue;
            }
            if !symbol_info.tradeable() {
                warn!(
                    symbol = %symbol_info.symbol,
                    status = symbol_info.status.as_deref().unwrap_or("<absent>"),
                    "Binance.US will not trade this symbol right now — skipping"
                );
                continue;
            }
            out.push(self.instrument_from(symbol_info)?);
        }

        // Configured symbols Binance did not return at all. Trading a smaller
        // universe than configured is a quiet way to under-trade.
        for (symbol, binance_symbol) in requested.iter().zip(&wanted) {
            if !seen.contains(binance_symbol) {
                warn!(symbol = %symbol, "Binance.US does not list this symbol — skipping");
            }
        }

        if let Some(max) = filter.max_results {
            out.truncate(max);
        }
        Ok(out)
    }

    #[instrument(skip(self), fields(venue = %self.id, symbol = %id.symbol))]
    async fn quote(&self, id: &InstrumentId) -> Result<Quote> {
        let symbol = Self::to_binance_symbol(&id.symbol);
        let depth: Depth = self
            .rest
            .public(
                DEPTH_PATH,
                &[
                    ("symbol", symbol.clone()),
                    ("limit", BOOK_DEPTH.to_string()),
                ],
            )
            .await
            .with_context(|| format!("Failed to fetch the Binance.US book for {symbol}"))?;

        let bids: Vec<PriceLevel> = depth
            .bids
            .iter()
            .map(|l| {
                Ok(PriceLevel {
                    price: money(l.price(), "bid price")?,
                    size: money(l.qty(), "bid size")?,
                })
            })
            .collect::<Result<_>>()?;
        let asks: Vec<PriceLevel> = depth
            .asks
            .iter()
            .map(|l| {
                Ok(PriceLevel {
                    price: money(l.price(), "ask price")?,
                    size: money(l.qty(), "ask size")?,
                })
            })
            .collect::<Result<_>>()?;

        // A one-sided book has no mid. Halving the side that exists invents a
        // price to trade against.
        let (Some(best_bid), Some(best_ask)) = (bids.first(), asks.first()) else {
            bail!("Binance.US returned a one-sided book for {symbol} — no mid exists");
        };

        let (bid, ask) = (best_bid.price, best_ask.price);
        let mid = (bid + ask) / Decimal::TWO;
        let ts = Utc::now();

        Ok(Quote {
            instrument: id.clone(),
            bid,
            ask,
            mid,
            last: None,
            ts,
            book: Some(OrderBookSnapshot {
                token_id: symbol,
                bids,
                asks,
                spread: ask - bid,
                midpoint: mid,
                // Shaped for prediction markets; meaningless for a spot pair,
                // so zeroed rather than filled with a mid a caller might read
                // as a probability.
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
        let symbol = Self::to_binance_symbol(&id.symbol);
        // Binance serves four-hour bars natively, so unlike Coinbase there is
        // nothing to fold and no chance of a mislabelled granularity.
        let binance_interval = match interval {
            CandleInterval::H1 => "1h",
            CandleInterval::H4 => "4h",
            CandleInterval::D1 => "1d",
        };

        let klines: Vec<Kline> = self
            .rest
            .public(
                KLINES_PATH,
                &[
                    ("symbol", symbol.clone()),
                    ("interval", binance_interval.to_string()),
                    ("limit", limit.clamp(1, MAX_KLINES).to_string()),
                ],
            )
            .await
            .with_context(|| format!("Failed to fetch Binance.US klines for {symbol}"))?;

        let mut candles: Vec<Candle> = klines
            .iter()
            .map(|k| {
                let ms = k.open_time_ms()?;
                Ok(Candle {
                    ts: Utc.timestamp_millis_opt(ms).single().with_context(|| {
                        format!("Binance.US sent an impossible candle time: {ms}")
                    })?,
                    open: k.decimal(1, "candle open")?,
                    high: k.decimal(2, "candle high")?,
                    low: k.decimal(3, "candle low")?,
                    close: k.decimal(4, "candle close")?,
                    volume: k.decimal(5, "candle volume")?,
                })
            })
            .collect::<Result<_>>()?;

        // Binance returns oldest first already, but every indicator here
        // depends on that and a reversed series produces a plausible number
        // from the wrong data rather than an error.
        candles.sort_by_key(|c| c.ts);
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
        let symbol = Self::to_binance_symbol(request.instrument.symbol());
        let side = match request.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };

        let mut params: Vec<(&str, String)> =
            vec![("symbol", symbol.clone()), ("side", side.to_string())];

        // Apply the instrument's own constraints before Binance does. The
        // filters are parsed out of `exchangeInfo` into the `Instrument` and
        // then have to be *used*: an exit sized from a spot balance
        // (0.00456789 BTC against a 0.00001 step) comes back `-1013 Filter
        // failure: LOT_SIZE`, and the position stays open through the next
        // drawdown. Alpaca grew this same guard after the same bug.
        let qty = request.instrument.round_qty(request.qty);
        if qty <= Decimal::ZERO {
            bail!(
                "Quantity {} for {symbol} rounds to zero against the venue's lot size — \
                 the order would only be rejected",
                request.qty
            );
        }
        // Checked after rounding, because the size submitted is the one that
        // has to clear.
        if !request.instrument.meets_min_qty(qty) {
            bail!(
                "Quantity {qty} for {symbol} is below Binance.US's minimum order size of {}",
                request
                    .instrument
                    .min_qty
                    .unwrap_or(Decimal::ZERO)
                    .normalize()
            );
        }

        match request.kind {
            OrderKind::Limit { price } => {
                // Binance has no good-till-date on spot, and no day orders on
                // a market that never closes. Rewriting either into GTC would
                // turn "fill now or be gone" into a resting order that fills
                // later at a price the decision no longer supports.
                let tif = match request.tif {
                    TimeInForce::Gtc => "GTC",
                    TimeInForce::Ioc => "IOC",
                    other => bail!(
                        "Binance.US spot cannot express {other:?} for a limit order; \
                         use Gtc or Ioc"
                    ),
                };
                if price <= Decimal::ZERO {
                    bail!("Binance.US limit price must be positive, got {price} for {symbol}");
                }
                let ticked = request.instrument.round_price(price, request.side);
                if ticked <= Decimal::ZERO {
                    bail!(
                        "Limit price {price} for {symbol} rounds to zero against the \
                         venue's tick size"
                    );
                }
                // The value floor is on the rounded pair, not the requested
                // one — rounding down can cross it.
                if !request.instrument.meets_min_notional(qty * ticked) {
                    bail!(
                        "Order value {} for {symbol} is below Binance.US's minimum of {}",
                        (qty * ticked).normalize(),
                        request
                            .instrument
                            .min_notional
                            .unwrap_or(Decimal::ZERO)
                            .normalize()
                    );
                }
                params.push(("type", "LIMIT".to_string()));
                params.push(("timeInForce", tif.to_string()));
                params.push(("quantity", qty.normalize().to_string()));
                params.push(("price", ticked.normalize().to_string()));
            }
            // Unlike Coinbase, Binance sizes a market order in the base asset
            // on both sides, so a market buy needs no price to express.
            OrderKind::Market => {
                params.push(("type", "MARKET".to_string()));
                params.push(("quantity", qty.normalize().to_string()));
            }
        }

        params.push(("newClientOrderId", request.client_order_id.clone()));
        // FULL rather than the ACK default: it carries the fills, and so the
        // commissions, for the part that filled immediately.
        params.push(("newOrderRespType", "FULL".to_string()));

        let order: Order = self
            .rest
            .signed(Method::POST, ORDER_PATH, &params, Self::now_ms()?)
            .await
            .context("Failed to submit the Binance.US order")?;

        self.to_ack_with_fees(&order, &symbol).await
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn get_order(&self, order: &OrderRef) -> Result<OrderAck> {
        match order {
            OrderRef::Venue(raw) => {
                let key = OrderKey::parse(raw)?;
                let params = vec![
                    ("symbol", key.symbol.clone()),
                    ("orderId", key.order_id.to_string()),
                ];
                self.fetch_order(&key, params).await
            }
            OrderRef::Client(client_order_id) => {
                // Binance looks up by client id natively — but still only
                // within one symbol. The reconciler reaches this path when no
                // venue id was recorded, so the symbol is not known here.
                //
                // `openOrders` returns `clientOrderId` account-wide in one
                // request, so a still-resting order — which is what this path
                // is usually chasing — costs one round trip instead of one
                // signed request per configured symbol, every cycle.
                if let Ok(open) = self.open_orders().await {
                    if let Some(ack) = open.iter().find(|a| a.client_order_id == *client_order_id) {
                        let key = OrderKey::parse(&ack.venue_order_id)?;
                        let params = vec![
                            ("symbol", key.symbol.clone()),
                            ("orderId", key.order_id.to_string()),
                        ];
                        return self.fetch_order(&key, params).await;
                    }
                }

                // Not resting: it filled, was cancelled, or never arrived. Now
                // the per-symbol search is the only way left to tell which.
                let mut last_error = None;
                for symbol in &self.symbols {
                    let binance_symbol = Self::to_binance_symbol(symbol);
                    let params = vec![
                        ("symbol", binance_symbol.clone()),
                        ("origClientOrderId", client_order_id.clone()),
                    ];
                    let key = OrderKey::new(binance_symbol, 0);
                    match self.fetch_order(&key, params).await {
                        Ok(ack) => return Ok(ack),
                        Err(e) => last_error = Some(e),
                    }
                }
                match last_error {
                    Some(e) => Err(e).with_context(|| {
                        format!(
                            "No configured Binance.US symbol has an order with client id \
                             {client_order_id}"
                        )
                    }),
                    None => bail!(
                        "Cannot look up Binance.US client id {client_order_id}: the venue has \
                         no configured symbols to search"
                    ),
                }
            }
        }
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn cancel_order(&self, venue_order_id: &str) -> Result<()> {
        let key = OrderKey::parse(venue_order_id)?;
        let params = [
            ("symbol", key.symbol.clone()),
            ("orderId", key.order_id.to_string()),
        ];
        let _: Order = self
            .rest
            .signed(Method::DELETE, ORDER_PATH, &params, Self::now_ms()?)
            .await
            .with_context(|| format!("Failed to cancel Binance.US order {key}"))?;
        Ok(())
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn cancel_all(&self) -> Result<()> {
        // Driven from the **configured** universe, not from what the account
        // happens to be holding, for two reasons.
        //
        // Mandate: `DELETE /openOrders` cancels a whole symbol's book. Taking
        // the symbol list from an account-wide listing would have the kill
        // switch cancel the operator's own resting orders on pairs the agent
        // does not trade — money it has no business touching.
        //
        // Reach: deriving it from `open_orders()` also made a failed listing
        // abort before a single cancel, which is the failure this is supposed
        // to be the answer to. The symbols are known locally, so no call has
        // to succeed first.
        if self.symbols.is_empty() {
            return Ok(());
        }

        let mut failed = Vec::new();
        let mut cancelled = 0usize;
        for symbol in &self.symbols {
            let binance_symbol = Self::to_binance_symbol(symbol);
            let params = [("symbol", binance_symbol.clone())];
            match self
                .rest
                .signed::<serde_json::Value>(
                    Method::DELETE,
                    OPEN_ORDERS_PATH,
                    &params,
                    Self::now_ms()?,
                )
                .await
            {
                Ok(_) => cancelled += 1,
                // Nothing was resting on that symbol. That is the outcome
                // wanted, not a failure — and treating it as one would have
                // the kill switch report a book it actually cleared.
                Err(e) if is_no_orders_to_cancel(&e) => {}
                // One symbol failing must not cost the cancels for the rest.
                Err(e) => failed.push(format!("{binance_symbol}: {e:#}")),
            }
        }

        if !failed.is_empty() {
            bail!(
                "Binance.US could not cancel {} symbol(s) — orders may still be resting: {}",
                failed.len(),
                failed.join("; ")
            );
        }
        info!(symbols = cancelled, "Cancelled resting Binance.US orders");
        Ok(())
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn open_orders(&self) -> Result<Vec<OrderAck>> {
        // Account-wide in one unpaged response, then filtered to the
        // configured universe. The filter is the point: the orphan audit
        // turns any resting order it has no local record of into an
        // `UntilResume` halt, so an operator's own manual order on a pair the
        // agent does not trade would halt it on the first cycle — the same
        // trap `positions()` guards against, which this originally did not.
        let orders: Vec<Order> = self
            .rest
            .signed(Method::GET, OPEN_ORDERS_PATH, &[], Self::now_ms()?)
            .await
            .context("Failed to list open Binance.US orders")?;

        let mine: HashSet<String> = self
            .symbols
            .iter()
            .map(|s| Self::to_binance_symbol(s))
            .collect();

        let mut acks = Vec::new();
        let mut outside = 0usize;
        for order in &orders {
            let symbol = order.symbol.clone().unwrap_or_default().to_uppercase();
            if !mine.contains(&symbol) {
                outside += 1;
                continue;
            }
            // Lossy on purpose: one unparseable field must not cost the whole
            // listing. The id and the symbol are what the audit needs, and
            // both survive.
            match to_ack(order, &symbol) {
                Ok(ack) => acks.push(ack),
                Err(e) => {
                    warn!(
                        order_id = order.order_id,
                        symbol = %symbol,
                        error = %format!("{e:#}"),
                        "Binance.US sent an unparseable order field — keeping the order, with \
                         its state reported as unknown"
                    );
                    acks.push(OrderAck {
                        venue_order_id: OrderKey::new(symbol, order.order_id).to_string(),
                        client_order_id: order.client_order_id.clone().unwrap_or_default(),
                        // Not `to_order_state`: it reads an unparseable
                        // quantity as zero, which turns a cancelled order that
                        // caught a partial fill into a plain `Cancelled` — and
                        // the filled part is a real position that must stay
                        // exitable. Unknown is the honest answer.
                        state: OrderState::Unknown,
                        filled_qty: Decimal::ZERO,
                        avg_fill_price: None,
                        fees: Decimal::ZERO,
                    });
                }
            }
        }

        if outside > 0 {
            warn!(
                venue = %self.id,
                orders = outside,
                "Binance.US has resting orders outside the configured universe — not reported, \
                 since the orphan audit would read them as drift and halt"
            );
        }
        Ok(acks)
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn positions(&self) -> Result<Vec<Position>> {
        // Spot crypto has no position endpoint: a position *is* a non-zero
        // balance in the base asset.
        let account = self.account().await?;
        let cash = self.cash_ccy().to_string();

        let mut out = Vec::new();
        let mut outside = Vec::new();
        for balance in &account.balances {
            let asset = balance.asset.to_uppercase();
            if asset == cash {
                continue;
            }

            // Only what the agent actually trades. A Binance.US account is a
            // personal wallet as well as an agent account, and reporting a
            // holding the ledger has no record of is an `UntilResume` halt
            // needing a human.
            //
            // Checked *before* parsing: an account can hold dust in dozens of
            // assets, and letting one unparseable amount fail the whole call
            // would blind the audit over a row that was going to be discarded
            // anyway.
            let Some(symbol) = self.symbol_for_base(&asset) else {
                outside.push(asset);
                continue;
            };

            // Free *and* locked: coins held against a resting order are still
            // the account's position, and omitting them would read as drift
            // the moment an exit order rests.
            let qty = balance.total_amount()?;
            if qty <= Decimal::ZERO {
                continue;
            }

            out.push(Position {
                instrument: InstrumentId::new(self.id.clone(), symbol),
                qty,
                // Binance reports no cost basis on the account endpoint. Zero
                // would claim the position was free and make every P&L wrong;
                // the reconciler compares quantities.
                avg_entry: Decimal::ZERO,
            });
        }

        if !outside.is_empty() {
            warn!(
                venue = %self.id,
                holdings = %outside.join(", "),
                "Binance.US holds balances outside the configured universe — reported here \
                 only, since the reconciler would otherwise read them as drift and halt"
            );
        }
        Ok(out)
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn balance(&self) -> Result<Balance> {
        // Cash only, and therefore no quotes: the bankroll and the survival
        // ladder ask several times a cycle, and `shutdown` asks while trying
        // to exit.
        let (available, _, _) = self.cash_and_holdings().await?;
        Ok(Balance {
            ccy: self.cash_ccy().to_string(),
            available,
        })
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn equity(&self) -> Result<Option<Decimal>> {
        let (_, cash_total, holdings) = self.cash_and_holdings().await?;
        let ccy = self.cash_ccy().to_string();
        Ok(mark_equity(self, &self.id, &ccy, cash_total, &holdings).await)
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn settlement(&self, _id: &InstrumentId) -> Result<Option<Settlement>> {
        // Spot crypto never settles; positions are closed by trading out.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use serde_json::{json, Value};
    use wiremock::matchers::{
        header, method as http_method, path, query_param, query_param_is_missing,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn venue(server: &MockServer, symbols: &[&str]) -> BinanceUsVenue {
        BinanceUsVenue::new(
            BinanceUsConfig::new("api-key", "secret-key")
                .with_base_url(server.uri())
                .with_symbols(symbols.iter().map(|s| s.to_string()).collect()),
        )
        .expect("venue builds")
    }

    fn btc_symbol() -> Value {
        json!({
            "symbol": "BTCUSD",
            "status": "TRADING",
            "baseAsset": "BTC",
            "quoteAsset": "USD",
            "isSpotTradingAllowed": true,
            "filters": [
                {"filterType": "PRICE_FILTER", "tickSize": "0.01"},
                {"filterType": "LOT_SIZE", "minQty": "0.00001", "stepSize": "0.00000100"},
                {"filterType": "NOTIONAL", "minNotional": "10.00"}
            ]
        })
    }

    fn instrument() -> Instrument {
        Instrument {
            id: InstrumentId::new(VenueId::new(DEFAULT_VENUE_ID), "BTC/USD"),
            asset_class: AssetClass::CryptoSpot,
            display_name: "BTC/USD".to_string(),
            quote_ccy: "USD".to_string(),
            tick_size: Some(dec!(0.01)),
            lot_size: Some(dec!(0.00000100)),
            min_notional: Some(dec!(10.00)),
            min_qty: Some(dec!(0.00001)),
            fractional: true,
            meta: InstrumentMeta::Spot {
                base: "BTC".to_string(),
            },
        }
    }

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

    async fn mount_account(server: &MockServer, body: Value) {
        Mock::given(http_method("GET"))
            .and(path(ACCOUNT_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    // ---- the composite order id -----------------------------------------

    /// Binance identifies an order by `(symbol, orderId)`, but the trait and
    /// the ledger carry one string. Losing the symbol means no cancel and no
    /// lookup is possible at all.
    #[test]
    fn an_order_key_round_trips_through_its_string_form() {
        let key = OrderKey::new("BTCUSD", 123456);
        assert_eq!(key.to_string(), "BTCUSD:123456");
        assert_eq!(OrderKey::parse("BTCUSD:123456").unwrap(), key);
        assert_eq!(OrderKey::parse("btcusd:123456").unwrap().symbol, "BTCUSD");
    }

    #[test]
    fn a_bare_order_id_is_refused_with_an_explanation() {
        let err = OrderKey::parse("123456").unwrap_err();
        assert!(
            format!("{err:#}").contains("SYMBOL"),
            "the error has to say what the shape is: {err:#}"
        );
        assert!(OrderKey::parse("BTCUSD:").is_err());
        assert!(OrderKey::parse(":123").is_err());
        assert!(OrderKey::parse("BTCUSD:abc").is_err());
    }

    // ---- instruments -----------------------------------------------------

    #[tokio::test]
    async fn the_symbol_filters_become_instrument_limits() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(EXCHANGE_INFO_PATH))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"symbols": [btc_symbol()]})),
            )
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let found = venue
            .list_instruments(&ScanFilter::default())
            .await
            .unwrap();

        assert_eq!(found.len(), 1);
        let i = &found[0];
        assert_eq!(i.symbol(), "BTC/USD", "built from baseAsset and quoteAsset");
        assert_eq!(i.tick_size, Some(dec!(0.01)));
        assert_eq!(i.lot_size, Some(dec!(0.00000100)));
        // Both floors bind at micro capital, and Binance gates on each.
        assert_eq!(i.min_notional, Some(dec!(10.00)));
        assert_eq!(i.min_qty, Some(dec!(0.00001)));
    }

    /// `BTCUSD` could split as `BTC/USD` or `BT/CUSD`. The pair always comes
    /// from Binance's own assets, never from string surgery.
    #[tokio::test]
    async fn the_pair_comes_from_binances_assets_not_from_splitting_the_name() {
        let server = MockServer::start().await;
        let mut odd = btc_symbol();
        odd["symbol"] = json!("ETHBTC");
        odd["baseAsset"] = json!("ETH");
        odd["quoteAsset"] = json!("BTC");
        Mock::given(http_method("GET"))
            .and(path(EXCHANGE_INFO_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"symbols": [odd]})))
            .mount(&server)
            .await;

        let venue = venue(&server, &["ETH/BTC"]);
        let found = venue
            .list_instruments(&ScanFilter::default())
            .await
            .unwrap();
        assert_eq!(found[0].symbol(), "ETH/BTC");
        assert_eq!(found[0].quote_ccy, "BTC");
    }

    /// An order against a halted symbol is a guaranteed rejection that has
    /// already cost a valuation.
    #[tokio::test]
    async fn a_symbol_binance_will_not_trade_is_skipped() {
        let server = MockServer::start().await;
        let mut halted = btc_symbol();
        halted["status"] = json!("BREAK");
        Mock::given(http_method("GET"))
            .and(path(EXCHANGE_INFO_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"symbols": [halted]})))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        assert!(venue
            .list_instruments(&ScanFilter::default())
            .await
            .unwrap()
            .is_empty());
    }

    /// `exchangeInfo?symbols=[...]` fails the **whole** request with `-1121`
    /// if any one symbol is unknown, so one delisted pair would take the rest
    /// of the universe down with it — and Binance never sends the partial
    /// answer the "does not list this symbol" warning was written for.
    #[tokio::test]
    async fn one_delisted_symbol_does_not_take_the_others_down() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(EXCHANGE_INFO_PATH))
            // Unfiltered: a `symbols` parameter would have made this a 400.
            .and(query_param_is_missing("symbols"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"symbols": [btc_symbol()]})),
            )
            .mount(&server)
            .await;

        // MATIC/USD is configured but no longer listed.
        let venue = venue(&server, &["BTC/USD", "MATIC/USD"]);
        let found = venue
            .list_instruments(&ScanFilter::default())
            .await
            .unwrap();

        assert_eq!(found.len(), 1, "BTC keeps trading");
        assert_eq!(found[0].symbol(), "BTC/USD");
    }

    /// Filtering moved to this side, so it still has to actually filter.
    #[tokio::test]
    async fn symbols_the_venue_does_not_trade_are_left_out() {
        let server = MockServer::start().await;
        let mut doge = btc_symbol();
        doge["symbol"] = json!("DOGEUSD");
        doge["baseAsset"] = json!("DOGE");
        Mock::given(http_method("GET"))
            .and(path(EXCHANGE_INFO_PATH))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"symbols": [btc_symbol(), doge]})),
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

    // ---- quotes and candles ---------------------------------------------    // ---- quotes and candles ---------------------------------------------

    #[tokio::test]
    async fn a_quote_reads_the_positional_book_levels() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(DEPTH_PATH))
            .and(query_param("symbol", "BTCUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "lastUpdateId": 1,
                "bids": [["60000.00", "0.5"], ["59999.00", "1.25"]],
                "asks": [["60010.00", "0.3"]]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let quote = venue.quote(&instrument().id).await.unwrap();

        assert_eq!(quote.bid, dec!(60000.00));
        assert_eq!(quote.ask, dec!(60010.00));
        assert_eq!(quote.mid, dec!(60005.00));
        let book = quote.book.as_ref().expect("Binance publishes depth");
        assert_eq!(book.bids.len(), 2);
        assert_eq!(book.bids[1].size, dec!(1.25), "price then quantity");
    }

    #[tokio::test]
    async fn a_one_sided_book_is_an_error_not_a_halved_mid() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(DEPTH_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "bids": [["60000.00", "0.5"]], "asks": []
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let err = venue.quote(&instrument().id).await.unwrap_err();
        assert!(format!("{err:#}").contains("one-sided"), "{err:#}");
    }

    /// Binance serves four-hour bars natively — the interval sent must be the
    /// interval asked for, which is the mistake the Coinbase adapter made.
    #[tokio::test]
    async fn four_hour_candles_ask_binance_for_four_hour_bars() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(KLINES_PATH))
            .and(query_param("interval", "4h"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                [
                    1758441600000i64,
                    "2",
                    "6",
                    "1",
                    "3",
                    "1",
                    1758456000000i64,
                    "0",
                    1,
                    "0",
                    "0",
                    "0"
                ],
                [
                    1758456000000i64,
                    "3",
                    "8",
                    "2",
                    "4",
                    "2",
                    1758470400000i64,
                    "0",
                    1,
                    "0",
                    "0",
                    "0"
                ]
            ])))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let candles = venue
            .candles(&instrument().id, CandleInterval::H4, 10)
            .await
            .unwrap();

        assert_eq!(candles.len(), 2);
        assert!(candles[0].ts < candles[1].ts, "oldest first");
        assert_eq!(candles[0].open, dec!(2));
        assert_eq!(candles[0].close, dec!(3));
        assert_eq!(candles[1].high, dec!(8));
    }

    // ---- orders ----------------------------------------------------------

    #[tokio::test]
    async fn a_limit_order_carries_its_price_size_and_time_in_force() {
        let server = MockServer::start().await;
        Mock::given(http_method("POST"))
            .and(path(ORDER_PATH))
            .and(query_param("symbol", "BTCUSD"))
            .and(query_param("side", "BUY"))
            .and(query_param("type", "LIMIT"))
            .and(query_param("timeInForce", "GTC"))
            .and(query_param("quantity", "0.001"))
            .and(query_param("price", "60000"))
            .and(query_param("newClientOrderId", "cid-1"))
            .and(query_param("newOrderRespType", "FULL"))
            // The key goes in a header, the signature in the query.
            .and(header("X-MBX-APIKEY", "api-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "symbol": "BTCUSD", "orderId": 28, "clientOrderId": "cid-1",
                "status": "NEW", "executedQty": "0.00000000",
                "cummulativeQuoteQty": "0.00000000", "fills": []
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let ack = venue
            .place_order(&request(
                OrderKind::Limit { price: dec!(60000) },
                Side::Buy,
                TimeInForce::Gtc,
            ))
            .await
            .unwrap();

        assert_eq!(
            ack.venue_order_id, "BTCUSD:28",
            "the symbol travels with the id or no cancel is possible"
        );
        assert_eq!(ack.state, OrderState::Accepted);
        assert_eq!(ack.filled_qty, Decimal::ZERO);
        assert_eq!(ack.avg_fill_price, None, "an order id is not a fill");
    }

    /// Binance has no GTD on spot and no day orders on a market that never
    /// closes. Rewriting either into GTC leaves a resting order that fills
    /// later at a price the decision no longer supports.
    #[tokio::test]
    async fn an_unsupported_time_in_force_is_refused_not_rewritten() {
        let server = MockServer::start().await;
        let venue = venue(&server, &["BTC/USD"]);
        for tif in [TimeInForce::Day, TimeInForce::Gtd(Utc::now())] {
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

    /// Unlike Coinbase, Binance sizes a market order in the base asset on both
    /// sides, so a market buy needs no price to express.
    #[tokio::test]
    async fn a_market_buy_is_sized_in_the_base_asset() {
        let server = MockServer::start().await;
        Mock::given(http_method("POST"))
            .and(path(ORDER_PATH))
            .and(query_param("type", "MARKET"))
            .and(query_param("quantity", "0.001"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "symbol": "BTCUSD", "orderId": 29, "status": "FILLED",
                "executedQty": "0.001", "cummulativeQuoteQty": "60.00",
                "fills": [{"price": "60000.00", "qty": "0.001",
                           "commission": "0.06", "commissionAsset": "USD"}]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let ack = venue
            .place_order(&request(OrderKind::Market, Side::Buy, TimeInForce::Ioc))
            .await
            .unwrap();
        assert_eq!(ack.state, OrderState::Filled);
        assert_eq!(ack.filled_qty, dec!(0.001));
    }

    /// Binance reports no average price, only a cumulative quote value.
    /// Reading that as a price would book a $60 fill at $60 a coin.
    #[tokio::test]
    async fn the_average_price_is_derived_from_the_cumulative_quote_value() {
        let server = MockServer::start().await;
        Mock::given(http_method("POST"))
            .and(path(ORDER_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "symbol": "BTCUSD", "orderId": 30, "status": "FILLED",
                "executedQty": "0.002", "cummulativeQuoteQty": "120.50",
                "fills": []
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let ack = venue
            .place_order(&request(
                OrderKind::Limit { price: dec!(60250) },
                Side::Buy,
                TimeInForce::Gtc,
            ))
            .await
            .unwrap();
        assert_eq!(ack.avg_fill_price, Some(dec!(60250)), "120.50 / 0.002");
    }

    #[test]
    fn nothing_filled_means_no_average_price_rather_than_a_division() {
        let order: Order = serde_json::from_value(json!({
            "orderId": 1, "status": "NEW",
            "executedQty": "0", "cummulativeQuoteQty": "0"
        }))
        .unwrap();
        assert_eq!(to_ack(&order, "BTCUSD").unwrap().avg_fill_price, None);
    }

    #[test]
    fn a_cancel_that_caught_a_partial_fill_is_not_simply_cancelled() {
        // The filled part is a real position and must stay exitable.
        let order: Order = serde_json::from_value(json!({
            "orderId": 1, "status": "CANCELED", "executedQty": "0.0004"
        }))
        .unwrap();
        assert_eq!(to_order_state(&order), OrderState::PartiallyFilled);
    }

    #[test]
    fn a_pending_cancel_is_still_live() {
        // It can still fill; treating it as gone abandons a live order.
        let order: Order = serde_json::from_value(json!({
            "orderId": 1, "status": "PENDING_CANCEL", "executedQty": "0"
        }))
        .unwrap();
        assert_eq!(to_order_state(&order), OrderState::Accepted);
    }

    #[test]
    fn an_unrecognised_status_is_unknown_rather_than_a_guess() {
        let order: Order = serde_json::from_value(json!({
            "orderId": 1, "status": "SOMETHING_NEW"
        }))
        .unwrap();
        assert_eq!(to_order_state(&order), OrderState::Unknown);
    }

    /// The filters are parsed into the `Instrument` and then have to be
    /// *used*. An exit sized from a spot balance (0.00456789 against a
    /// 0.00001 step) comes back `-1013 Filter failure: LOT_SIZE` and the
    /// position stays open through the next drawdown.
    #[tokio::test]
    async fn the_quantity_and_price_are_rounded_to_the_venues_filters() {
        let server = MockServer::start().await;
        Mock::given(http_method("POST"))
            .and(path(ORDER_PATH))
            .and(query_param("quantity", "0.004567"))
            .and(query_param("price", "60000.01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "symbol": "BTCUSD", "orderId": 40, "status": "NEW",
                "executedQty": "0", "cummulativeQuoteQty": "0"
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let mut req = request(
            OrderKind::Limit {
                price: dec!(60000.019),
            },
            Side::Buy,
            TimeInForce::Gtc,
        );
        req.qty = dec!(0.00456789);
        venue
            .place_order(&req)
            .await
            .expect("rounded to step 0.000001 and tick 0.01");
    }

    /// Rounding down can cross the floor, so it is checked after.
    #[tokio::test]
    async fn an_order_that_rounds_below_the_minimum_is_refused_here() {
        let server = MockServer::start().await;
        let venue = venue(&server, &["BTC/USD"]);
        let mut req = request(
            OrderKind::Limit { price: dec!(60000) },
            Side::Buy,
            TimeInForce::Gtc,
        );
        // Below the 0.00001 minQty once rounded.
        req.qty = dec!(0.000004);
        let err = venue.place_order(&req).await.unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("rounds to zero") || rendered.contains("minimum order size"),
            "{rendered}"
        );
    }

    /// Binance gates on order *value* as well as size, and at micro capital
    /// that floor is the one that binds.
    #[tokio::test]
    async fn an_order_below_the_minimum_notional_is_refused_here() {
        let server = MockServer::start().await;
        let venue = venue(&server, &["BTC/USD"]);
        let mut req = request(
            OrderKind::Limit { price: dec!(60000) },
            Side::Buy,
            TimeInForce::Gtc,
        );
        // 0.0001 x 60000 = $6, under the $10 NOTIONAL filter.
        req.qty = dec!(0.0001);
        let err = venue.place_order(&req).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("below Binance.US's minimum"),
            "{err:#}"
        );
    }

    // ---- commissions -----------------------------------------------------

    /// A buy is commissioned in the base asset. Adding that to a dollar fee
    /// adds bitcoin to dollars; it is converted at the price of the trade that
    /// incurred it.
    #[tokio::test]
    async fn a_base_asset_commission_is_converted_at_the_fill_price() {
        let server = MockServer::start().await;
        Mock::given(http_method("POST"))
            .and(path(ORDER_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "symbol": "BTCUSD", "orderId": 31, "status": "FILLED",
                "executedQty": "0.001", "cummulativeQuoteQty": "60.00",
                "fills": [{"price": "60000.00", "qty": "0.001",
                           "commission": "0.000001", "commissionAsset": "BTC"}]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let ack = venue
            .place_order(&request(
                OrderKind::Limit { price: dec!(60000) },
                Side::Buy,
                TimeInForce::Gtc,
            ))
            .await
            .unwrap();
        assert_eq!(ack.fees, dec!(0.06), "0.000001 BTC at 60000 is $0.06");
    }

    /// A limit order that rests and fills later is read back by the
    /// reconciler, and `GET /order` carries no commissions — so without this
    /// every such fill books zero fees and understates the round trip the
    /// sizing gate exists to weigh.
    #[tokio::test]
    async fn a_later_fill_reads_its_commission_from_the_trade_history() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(ORDER_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "symbol": "BTCUSD", "orderId": 32, "clientOrderId": "cid-1",
                "status": "FILLED", "executedQty": "0.001",
                "cummulativeQuoteQty": "60.00"
            })))
            .mount(&server)
            .await;
        Mock::given(http_method("GET"))
            .and(path(MY_TRADES_PATH))
            .and(query_param("orderId", "32"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"price": "60000.00", "qty": "0.001",
                 "commission": "0.09", "commissionAsset": "USD"}
            ])))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let ack = venue
            .get_order(&OrderRef::Venue("BTCUSD:32".to_string()))
            .await
            .unwrap();
        assert_eq!(ack.filled_qty, dec!(0.001));
        assert_eq!(ack.fees, dec!(0.09), "read from myTrades, not assumed zero");
    }

    /// The fill is real whether or not its fee could be read. Dropping it
    /// would leave a position the ledger does not know about, which is far
    /// worse than an understated cost.
    #[tokio::test]
    async fn a_fill_is_still_recorded_when_the_commission_cannot_be_read() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(ORDER_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "symbol": "BTCUSD", "orderId": 33, "status": "FILLED",
                "executedQty": "0.001", "cummulativeQuoteQty": "60.00"
            })))
            .mount(&server)
            .await;
        Mock::given(http_method("GET"))
            .and(path(MY_TRADES_PATH))
            .respond_with(ResponseTemplate::new(500).set_body_string("upstream is down"))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let ack = venue
            .get_order(&OrderRef::Venue("BTCUSD:33".to_string()))
            .await
            .expect("the fill must survive a missing fee");
        assert_eq!(ack.filled_qty, dec!(0.001));
        assert_eq!(ack.fees, Decimal::ZERO);
    }

    /// `BTCUSD` could split as `BTC/USD` or `BT/CUSD`, so which side a
    /// commission was charged on cannot come from string surgery: guessing a
    /// quote fee for a base one multiplies it by the fill price — a 60000x
    /// overstatement on a BTC pair.
    #[tokio::test]
    async fn the_commission_side_comes_from_the_configured_pair() {
        let server = MockServer::start().await;
        Mock::given(http_method("POST"))
            .and(path(ORDER_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "symbol": "ETHBTC", "orderId": 41, "status": "FILLED",
                "executedQty": "1", "cummulativeQuoteQty": "0.05",
                // Charged in BTC — the QUOTE asset of ETH/BTC, though "BTC"
                // is also a prefix of nothing here and a suffix of the name.
                "fills": [{"price": "0.05", "qty": "1",
                           "commission": "0.0001", "commissionAsset": "BTC"}]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["ETH/BTC"]);
        let mut req = request(
            OrderKind::Limit { price: dec!(0.05) },
            Side::Buy,
            TimeInForce::Gtc,
        );
        req.instrument.id = InstrumentId::new(VenueId::new(DEFAULT_VENUE_ID), "ETH/BTC");
        req.instrument.min_notional = None;
        req.instrument.min_qty = None;
        req.qty = dec!(1);
        let ack = venue.place_order(&req).await.unwrap();
        assert_eq!(
            ack.fees,
            dec!(0.0001),
            "BTC is the quote here, so the fee is already in quote terms"
        );
    }

    // ---- cancels ---------------------------------------------------------

    #[tokio::test]
    async fn cancelling_sends_the_symbol_binance_requires() {
        let server = MockServer::start().await;
        Mock::given(http_method("DELETE"))
            .and(path(ORDER_PATH))
            .and(query_param("symbol", "BTCUSD"))
            .and(query_param("orderId", "28"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "symbol": "BTCUSD", "orderId": 28, "status": "CANCELED"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        venue.cancel_order("BTCUSD:28").await.unwrap();
    }

    /// Binance answers a refused cancel with a 4xx and a code, so unlike
    /// Coinbase the failure is in the status — but it must still not read as
    /// success, or the reconciler writes off a live order.
    #[tokio::test]
    async fn a_refused_cancel_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(http_method("DELETE"))
            .and(path(ORDER_PATH))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "code": -2011, "msg": "Unknown order sent."
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let err = venue.cancel_order("BTCUSD:28").await.unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("-2011"),
            "the code is the actionable part: {rendered}"
        );
        assert!(rendered.contains("Unknown order sent"), "{rendered}");
    }

    /// The kill switch: one symbol failing must not cost the cancels for the
    /// others.
    #[tokio::test]
    async fn cancel_all_clears_every_symbol_it_can() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(OPEN_ORDERS_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"symbol": "BTCUSD", "orderId": 1, "status": "NEW", "executedQty": "0"},
                {"symbol": "BTCUSD", "orderId": 2, "status": "NEW", "executedQty": "0"},
                {"symbol": "ETHUSD", "orderId": 3, "status": "NEW", "executedQty": "0"}
            ])))
            .mount(&server)
            .await;
        Mock::given(http_method("DELETE"))
            .and(path(OPEN_ORDERS_PATH))
            .and(query_param("symbol", "ETHUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(http_method("DELETE"))
            .and(path(OPEN_ORDERS_PATH))
            .and(query_param("symbol", "BTCUSD"))
            .respond_with(ResponseTemplate::new(500).set_body_string("down"))
            .expect(1)
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD", "ETH/USD"]);
        let err = venue.cancel_all().await.unwrap_err();
        assert!(
            format!("{err:#}").contains("may still be resting"),
            "the caller has to know the book is not flat: {err:#}"
        );
        // ETHUSD's `expect(1)` is the other half: it cancelled what it could,
        // and asked once per symbol rather than once per order.
    }

    /// `cancel_all` starts here, so one unparseable field must not cost the
    /// cancels for the whole book.
    #[tokio::test]
    async fn one_malformed_order_does_not_stop_the_kill_switch() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(OPEN_ORDERS_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"symbol": "BTCUSD", "orderId": 1, "status": "NEW", "executedQty": "not-a-number"},
                {"symbol": "BTCUSD", "orderId": 2, "status": "NEW", "executedQty": "0"}
            ])))
            .mount(&server)
            .await;
        Mock::given(http_method("DELETE"))
            .and(path(OPEN_ORDERS_PATH))
            .and(query_param("symbol", "BTCUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let open = venue.open_orders().await.unwrap();
        assert_eq!(
            open.len(),
            2,
            "the malformed order is kept so it can be cancelled"
        );
        venue.cancel_all().await.expect("the book is still cleared");
    }

    /// The kill switch must not need a listing to work. Deriving its symbols
    /// from `open_orders()` made a failed listing abort before a single
    /// cancel — the exact failure it is supposed to be the answer to.
    #[tokio::test]
    async fn cancel_all_works_without_listing_anything() {
        let server = MockServer::start().await;
        // Deliberately no GET /openOrders mock: calling it would 404 here.
        Mock::given(http_method("DELETE"))
            .and(path(OPEN_ORDERS_PATH))
            .and(query_param("symbol", "BTCUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        venue.cancel_all().await.expect("no listing is required");
    }

    /// `DELETE /openOrders` cancels a whole symbol's book. Taking the symbol
    /// list from the account would have the kill switch cancel the operator's
    /// own resting orders on pairs the agent has no mandate over.
    #[tokio::test]
    async fn cancel_all_touches_only_the_configured_symbols() {
        let server = MockServer::start().await;
        Mock::given(http_method("DELETE"))
            .and(path(OPEN_ORDERS_PATH))
            .and(query_param("symbol", "BTCUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;
        // Any request for another symbol would not match a mock and fail the
        // call, so a mandate breach shows up as an error here.
        let venue = venue(&server, &["BTC/USD"]);
        venue
            .cancel_all()
            .await
            .expect("only BTCUSD is the agent's");
    }

    /// Binance answers an empty book with `-2011`. Reading that as a failure
    /// would have the kill switch report a book it did in fact clear.
    #[tokio::test]
    async fn an_empty_book_is_not_a_failed_cancel() {
        let server = MockServer::start().await;
        Mock::given(http_method("DELETE"))
            .and(path(OPEN_ORDERS_PATH))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "code": -2011, "msg": "Unknown order sent."
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        venue.cancel_all().await.expect("nothing was resting");
    }

    /// The orphan audit turns a resting order it has no local record of into
    /// an `UntilResume` halt — so an operator's own manual order on a pair
    /// the agent does not trade would halt it on the first cycle. The same
    /// trap `positions()` guards, which this originally did not.
    #[tokio::test]
    async fn open_orders_ignores_pairs_the_agent_does_not_trade() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(OPEN_ORDERS_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"symbol": "SOLUSD", "orderId": 99, "clientOrderId": "web_7f2",
                 "status": "NEW", "executedQty": "0"},
                {"symbol": "BTCUSD", "orderId": 1, "status": "NEW", "executedQty": "0"}
            ])))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let open = venue.open_orders().await.unwrap();
        assert_eq!(open.len(), 1, "the personal SOL order is not the agent's");
        assert_eq!(open[0].venue_order_id, "BTCUSD:1");
    }

    /// `to_order_state` reads an unparseable quantity as zero, which turns a
    /// cancelled order that caught a partial fill into a plain `Cancelled` —
    /// and the filled part is a real position that must stay exitable.
    #[tokio::test]
    async fn an_order_whose_quantity_will_not_parse_is_unknown_not_cancelled() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(OPEN_ORDERS_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"symbol": "BTCUSD", "orderId": 1, "status": "CANCELED",
                 "executedQty": "not-a-number"}
            ])))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let open = venue.open_orders().await.unwrap();
        assert_eq!(open.len(), 1, "it is kept so it can still be cancelled");
        assert_eq!(
            open[0].state,
            OrderState::Unknown,
            "reading the quantity as zero would write off a real position"
        );
    }

    // ---- balances and positions -----------------------------------------    // ---- balances and positions -----------------------------------------

    #[tokio::test]
    async fn balance_counts_free_cash_in_the_configured_quote_currency() {
        let server = MockServer::start().await;
        mount_account(
            &server,
            json!({
                "canTrade": true,
                "balances": [
                    {"asset": "USD",  "free": "120.50", "locked": "30.00"},
                    {"asset": "USDT", "free": "900.00", "locked": "0"},
                    {"asset": "BTC",  "free": "0.01",   "locked": "0"}
                ]
            }),
        )
        .await;

        Mock::given(http_method("GET"))
            .and(path(DEPTH_PATH))
            .and(query_param("symbol", "BTCUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "bids": [["59990.00", "1"]], "asks": [["60010.00", "1"]]
            })))
            .mount(&server)
            .await;
        // The USDT is not this venue's cash, so it is a holding like any other
        // and has to be priced rather than assumed to be a dollar.
        Mock::given(http_method("GET"))
            .and(path(DEPTH_PATH))
            .and(query_param("symbol", "USDTUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "bids": [["0.9999", "1"]], "asks": [["1.0001", "1"]]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let balance = venue.balance().await.unwrap();

        assert_eq!(
            balance.available,
            dec!(120.50),
            "USD only — this account trades BTC/USD, and locked cash is committed"
        );
        assert_eq!(balance.ccy, "USD");
        assert_eq!(
            venue.equity().await.unwrap(),
            Some(dec!(1650.50)),
            "150.50 cash (free + locked) + 0.01 BTC at 60000 + 900 USDT at 1.00"
        );
    }

    /// An account trading BTC/USDT holds its cash in USDT. Counting USD as
    /// well would report money that cannot be spent on the pairs traded.
    #[tokio::test]
    async fn the_cash_currency_follows_the_configured_pairs() {
        let server = MockServer::start().await;
        mount_account(
            &server,
            json!({
                "balances": [
                    {"asset": "USD",  "free": "500.00"},
                    {"asset": "USDT", "free": "42.00"}
                ]
            }),
        )
        .await;

        let venue = venue(&server, &["BTC/USDT"]);
        let balance = venue.balance().await.unwrap();
        assert_eq!(balance.available, dec!(42.00));
        assert_eq!(balance.ccy, "USDT");
    }

    /// Coins held against a resting exit order are still the account's
    /// position. Omitting them reads as drift the moment an exit rests.
    #[tokio::test]
    async fn a_position_includes_what_is_locked_against_a_resting_order() {
        let server = MockServer::start().await;
        mount_account(
            &server,
            json!({
                "balances": [
                    {"asset": "USD", "free": "100"},
                    {"asset": "BTC", "free": "0.004", "locked": "0.006"}
                ]
            }),
        )
        .await;

        let venue = venue(&server, &["BTC/USD"]);
        let positions = venue.positions().await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].instrument.symbol, "BTC/USD");
        assert_eq!(positions[0].qty, dec!(0.01), "free plus locked");
    }

    /// A Binance.US account is a personal wallet too. Reporting a holding the
    /// ledger has no record of is an `UntilResume` halt needing a human, so
    /// anyone enabling this venue on an account they already use would halt on
    /// the first cycle.
    #[tokio::test]
    async fn a_holding_outside_the_configured_universe_is_not_a_position() {
        let server = MockServer::start().await;
        mount_account(
            &server,
            json!({
                "balances": [
                    {"asset": "SOL", "free": "2"},
                    {"asset": "BNB", "free": "1"},
                    {"asset": "BTC", "free": "0.01"},
                    {"asset": "ETH", "free": "0"}
                ]
            }),
        )
        .await;

        let venue = venue(&server, &["BTC/USD"]);
        let positions = venue.positions().await.unwrap();
        assert_eq!(
            positions.len(),
            1,
            "only what the agent trades, and not a zero"
        );
        assert_eq!(positions[0].instrument.symbol, "BTC/USD");
    }

    /// Summing USD and USDT into one figure asserts they are interchangeable
    /// and would tell the sizing gate there are spendable dollars that can in
    /// fact only fund the other pairs. Dropping one instead understates the
    /// account. `Balance` has room for one currency, so the configuration
    /// itself is refused — at construction, where an operator is looking.
    #[test]
    fn a_venue_quoting_in_two_currencies_is_refused_at_construction() {
        let err = BinanceUsVenue::new(
            BinanceUsConfig::new("api-key", "secret-key")
                .with_symbols(vec!["BTC/USD".to_string(), "ETH/USDT".to_string()]),
        )
        .unwrap_err();

        let rendered = format!("{err:#}");
        assert!(rendered.contains("USD and USDT"), "name both: {rendered}");
        assert!(
            rendered.contains("venue entry per quote currency"),
            "and say what to do instead: {rendered}"
        );
    }

    /// The single-currency case is the normal one and must still build.
    #[test]
    fn one_quote_currency_builds() {
        assert!(BinanceUsVenue::new(
            BinanceUsConfig::new("api-key", "secret-key")
                .with_symbols(vec!["BTC/USDT".to_string(), "ETH/USDT".to_string()]),
        )
        .is_ok());
    }

    /// The number every loss limit is measured against. Without it
    /// `check_breakers` halts, so a crypto-only deployment could not trade
    /// live at all.
    #[tokio::test]
    async fn equity_marks_each_holding_at_the_current_mid() {
        let server = MockServer::start().await;
        mount_account(
            &server,
            json!({
                "balances": [
                    {"asset": "USD", "free": "100.00", "locked": "0"},
                    {"asset": "BTC", "free": "0.001",  "locked": "0.001"}
                ]
            }),
        )
        .await;
        Mock::given(http_method("GET"))
            .and(path(DEPTH_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "bids": [["60000.00", "1"]], "asks": [["60010.00", "1"]]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let balance = venue.balance().await.unwrap();

        assert_eq!(balance.available, dec!(100.00));
        // 0.002 BTC (free + locked) at a 60005 mid = 120.01.
        assert_eq!(
            venue.equity().await.unwrap(),
            Some(dec!(220.01)),
            "cash plus the marked holding, locked coins included"
        );
    }

    /// A partial equity is a wrong equity, and the drawdown breaker cannot
    /// tell one from a real loss — a missing position reads exactly like a
    /// position that went to zero.
    #[tokio::test]
    async fn equity_is_unknown_rather_than_partial_when_a_holding_cannot_be_priced() {
        let server = MockServer::start().await;
        mount_account(
            &server,
            json!({
                "balances": [
                    {"asset": "USD", "free": "100.00"},
                    {"asset": "BTC", "free": "0.01"}
                ]
            }),
        )
        .await;
        Mock::given(http_method("GET"))
            .and(path(DEPTH_PATH))
            .respond_with(ResponseTemplate::new(500).set_body_string("down"))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let balance = venue.balance().await.unwrap();
        assert_eq!(balance.available, dec!(100.00), "cash is still readable");
        assert_eq!(
            venue.equity().await.unwrap(),
            None,
            "reporting 100 here would book the whole position as an instant loss"
        );
    }

    /// Cash and equity are projections of one payload, so they must come from
    /// one snapshot. Two fetches let a fill land between them — free cash from
    /// before it, the equity figure from after — so `available` can exceed the
    /// cash inside equity, which a single response made impossible.
    #[tokio::test]
    async fn balance_and_equity_share_one_account_snapshot() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(ACCOUNT_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "canTrade": true,
                "balances": [{"asset": "USD", "free": "250.00", "locked": "0"}]
            })))
            // Asserted on drop.
            .expect(1)
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        assert_eq!(venue.balance().await.unwrap().available, dec!(250.00));
        assert_eq!(venue.equity().await.unwrap(), Some(dec!(250.00)));
    }

    #[tokio::test]
    async fn the_account_snapshot_is_refetched_once_it_expires() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(ACCOUNT_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "canTrade": true,
                "balances": [{"asset": "USD", "free": "250.00"}]
            })))
            .expect(2)
            .mount(&server)
            .await;

        let venue = BinanceUsVenue::new(
            BinanceUsConfig::new("api-key", "secret-key")
                .with_base_url(server.uri())
                .with_symbols(vec!["BTC/USD".to_string()])
                .with_account_cache_ttl(Duration::ZERO),
        )
        .unwrap();
        venue.balance().await.unwrap();
        venue.equity().await.unwrap();
    }

    /// The whole point of the split: `balance()` must price nothing.
    #[tokio::test]
    async fn balance_makes_no_quote_calls_even_with_holdings() {
        let server = MockServer::start().await;
        mount_account(
            &server,
            json!({
                "balances": [
                    {"asset": "USD", "free": "100.00"},
                    {"asset": "BTC", "free": "0.5"},
                    {"asset": "ETH", "free": "3"}
                ]
            }),
        )
        .await;
        Mock::given(http_method("GET"))
            .and(path(DEPTH_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "bids": [["1", "1"]], "asks": [["1", "1"]]
            })))
            // Asserted on drop: not one book call belongs in a cash check.
            .expect(0)
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        assert_eq!(venue.balance().await.unwrap().available, dec!(100.00));
    }

    /// Cash in another currency, and coins the agent does not trade, are both
    /// still the account's money. Dropping them reports a partial equity as
    /// authoritative — the review proved a $542 account reporting $42.
    #[tokio::test]
    async fn equity_prices_everything_the_account_holds() {
        let server = MockServer::start().await;
        mount_account(
            &server,
            json!({
                "balances": [
                    {"asset": "USDT", "free": "42.00"},
                    {"asset": "USD",  "free": "500.00"},
                    {"asset": "SOL",  "free": "2"}
                ]
            }),
        )
        .await;
        Mock::given(http_method("GET"))
            .and(path(DEPTH_PATH))
            .and(query_param("symbol", "USDUSDT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "bids": [["0.9999", "1"]], "asks": [["1.0001", "1"]]
            })))
            .mount(&server)
            .await;
        Mock::given(http_method("GET"))
            .and(path(DEPTH_PATH))
            .and(query_param("symbol", "SOLUSDT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "bids": [["149.00", "1"]], "asks": [["151.00", "1"]]
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USDT"]);
        let balance = venue.balance().await.unwrap();

        assert_eq!(balance.ccy, "USDT");
        assert_eq!(
            balance.available,
            dec!(42.00),
            "only USDT funds a USDT pair"
        );
        // 42 + (500 x 1.0) + (2 x 150) = 842
        assert_eq!(
            venue.equity().await.unwrap(),
            Some(dec!(842.00)),
            "the USD cash and the SOL are the account's too"
        );
    }

    /// A closed position leaves billionths of a coin behind. Demanding a quote
    /// for that would let a failed book call over a fraction of a cent blank
    /// the account's equity and halt the agent.
    #[tokio::test]
    async fn dust_does_not_need_a_quote_and_cannot_blank_equity() {
        let server = MockServer::start().await;
        mount_account(
            &server,
            json!({
                "balances": [
                    {"asset": "USD", "free": "1000.00"},
                    {"asset": "BTC", "free": "0.00000001"}
                ]
            }),
        )
        .await;
        // No depth mock at all: asking for one would fail the call.

        let venue = venue(&server, &["BTC/USD"]);
        assert_eq!(venue.equity().await.unwrap(), Some(dec!(1000.00)));
    }

    /// An account can hold dust in dozens of assets. One unparseable amount    /// An account can hold dust in dozens of assets. One unparseable amount
    /// must not fail the whole call and blind the audit — least of all for a
    /// row that was going to be discarded anyway.
    #[tokio::test]
    async fn a_junk_balance_outside_the_universe_does_not_blind_the_audit() {
        let server = MockServer::start().await;
        mount_account(
            &server,
            json!({
                "balances": [
                    {"asset": "JUNK", "free": "not-a-number"},
                    {"asset": "BTC",  "free": "0.01"}
                ]
            }),
        )
        .await;

        let venue = venue(&server, &["BTC/USD"]);
        let positions = venue
            .positions()
            .await
            .expect("the BTC position is readable");
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].qty, dec!(0.01));
    }

    /// A restricted account still answers `/account` with balances, so without
    /// this every order is rejected one at a time, each having already spent a
    /// valuation.
    #[tokio::test]
    async fn an_account_that_cannot_trade_fails_readiness() {
        let server = MockServer::start().await;
        mount_account(&server, json!({"canTrade": false, "balances": []})).await;

        let venue = venue(&server, &["BTC/USD"]);
        let err = venue.trading_readiness().await.unwrap_err();
        assert!(format!("{err:#}").contains("cannot trade"), "{err:#}");
    }

    #[tokio::test]
    async fn an_account_that_can_trade_passes_readiness() {
        let server = MockServer::start().await;
        mount_account(&server, json!({"canTrade": true, "balances": []})).await;
        venue(&server, &["BTC/USD"])
            .trading_readiness()
            .await
            .expect("a normal account is ready");
    }

    /// Binance omits `canTrade` on some responses, and absence is not a
    /// restriction — refusing to trade on a missing field would be an outage.
    #[tokio::test]
    async fn a_missing_can_trade_flag_is_not_a_restriction() {
        let server = MockServer::start().await;
        mount_account(&server, json!({"balances": []})).await;
        venue(&server, &["BTC/USD"])
            .trading_readiness()
            .await
            .expect("absence is not a signal");
    }

    /// `-1021` is a local clock problem. Without the hint an operator reads
    /// "timestamp outside recvWindow" and checks the network.
    #[tokio::test]
    async fn a_clock_skew_error_says_it_is_the_clock() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(ACCOUNT_PATH))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "code": -1021,
                "msg": "Timestamp for this request is outside of the recvWindow."
            })))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let err = venue.balance().await.unwrap_err();
        assert!(format!("{err:#}").contains("clock"), "{err:#}");
    }

    /// The query string carries the signature, and errors reach logs.
    #[tokio::test]
    async fn an_error_response_never_carries_the_signature() {
        let server = MockServer::start().await;
        Mock::given(http_method("GET"))
            .and(path(ACCOUNT_PATH))
            .respond_with(ResponseTemplate::new(418).set_body_string("teapot"))
            .mount(&server)
            .await;

        let venue = venue(&server, &["BTC/USD"]);
        let err = venue.balance().await.unwrap_err();
        let rendered = format!("{err:#}");
        assert!(!rendered.contains("signature"), "{rendered}");
        assert!(!rendered.contains("api-key"), "{rendered}");
    }

    /// A response that stops early must be an error, not an empty body that
    /// falls through to "returned an unexpected body:" with nothing after the
    /// colon — and it must not carry the signature either.
    ///
    /// Note that reqwest surfaces this at `send`, not at the body read, so
    /// the `without_url` on the body read itself stays defensive rather than
    /// exercised: there is no way to make `text()` fail here without holding
    /// the socket open past the client timeout.
    #[tokio::test]
    async fn a_truncated_body_is_reported_without_the_signature() {
        use std::io::Write as _;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                // Promises 500 bytes, sends 5, then hangs up.
                let _ = socket.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                      Content-Length: 500\r\n\r\nshort",
                );
                let _ = socket.flush();
            }
        });

        let venue = BinanceUsVenue::new(
            BinanceUsConfig::new("api-key", "secret-key")
                .with_base_url(format!("http://127.0.0.1:{port}"))
                .with_symbols(vec!["BTC/USD".to_string()]),
        )
        .unwrap();

        let err = venue.balance().await.unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("/api/v3/account"),
            "the error must name the call: {rendered}"
        );
        assert!(!rendered.contains("signature="), "{rendered}");
        assert!(!rendered.contains("timestamp="), "{rendered}");
    }

    /// The harder half, and the one the response test above cannot reach:
    /// when the *transport* fails, reqwest builds the error itself and puts
    /// the URL it was given — signature and all — inside it. Redacting only
    /// the context this code adds leaves the secret one line below it.
    #[tokio::test]
    async fn a_transport_failure_never_carries_the_signature() {
        // A port nothing is listening on, so reqwest fails before any
        // response exists.
        let venue = BinanceUsVenue::new(
            BinanceUsConfig::new("api-key", "secret-key")
                .with_base_url("http://127.0.0.1:1")
                .with_symbols(vec!["BTC/USD".to_string()]),
        )
        .unwrap();

        let err = venue.balance().await.unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            !rendered.contains("signature="),
            "the signed URL must not reach a log line: {rendered}"
        );
        assert!(!rendered.contains("timestamp="), "{rendered}");
        // It must still say which call failed.
        assert!(rendered.contains("/api/v3/account"), "{rendered}");
    }
}
