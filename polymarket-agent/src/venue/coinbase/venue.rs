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

use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use tracing::{info, instrument, warn};

use super::auth::CdpSigner;
use super::models::{
    money, Account, AccountsResponse, CandlesResponse, CreateOrderResponse, Order, OrderResponse,
    OrdersResponse, Product, ProductBookResponse, ProductsResponse,
};
use super::rest::CoinbaseRest;
use crate::market::models::{OrderBookSnapshot, PriceLevel};
use crate::venue::session::TradingSession;
use crate::venue::types::{
    AssetClass, Balance, Candle, CandleInterval, Instrument, InstrumentId, InstrumentMeta,
    OrderAck, OrderKind, OrderRef, OrderRequest, OrderState, Position, Quote, ScanFilter,
    Settlement, Side, VenueCapabilities, VenueId,
};
use crate::venue::Venue;

pub const DEFAULT_VENUE_ID: &str = "coinbase";
pub const BASE_URL: &str = "https://api.coinbase.com";
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Coinbase's page cap for `/products`. Well above any universe this agent
/// trades, but named rather than assumed.
const PRODUCT_PAGE_LIMIT: usize = 1000;

pub struct CoinbaseConfig {
    pub venue_id: VenueId,
    pub base_url: String,
    key_name: String,
    private_key_pem: String,
    pub symbols: Vec<String>,
    pub request_timeout: Duration,
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
}

pub struct CoinbaseVenue {
    id: VenueId,
    caps: VenueCapabilities,
    session: TradingSession,
    symbols: Vec<String>,
    rest: CoinbaseRest,
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

        let wanted: std::collections::HashSet<String> =
            requested.iter().map(|s| Self::to_product_id(s)).collect();

        let response: ProductsResponse = self
            .rest
            .get(
                "/api/v3/brokerage/products",
                &[
                    ("product_type", "SPOT".to_string()),
                    ("limit", PRODUCT_PAGE_LIMIT.to_string()),
                ],
            )
            .await
            .context("Failed to list Coinbase products")?;

        let mut out = Vec::new();
        for product in &response.products {
            if !wanted.contains(&product.product_id.to_uppercase()) {
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

        // Configured symbols Coinbase did not return at all. Silently trading
        // a smaller universe than configured is a quiet way to under-trade.
        for symbol in requested {
            let product_id = Self::to_product_id(symbol);
            if !response
                .products
                .iter()
                .any(|p| p.product_id.eq_ignore_ascii_case(&product_id))
            {
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
        let granularity = match interval {
            CandleInterval::H1 => "ONE_HOUR",
            CandleInterval::H4 => "SIX_HOUR",
            CandleInterval::D1 => "ONE_DAY",
        };

        // Coinbase requires an explicit window; it does not accept a bare
        // count. Asking for exactly `limit` periods back leaves no slack for
        // a missing bar, so this asks for a little more and truncates.
        let seconds = interval.hours() * 3600;
        let end = Utc::now();
        let start = end - chrono::Duration::seconds(seconds * (limit as i64 + 2));

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
        let configuration = match request.kind {
            OrderKind::Limit { price } => serde_json::json!({
                "limit_limit_gtc": {
                    "base_size": request.qty.normalize().to_string(),
                    "limit_price": price.normalize().to_string(),
                    "post_only": false,
                }
            }),
            OrderKind::Market => serde_json::json!({
                "market_market_ioc": {
                    "base_size": request.qty.normalize().to_string(),
                }
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
            .post("/api/v3/brokerage/orders", &payload)
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
                // filters. Bounded by the open set plus recent history, which
                // at a ten-minute cadence is small.
                let response: OrdersResponse = self
                    .rest
                    .get("/api/v3/brokerage/orders/historical/batch", &[])
                    .await
                    .context("Failed to list Coinbase orders")?;
                let found = response
                    .orders
                    .iter()
                    .find(|o| o.client_order_id.as_deref() == Some(client_order_id.as_str()))
                    .with_context(|| {
                        format!("Coinbase has no order with client id {client_order_id}")
                    })?;
                to_ack(found)
            }
        }
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn cancel_order(&self, venue_order_id: &str) -> Result<()> {
        let payload = serde_json::json!({ "order_ids": [venue_order_id] });
        let _: serde_json::Value = self
            .rest
            .post("/api/v3/brokerage/orders/batch_cancel", &payload)
            .await
            .with_context(|| format!("Failed to cancel Coinbase order {venue_order_id}"))?;
        Ok(())
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn cancel_all(&self) -> Result<()> {
        let open = self.open_orders().await?;
        if open.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = open.iter().map(|o| o.venue_order_id.clone()).collect();
        let payload = serde_json::json!({ "order_ids": ids });
        let _: serde_json::Value = self
            .rest
            .post("/api/v3/brokerage/orders/batch_cancel", &payload)
            .await
            .context("Failed to cancel all Coinbase orders")?;
        Ok(())
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn open_orders(&self) -> Result<Vec<OrderAck>> {
        let response: OrdersResponse = self
            .rest
            .get(
                "/api/v3/brokerage/orders/historical/batch",
                &[("order_status", "OPEN".to_string())],
            )
            .await
            .context("Failed to list open Coinbase orders")?;
        response.orders.iter().map(to_ack).collect()
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn positions(&self) -> Result<Vec<Position>> {
        // Spot crypto has no position endpoint: a position *is* a non-zero
        // balance in the base currency. Reported against the quote currency
        // the agent trades, so it lines up with the symbols in the ledger.
        let accounts = self.accounts().await?;
        let mut out = Vec::new();
        for account in &accounts {
            let Some(currency) = account.currency.as_deref() else {
                continue;
            };
            // The quote currency is cash, not a position.
            if currency.eq_ignore_ascii_case("USD") || currency.eq_ignore_ascii_case("USDC") {
                continue;
            }
            let Some(amount) = &account.available_balance else {
                continue;
            };
            let qty = money(&amount.value, "available_balance")?;
            if qty <= Decimal::ZERO {
                continue;
            }
            out.push(Position {
                instrument: InstrumentId::new(self.id.clone(), format!("{currency}/USD")),
                qty,
                // Coinbase does not report a cost basis on the accounts
                // endpoint. Zero would claim the position was free and make
                // every P&L calculation wrong; the reconciler compares
                // quantities, which is what this is for.
                avg_entry: Decimal::ZERO,
            });
        }
        Ok(out)
    }

    #[instrument(skip(self), fields(venue = %self.id))]
    async fn balance(&self) -> Result<Balance> {
        let accounts = self.accounts().await?;

        let mut cash = Decimal::ZERO;
        for account in &accounts {
            let Some(currency) = account.currency.as_deref() else {
                continue;
            };
            if !(currency.eq_ignore_ascii_case("USD") || currency.eq_ignore_ascii_case("USDC")) {
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
    async fn accounts(&self) -> Result<Vec<Account>> {
        let response: AccountsResponse = self
            .rest
            .get(
                "/api/v3/brokerage/accounts",
                &[("limit", "250".to_string())],
            )
            .await
            .context("Failed to fetch Coinbase accounts")?;
        Ok(response.accounts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use serde_json::{json, Value};
    use wiremock::matchers::{body_partial_json, header_exists, method, path, query_param};
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
}
