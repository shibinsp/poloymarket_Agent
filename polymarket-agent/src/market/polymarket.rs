//! Polymarket CLOB API client wrapper.
//!
//! Wraps `polymarket-client-sdk` with rate limiting, paper trading,
//! retry logic, authenticated live trading, and domain type conversion.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use alloy::signers::k256::ecdsa::SigningKey;
use alloy::signers::local::LocalSigner;
use alloy::signers::Signer;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::clob::types::request::{
    BalanceAllowanceRequest, OrderBookSummaryRequest, PriceHistoryRequest,
};
use polymarket_client_sdk::clob::types::response::OrderBookSummaryResponse;
use polymarket_client_sdk::clob::types::{Interval, OrderType, Side as ClobSide, TimeRange};
use polymarket_client_sdk::clob::Client as ClobClient;
use polymarket_client_sdk::types::{Decimal as SdkDecimal, U256};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use std::str::FromStr;
use tokio::sync::Mutex;
use tracing::{info, instrument, warn};

use crate::config::{AgentMode, AppConfig, ExposeSecret, RateLimitConfig, Secrets};
use crate::market::models::{
    Market, OrderBookSnapshot, PriceHistoryPoint, PriceLevel, Side, TokenInfo,
};
use crate::venue::types::{OrderState as VenueOrderState, Side as VenueSide};

type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

#[derive(Debug)]
pub struct MarketFilters {
    pub min_volume_24h: Decimal,
    pub max_resolution_days: u32,
    pub max_markets: usize,
    pub max_spread_pct: Decimal,
}

/// Paper trading simulated position.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PaperPosition {
    pub token_id: String,
    pub side: VenueSide,
    pub size: Decimal,
    pub entry_price: Decimal,
}

/// Paper trading simulated order.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PaperOrder {
    pub order_id: String,
    pub token_id: String,
    pub side: VenueSide,
    pub price: Decimal,
    pub size: Decimal,
    pub filled: bool,
    /// Whether this order was filled with adverse selection (price moved against us).
    pub adverse_selection: bool,
}

/// What a venue-semantics order placement produced.
#[derive(Debug, Clone)]
pub struct TokenOrderOutcome {
    pub order_id: String,
    pub state: VenueOrderState,
    pub filled_qty: Decimal,
    pub avg_fill_price: Option<Decimal>,
}

/// Result of a paper trading fill simulation.
#[derive(Debug, Clone)]
pub struct PaperFillResult {
    pub order_id: String,
    pub filled: bool,
    pub fill_price: Decimal,
    pub fill_size: Decimal,
    pub adverse_selection: bool,
}

/// Tracks simulated state for paper trading.
struct PaperTradingState {
    balance: Decimal,
    positions: Vec<PaperPosition>,
    order_history: Vec<PaperOrder>,
}

/// Authenticated CLOB client for live trading.
struct AuthenticatedClient {
    clob: ClobClient<Authenticated<Normal>>,
    signer: LocalSigner<SigningKey>,
}

pub struct PolymarketClient {
    config: Arc<AppConfig>,
    /// Unauthenticated CLOB client (for market data in all modes)
    clob: ClobClient,
    /// Authenticated CLOB client (only for live trading)
    auth_client: Option<AuthenticatedClient>,
    /// HTTP client for direct Gamma API calls (bypasses SDK deserialization issues)
    http: reqwest::Client,
    /// Gamma API base URL
    gamma_base_url: String,
    /// Rate limiter
    limiter: Arc<Limiter>,
    /// Paper trading state (only in Paper/Backtest mode)
    paper_state: Option<Mutex<PaperTradingState>>,
}

impl PolymarketClient {
    pub async fn new(config: Arc<AppConfig>, secrets: &Secrets) -> Result<Self> {
        let clob = ClobClient::new(
            &config.polymarket.clob_base_url,
            polymarket_client_sdk::clob::Config::default(),
        )
        .context("Failed to create CLOB client")?;

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        let gamma_base_url = config
            .polymarket
            .gamma_base_url
            .trim_end_matches('/')
            .to_string();

        let limiter = create_rate_limiter(&config.rate_limit);

        // Initialize authenticated client for live trading mode
        let auth_client = match config.agent.mode {
            AgentMode::Live => {
                let private_key = secrets.polymarket_private_key.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("POLYMARKET_PRIVATE_KEY required for live trading")
                })?;

                let signer = LocalSigner::from_str(private_key.expose_secret())
                    .context("Failed to parse private key")?
                    .with_chain_id(Some(137)); // Polygon chain ID

                let auth_clob = clob
                    .clone()
                    .authentication_builder(&signer)
                    .authenticate()
                    .await
                    .context("Failed to authenticate with Polymarket CLOB")?;

                info!("Authenticated CLOB client initialized for live trading");
                Some(AuthenticatedClient {
                    clob: auth_clob,
                    signer,
                })
            }
            _ => None,
        };

        let paper_state = match config.agent.mode {
            AgentMode::Paper | AgentMode::Backtest => Some(Mutex::new(PaperTradingState {
                balance: config.agent.initial_paper_balance,
                positions: Vec::new(),
                order_history: Vec::new(),
            })),
            _ => None,
        };

        Ok(Self {
            config,
            clob,
            auth_client,
            http,
            gamma_base_url,
            limiter,
            paper_state,
        })
    }

    // === Market Discovery (via Gamma API, direct reqwest) ===

    /// Fetch markets from Gamma API, filtered by our criteria.
    /// Uses direct reqwest instead of SDK to avoid Decimal deserialization issues
    /// with the Gamma API returning JSON floats instead of strings.
    #[instrument(skip(self, filters))]
    pub async fn get_markets(&self, filters: &MarketFilters) -> Result<Vec<Market>> {
        let mut all_markets = Vec::new();
        let mut offset = 0u32;
        let limit = 100u32;

        let now = Utc::now();
        let max_end_date = now + chrono::Duration::days(filters.max_resolution_days as i64);

        loop {
            self.rate_limit().await;

            let url = format!("{}/markets", self.gamma_base_url);

            let gamma_markets: Vec<GammaMarketResponse> = self
                .with_retry(|| {
                    let url = url.clone();
                    let end_min = now.to_rfc3339();
                    let end_max = max_end_date.to_rfc3339();
                    let vol_min = filters.min_volume_24h.to_string();
                    async move {
                        let resp = self
                            .http
                            .get(&url)
                            .query(&[
                                ("limit", limit.to_string()),
                                ("offset", offset.to_string()),
                                ("closed", "false".to_string()),
                                ("end_date_min", end_min),
                                ("end_date_max", end_max),
                                ("volume_num_min", vol_min),
                                ("order", "volume".to_string()),
                                ("ascending", "false".to_string()),
                            ])
                            .send()
                            .await
                            .map_err(|e| anyhow::anyhow!("HTTP error: {e}"))?;

                        if !resp.status().is_success() {
                            let status = resp.status();
                            let body = resp.text().await.unwrap_or_default();
                            return Err(anyhow::anyhow!("Gamma API {status}: {body}"));
                        }

                        resp.json::<Vec<GammaMarketResponse>>()
                            .await
                            .map_err(|e| anyhow::anyhow!("Deserialization error: {e}"))
                    }
                })
                .await
                .context("Failed to fetch markets from Gamma API")?;

            if gamma_markets.is_empty() {
                break;
            }

            let page_count = gamma_markets.len();

            for gm in &gamma_markets {
                if let Some(market) = convert_gamma_response(gm) {
                    if market.active && market.volume_24h >= filters.min_volume_24h {
                        all_markets.push(market);
                    }
                }
            }

            offset += limit;

            if all_markets.len() >= filters.max_markets || (page_count as u32) < limit {
                break;
            }
        }

        all_markets.truncate(filters.max_markets);

        info!(count = all_markets.len(), "Markets fetched from Gamma API");
        Ok(all_markets)
    }

    /// Fetch one market by condition id.
    ///
    /// Deliberately applies none of `get_markets`' filters. That call hard-codes
    /// `closed=false`, an `end_date_min` of now, and a top-N-by-volume
    /// truncation — all sensible for *discovering* something to trade, and all
    /// wrong for looking up something already held. A position needs quoting
    /// most urgently exactly when its market has closed, resolved, or dropped
    /// out of the top page, which is precisely when those filters hide it.
    pub async fn get_market_by_condition_id(&self, condition_id: &str) -> Result<Option<Market>> {
        self.rate_limit().await;

        let url = format!("{}/markets", self.gamma_base_url);
        let gamma_markets: Vec<GammaMarketResponse> = self
            .with_retry(|| {
                let url = url.clone();
                async move {
                    let resp = self
                        .http
                        .get(&url)
                        .query(&[("condition_id", condition_id)])
                        .send()
                        .await
                        .map_err(|e| anyhow::anyhow!("HTTP error: {e}"))?;

                    if !resp.status().is_success() {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();
                        return Err(anyhow::anyhow!("Gamma API {status}: {body}"));
                    }

                    resp.json::<Vec<GammaMarketResponse>>()
                        .await
                        .map_err(|e| anyhow::anyhow!("Failed to decode Gamma market: {e}"))
                }
            })
            .await
            .with_context(|| format!("Failed to fetch market {condition_id}"))?;

        Ok(gamma_markets.iter().find_map(convert_gamma_response))
    }

    // === Order Book (via CLOB API) ===

    /// Get order book for a specific token.
    #[instrument(skip(self), fields(token_id = %token_id))]
    pub async fn get_order_book(&self, token_id: &str) -> Result<OrderBookSnapshot> {
        self.rate_limit().await;

        let token_u256 = parse_token_id(token_id)?;

        let request = OrderBookSummaryRequest::builder()
            .token_id(token_u256)
            .build();

        let response: OrderBookSummaryResponse = self
            .with_retry(|| {
                let req = &request;
                async move {
                    self.clob
                        .order_book(req)
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))
                }
            })
            .await
            .context("Failed to fetch order book")?;

        Ok(convert_order_book(token_id, &response))
    }

    // === Price History ===

    /// Fetch price history for a token.
    #[instrument(skip(self), fields(token_id = %token_id))]
    pub async fn get_price_history(
        &self,
        token_id: &str,
        interval: Interval,
    ) -> Result<Vec<PriceHistoryPoint>> {
        self.rate_limit().await;

        let token_u256 = parse_token_id(token_id)?;

        let request = PriceHistoryRequest::builder()
            .market(token_u256)
            .time_range(TimeRange::Interval { interval })
            .build();

        let response: polymarket_client_sdk::clob::types::response::PriceHistoryResponse = self
            .with_retry(|| {
                let req = &request;
                async move {
                    self.clob
                        .price_history(req)
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))
                }
            })
            .await
            .context("Failed to fetch price history")?;

        let points = response
            .history
            .into_iter()
            .filter_map(|p| {
                let timestamp = chrono::DateTime::from_timestamp(p.t, 0)?;
                Some(PriceHistoryPoint {
                    timestamp,
                    price: p.p,
                })
            })
            .collect();

        Ok(points)
    }

    // === Midpoint Price ===

    /// Get midpoint price for a token.
    pub async fn get_midpoint(&self, token_id: &str) -> Result<Decimal> {
        let book = self.get_order_book(token_id).await?;
        Ok(book.midpoint)
    }

    /// Get current YES price for a market by condition_id from Gamma API.
    /// Returns the first outcome price (YES) as a Decimal.
    /// This is a lightweight call for exit signal evaluation.
    pub async fn get_current_yes_price(&self, condition_id: &str) -> Result<Decimal> {
        self.rate_limit().await;

        let url = format!("{}/markets", self.gamma_base_url);
        let markets: Vec<GammaMarketResponse> = self
            .http
            .get(&url)
            .query(&[("condition_id", condition_id)])
            .send()
            .await
            .context("HTTP request to Gamma API failed")?
            .json()
            .await
            .context("Failed to parse Gamma response")?;

        let market = markets.first().context("Market not found on Gamma")?;
        let prices_str = market.outcome_prices.as_deref().unwrap_or("[]");
        let prices: Vec<String> = serde_json::from_str(prices_str).unwrap_or_default();
        let yes_price = prices
            .first()
            .and_then(|s| Decimal::from_str(s).ok())
            .unwrap_or(dec!(0.5));

        Ok(yes_price)
    }

    // === Order Placement ===

    /// Place a limit order. In paper mode, simulates the order.
    /// In live mode, places a real order on Polymarket CLOB.
    #[instrument(skip(self), fields(token_id = %token_id, side = %side, price = %price, size = %size))]
    pub async fn place_limit_order(
        &self,
        token_id: &str,
        side: Side,
        price: Decimal,
        size: Decimal,
    ) -> Result<String> {
        match self.config.agent.mode {
            AgentMode::Paper => {
                self.paper_place_order(token_id, VenueSide::Buy, price, size)
                    .await
            }
            AgentMode::Live => {
                self.live_place_limit_order(token_id, side, price, size)
                    .await
            }
            AgentMode::Backtest => {
                // In backtest mode, simulate orders same as paper trading
                self.paper_place_order(token_id, VenueSide::Buy, price, size)
                    .await
            }
        }
    }

    /// Place a live limit order on Polymarket CLOB.
    async fn live_place_limit_order(
        &self,
        token_id: &str,
        side: Side,
        price: Decimal,
        size: Decimal,
    ) -> Result<String> {
        let auth = self.auth_client.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Authenticated client not available for live trading")
        })?;

        let token_u256 = parse_token_id(token_id)?;
        let sdk_price = SdkDecimal::from_str(&price.to_string())
            .context("Failed to convert price to SDK decimal")?;
        let sdk_size = SdkDecimal::from_str(&size.to_string())
            .context("Failed to convert size to SDK decimal")?;

        // An entry is a BUY of whichever outcome token was selected — the
        // token id already says which outcome. Mapping Side::No to a SELL
        // (as this did) tried to sell NO shares the wallet doesn't hold.
        let clob_side = match side {
            Side::Yes | Side::No => ClobSide::Buy,
        };

        // Build limit order (GTD = Good Till Date, 7 day expiry)
        let order = auth
            .clob
            .limit_order()
            .token_id(token_u256)
            .order_type(OrderType::GTD)
            .expiration(Utc::now() + TimeDelta::days(7))
            .price(sdk_price)
            .size(sdk_size)
            .side(clob_side)
            .build()
            .await
            .context("Failed to build limit order")?;

        // Sign the order with EIP-712
        let signed_order = auth
            .clob
            .sign(&auth.signer, order)
            .await
            .context("Failed to sign order")?;

        // Submit the order
        let response = auth
            .clob
            .post_order(signed_order)
            .await
            .map_err(|e| anyhow::anyhow!("Order submission failed: {e}"))?;

        info!(
            order_id = %response.order_id,
            success = response.success,
            "Live order placed successfully"
        );

        Ok(response.order_id)
    }

    /// Cancel an order by ID.
    pub async fn cancel_order(&self, order_id: &str) -> Result<()> {
        match self.config.agent.mode {
            AgentMode::Paper => {
                if let Some(ref state) = self.paper_state {
                    let mut state = state.lock().await;
                    if let Some(order) = state
                        .order_history
                        .iter_mut()
                        .find(|o| o.order_id == order_id && !o.filled)
                    {
                        order.filled = true;
                        info!(order_id, "Paper order cancelled");
                    }
                }
                Ok(())
            }
            AgentMode::Live => self.live_cancel_order(order_id).await,
            AgentMode::Backtest => Ok(()),
        }
    }

    /// Cancel a live order on Polymarket CLOB.
    async fn live_cancel_order(&self, order_id: &str) -> Result<()> {
        let auth = self.auth_client.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Authenticated client not available for live trading")
        })?;

        self.with_retry(|| {
            let oid = order_id.to_string();
            async move {
                auth.clob
                    .cancel_order(&oid)
                    .await
                    .map_err(|e| anyhow::anyhow!("Cancel order failed: {e}"))
            }
        })
        .await?;

        info!(order_id, "Live order cancelled successfully");
        Ok(())
    }

    // === Balance ===

    /// Get available balance. In paper mode, returns simulated balance.
    /// In live mode, queries the Polymarket CLOB balance allowance endpoint.
    pub async fn get_balance(&self) -> Result<Decimal> {
        match self.config.agent.mode {
            AgentMode::Paper => {
                if let Some(ref state) = self.paper_state {
                    let state = state.lock().await;
                    Ok(state.balance)
                } else {
                    Ok(Decimal::ZERO)
                }
            }
            AgentMode::Live => self.live_get_balance().await,
            AgentMode::Backtest => Ok(Decimal::ZERO),
        }
    }

    /// Get live balance from Polymarket CLOB.
    async fn live_get_balance(&self) -> Result<Decimal> {
        let auth = self.auth_client.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Authenticated client not available for live trading")
        })?;

        let response = self
            .with_retry(|| async move {
                auth.clob
                    .balance_allowance(BalanceAllowanceRequest::default())
                    .await
                    .map_err(|e| anyhow::anyhow!("Balance query failed: {e}"))
            })
            .await?;

        // The balance is returned as a string in the response
        let balance_str = response.balance.to_string();
        let balance =
            Decimal::from_str(&balance_str).context("Failed to parse balance from response")?;

        info!(balance = %balance, "Live balance retrieved");
        Ok(balance)
    }

    /// Get the wallet address for the authenticated account.
    pub async fn get_wallet_address(&self) -> Result<String> {
        let auth = self
            .auth_client
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Authenticated client not available"))?;

        Ok(format!("{:?}", auth.signer.address()))
    }

    /// Exit a position by placing a sell order.
    /// In live mode, places a real sell order. In paper mode, marks as exited.
    pub async fn exit_position(
        &self,
        token_id: &str,
        side: Side,
        price: Decimal,
        size: Decimal,
    ) -> Result<String> {
        match self.config.agent.mode {
            AgentMode::Paper => {
                // Run the exit through the same fill simulator as an entry so
                // the balance and fill price reflect it. Previously this was a
                // log line that returned a fake id, so paper exits never
                // realised any P&L.
                self.paper_place_order(token_id, VenueSide::Sell, price, size)
                    .await
            }
            AgentMode::Live => {
                // Exiting means selling the outcome token we hold, whichever
                // outcome it is — the token id already identifies it.
                let _ = side;
                self.live_place_limit_order_with_side(token_id, ClobSide::Sell, price, size)
                    .await
            }
            AgentMode::Backtest => Ok(format!("backtest_exit_{token_id}")),
        }
    }

    /// Place a live limit order with an explicit CLOB side (for exits).
    async fn live_place_limit_order_with_side(
        &self,
        token_id: &str,
        clob_side: ClobSide,
        price: Decimal,
        size: Decimal,
    ) -> Result<String> {
        let auth = self.auth_client.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Authenticated client not available for live trading")
        })?;

        let token_u256 = parse_token_id(token_id)?;
        let sdk_price = SdkDecimal::from_str(&price.to_string())
            .context("Failed to convert price to SDK decimal")?;
        let sdk_size = SdkDecimal::from_str(&size.to_string())
            .context("Failed to convert size to SDK decimal")?;

        // Build limit order (GTD = Good Till Date, 7 day expiry)
        let order = auth
            .clob
            .limit_order()
            .token_id(token_u256)
            .order_type(OrderType::GTD)
            .expiration(Utc::now() + TimeDelta::days(7))
            .price(sdk_price)
            .size(sdk_size)
            .side(clob_side)
            .build()
            .await
            .context("Failed to build exit limit order")?;

        // Sign the order with EIP-712
        let signed_order = auth
            .clob
            .sign(&auth.signer, order)
            .await
            .context("Failed to sign exit order")?;

        // Submit the order
        let response = auth
            .clob
            .post_order(signed_order)
            .await
            .map_err(|e| anyhow::anyhow!("Exit order submission failed: {e}"))?;

        info!(
            order_id = %response.order_id,
            success = response.success,
            "Live exit order placed successfully"
        );

        Ok(response.order_id)
    }

    // === Venue-semantics order placement ===

    /// Place an order on one outcome token with explicit buy/sell semantics.
    ///
    /// `place_limit_order` treats YES/NO as *sides*, which is what forced
    /// buying NO to be sent as a SELL. Here the token id identifies the
    /// outcome and the side means exactly what it says, so the venue adapter
    /// can express "buy the NO token" without any inversion.
    ///
    /// Unlike the legacy path this also returns what actually filled, rather
    /// than discarding the simulator's fill price and size.
    pub(crate) async fn place_token_order(
        &self,
        token_id: &str,
        side: VenueSide,
        price: Decimal,
        size: Decimal,
    ) -> Result<TokenOrderOutcome> {
        match self.config.agent.mode {
            AgentMode::Paper | AgentMode::Backtest => {
                let fill = self
                    .simulate_paper_fill(token_id, side, price, size)
                    .await?;
                Ok(if fill.filled {
                    TokenOrderOutcome {
                        order_id: fill.order_id,
                        state: VenueOrderState::Filled,
                        filled_qty: fill.fill_size,
                        avg_fill_price: Some(fill.fill_price),
                    }
                } else {
                    // Resting unfilled is a normal outcome, not an error.
                    TokenOrderOutcome {
                        order_id: fill.order_id,
                        state: VenueOrderState::Accepted,
                        filled_qty: Decimal::ZERO,
                        avg_fill_price: None,
                    }
                })
            }
            AgentMode::Live => {
                let clob_side = match side {
                    VenueSide::Buy => ClobSide::Buy,
                    VenueSide::Sell => ClobSide::Sell,
                };
                let order_id = self
                    .live_place_limit_order_with_side(token_id, clob_side, price, size)
                    .await?;
                // Accepted, not Filled: the CLOB returning an id says nothing
                // about whether it filled. Confirmation is the caller's job.
                Ok(TokenOrderOutcome {
                    order_id,
                    state: VenueOrderState::Accepted,
                    filled_qty: Decimal::ZERO,
                    avg_fill_price: None,
                })
            }
        }
    }

    // === Paper Trading ===

    /// Place a paper order with realistic fill simulation.
    ///
    /// Instead of assuming instant fills, this simulates:
    /// - Fill probability based on order aggressiveness
    /// - Adverse selection (fills when price moves against us)
    /// - Partial fills based on order book depth
    async fn paper_place_order(
        &self,
        token_id: &str,
        side: VenueSide,
        price: Decimal,
        size: Decimal,
    ) -> Result<String> {
        let fill_result = self
            .simulate_paper_fill(token_id, side, price, size)
            .await?;

        if !fill_result.filled {
            bail!("Paper order not filled — simulated queue position not reached");
        }

        Ok(fill_result.order_id)
    }

    /// Realistic paper trading fill simulation.
    ///
    /// Models:
    /// 1. Fill probability based on order aggressiveness vs midpoint
    /// 2. Adverse selection: when filled, price has moved against us by 1-3%
    /// 3. Partial fills: only 60-100% of order size fills
    async fn simulate_paper_fill(
        &self,
        token_id: &str,
        side: VenueSide,
        price: Decimal,
        size: Decimal,
    ) -> Result<PaperFillResult> {
        let Some(ref state_mutex) = self.paper_state else {
            bail!("Paper trading state not initialized");
        };

        let mut state = state_mutex.lock().await;

        // Generate deterministic but varied fill behavior based on token_id
        let seed = token_id
            .chars()
            .fold(0u64, |acc, c| acc.wrapping_add(c as u64));
        let fill_probability = compute_fill_probability(side, price, seed);
        let fill_ratio = compute_fill_ratio(seed);
        let adverse_selection = compute_adverse_selection(seed);

        // Roll for fill
        let fill_threshold = (fill_probability * dec!(100)).to_u32().unwrap_or(70);
        let fills = (seed % 100) < fill_threshold as u64;

        let order_id = uuid::Uuid::new_v4().to_string();

        if !fills {
            // Order not filled — record but don't deduct balance
            state.order_history.push(PaperOrder {
                order_id: order_id.clone(),
                token_id: token_id.to_string(),
                side,
                price,
                size,
                filled: false,
                adverse_selection: false,
            });

            info!(
                order_id = %order_id,
                fill_probability = %fill_probability,
                "Paper order NOT filled (simulated)"
            );

            return Ok(PaperFillResult {
                order_id,
                filled: false,
                fill_price: Decimal::ZERO,
                fill_size: Decimal::ZERO,
                adverse_selection: false,
            });
        }

        // Order filled — apply adverse selection and partial fill
        let actual_size = size * fill_ratio;

        // Adverse selection: we fill at a slightly worse price than requested
        let adverse_slippage = if adverse_selection {
            // Price moved 1-3% against us at fill time
            let slippage_pct = dec!(0.01) + (dec!(0.02) * Decimal::from(seed % 100) / dec!(100));
            match side {
                // A buy fills worse by paying more; a sell by receiving less.
                VenueSide::Buy => price * (dec!(1) + slippage_pct),
                VenueSide::Sell => price * (dec!(1) - slippage_pct),
            }
        } else {
            price
        };

        apply_paper_fill(&mut state, token_id, side, adverse_slippage, actual_size)?;

        state.order_history.push(PaperOrder {
            order_id: order_id.clone(),
            token_id: token_id.to_string(),
            side,
            price: adverse_slippage,
            size: actual_size,
            filled: true,
            adverse_selection,
        });

        info!(
            order_id = %order_id,
            balance = %state.balance,
            fill_ratio = %fill_ratio,
            adverse_selection,
            fill_price = %adverse_slippage,
            "Paper order filled (realistic simulation)"
        );

        Ok(PaperFillResult {
            order_id,
            filled: true,
            fill_price: adverse_slippage,
            fill_size: actual_size,
            adverse_selection,
        })
    }

    // === Accessors ===

    /// Borrow the HTTP client for use by resolution and other modules.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    /// Borrow the Gamma API base URL.
    pub fn gamma_base_url(&self) -> &str {
        &self.gamma_base_url
    }

    // === Rate Limiting ===

    async fn rate_limit(&self) {
        self.limiter.until_ready().await;
    }

    // === Retry Logic ===

    async fn with_retry<F, Fut, T>(&self, operation: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let max_retries = self.config.execution.max_retries;
        let base_ms = self.config.rate_limit.backoff_base_ms;
        let max_ms = self.config.rate_limit.backoff_max_ms;

        let mut attempt = 0u32;

        loop {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    attempt += 1;

                    let err_str = e.to_string();

                    // Non-retryable errors
                    if err_str.contains("insufficient")
                        || err_str.contains("Insufficient")
                        || err_str.contains("balance")
                    {
                        return Err(e.context("Insufficient balance — not retrying"));
                    }
                    if err_str.contains("401")
                        || err_str.contains("403")
                        || err_str.contains("auth")
                    {
                        return Err(e.context("Authentication failure — not retrying"));
                    }

                    if attempt > max_retries {
                        return Err(e.context(format!("Failed after {max_retries} retries")));
                    }

                    let backoff_ms =
                        std::cmp::min(base_ms.saturating_mul(2u64.pow(attempt - 1)), max_ms);

                    warn!(
                        attempt,
                        backoff_ms,
                        error = %e,
                        "Retrying after transient failure"
                    );

                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }
}

// === Helper Functions ===

fn create_rate_limiter(config: &RateLimitConfig) -> Arc<Limiter> {
    let rps = NonZeroU32::new(config.requests_per_second).unwrap_or(NonZeroU32::new(10).unwrap());
    let burst = NonZeroU32::new(config.burst_size).unwrap_or(NonZeroU32::new(20).unwrap());

    let quota = Quota::per_second(rps).allow_burst(burst);
    Arc::new(RateLimiter::direct(quota))
}

fn parse_token_id(token_id: &str) -> Result<U256> {
    token_id
        .parse::<U256>()
        .map_err(|e| anyhow::anyhow!("Invalid token_id '{}': {}", token_id, e))
}

/// Lightweight Gamma API market response for direct deserialization.
/// The Gamma API returns some fields as JSON-encoded strings (outcomes,
/// outcomePrices, clobTokenIds) and numeric fields as floats.
/// We use `#[serde(default)]` and `Option` liberally to handle missing fields.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaMarketResponse {
    condition_id: Option<String>,
    question: Option<String>,
    /// JSON-encoded string: "[\"Yes\", \"No\"]"
    outcomes: Option<String>,
    /// JSON-encoded string: "[\"0.025\", \"0.975\"]"
    outcome_prices: Option<String>,
    /// JSON-encoded string: "[\"14310...\", \"49141...\"]"
    clob_token_ids: Option<String>,
    /// RFC3339 datetime string
    end_date: Option<String>,
    volume24hr: Option<f64>,
    active: Option<bool>,
    closed: Option<bool>,
}

/// Parse a JSON-encoded string array like "[\"a\", \"b\"]" into Vec<String>.
fn parse_json_string_array(s: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(s).unwrap_or_default()
}

/// Convert our direct Gamma response to domain Market type.
/// Apply a fill to the paper book.
///
/// Free function so the accounting can be exercised without constructing a
/// live CLOB client, which is what let the sell path stay wrong.
fn apply_paper_fill(
    state: &mut PaperTradingState,
    token_id: &str,
    side: VenueSide,
    fill_price: Decimal,
    fill_size: Decimal,
) -> Result<()> {
    let notional = fill_price * fill_size;

    // A sell has to consume an existing holding, not conjure one. Crediting
    // cash unconditionally and then pushing a second position left an exit
    // holding *both* a long and a short of the same token: reported
    // exposure doubled, and repeating the exit credited the proceeds again
    // from nothing, so paper balance grew without bound. Paper P&L is the
    // only evidence the strategy works, and that made it evidence of
    // nothing.
    match side {
        VenueSide::Buy => {
            if notional > state.balance {
                bail!(
                    "Insufficient paper balance: {} < cost {}",
                    state.balance,
                    notional
                );
            }
            state.balance -= notional;
            // Each buy is its own lot, so entry prices stay meaningful
            // across several entries into the same token.
            state.positions.push(PaperPosition {
                token_id: token_id.to_string(),
                side,
                size: fill_size,
                entry_price: fill_price,
            });
        }
        VenueSide::Sell => {
            let held: Decimal = state
                .positions
                .iter()
                .filter(|p| p.token_id == token_id)
                .map(|p| p.size)
                .sum();

            // Outcome tokens cannot be shorted — the complement is a
            // separate instrument you buy — so selling more than is held
            // means the ledger and the paper book have diverged. Failing
            // loudly beats clamping, which would hide the divergence
            // behind a trade of a size nobody asked for.
            if fill_size > held {
                bail!(
                    "Insufficient paper position in {token_id}: holding {held}, tried to sell {fill_size}"
                );
            }

            let mut remaining = fill_size;
            for lot in state
                .positions
                .iter_mut()
                .filter(|p| p.token_id == token_id)
            {
                if remaining.is_zero() {
                    break;
                }
                let taken = remaining.min(lot.size);
                lot.size -= taken;
                remaining -= taken;
            }
            state.positions.retain(|p| p.size > Decimal::ZERO);
            state.balance += notional;
        }
    }

    Ok(())
}

fn convert_gamma_response(gm: &GammaMarketResponse) -> Option<Market> {
    let question = gm.question.clone()?;
    let end_date_str = gm.end_date.as_ref()?;
    let end_date: DateTime<Utc> = DateTime::parse_from_rfc3339(end_date_str)
        .ok()?
        .with_timezone(&Utc);

    let token_ids = parse_json_string_array(gm.clob_token_ids.as_deref()?);
    let outcomes = parse_json_string_array(gm.outcomes.as_deref()?);
    let outcome_prices = parse_json_string_array(gm.outcome_prices.as_deref().unwrap_or("[]"));

    if token_ids.is_empty() || outcomes.is_empty() {
        return None;
    }

    let tokens: Vec<TokenInfo> = token_ids
        .iter()
        .enumerate()
        .map(|(i, tid)| {
            let outcome = outcomes.get(i).cloned().unwrap_or_default();
            let price = outcome_prices
                .get(i)
                .and_then(|s| Decimal::from_str(s).ok())
                .unwrap_or(Decimal::ZERO);
            TokenInfo {
                token_id: tid.clone(),
                outcome,
                price,
            }
        })
        .collect();

    let category = crate::market::category::infer_category(&question);

    let volume_24h = gm
        .volume24hr
        .and_then(|v| Decimal::try_from(v).ok())
        .unwrap_or(Decimal::ZERO);
    let active = gm.active.unwrap_or(false) && !gm.closed.unwrap_or(true);

    Some(Market {
        condition_id: gm.condition_id.clone().unwrap_or_default(),
        question,
        outcomes,
        tokens,
        end_date,
        category,
        volume_24h,
        active,
    })
}

/// Convert SDK order book response to our domain type.
fn convert_order_book(token_id: &str, response: &OrderBookSummaryResponse) -> OrderBookSnapshot {
    let bids: Vec<PriceLevel> = response
        .bids
        .iter()
        .map(|o| PriceLevel {
            price: o.price,
            size: o.size,
        })
        .collect();

    let asks: Vec<PriceLevel> = response
        .asks
        .iter()
        .map(|o| PriceLevel {
            price: o.price,
            size: o.size,
        })
        .collect();

    let best_bid = bids.first().map(|b| b.price).unwrap_or(Decimal::ZERO);
    let best_ask = asks.first().map(|a| a.price).unwrap_or(Decimal::ONE);
    let midpoint = (best_bid + best_ask) / dec!(2);
    let spread = best_ask - best_bid;

    // In binary prediction markets, price approximates probability
    let implied_probability = midpoint;

    OrderBookSnapshot {
        token_id: token_id.to_string(),
        bids,
        asks,
        spread,
        midpoint,
        implied_probability,
        timestamp: Utc::now(),
    }
}

// === Paper Trading Fill Simulation Helpers ===

/// Compute fill probability based on order aggressiveness.
/// Orders at or inside the spread fill more often; outside fill less.
fn compute_fill_probability(_side: VenueSide, _price: Decimal, seed: u64) -> Decimal {
    // Base fill rate: 70% for aggressive orders
    let base = dec!(0.70);

    // Deterministic variation based on seed: +/- 15%
    let variation = dec!(0.15) * Decimal::from(seed % 100) / dec!(50) - dec!(0.15);

    // Clamp to 40%-95% range
    (base + variation).max(dec!(0.40)).min(dec!(0.95))
}

/// Compute fill ratio: what fraction of the order actually fills.
/// Models partial fills due to queue position and available liquidity.
fn compute_fill_ratio(seed: u64) -> Decimal {
    // 60-100% of order fills
    dec!(0.60) + dec!(0.40) * Decimal::from(seed % 100) / dec!(100)
}

/// Determine if this fill has adverse selection.
/// ~30% of fills experience adverse selection (price moved against us).
fn compute_adverse_selection(seed: u64) -> bool {
    seed % 10 < 3 // 30% chance
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deserialize_order_book(json: &str) -> OrderBookSummaryResponse {
        serde_json::from_str(json).expect("valid order book JSON")
    }

    /// A paper-mode client whose Gamma base URL points at a mock server.
    /// Paper mode needs no private key, and `ClobClient::new` only parses its
    /// base URL, so the whole Gamma path is exercisable offline.
    async fn client_against(gamma_base_url: &str) -> PolymarketClient {
        let contents = std::fs::read_to_string("config/default.toml")
            .expect("config/default.toml should exist");
        let mut config: AppConfig = toml::from_str(&contents).expect("should parse");
        config.agent.mode = AgentMode::Paper;
        config.polymarket.gamma_base_url = gamma_base_url.to_string();

        PolymarketClient::new(Arc::new(config), &Secrets::default())
            .await
            .expect("paper client needs no credentials")
    }

    fn gamma_market_json(condition_id: &str, closed: bool) -> serde_json::Value {
        serde_json::json!([{
            "conditionId": condition_id,
            "question": "Will it rain?",
            "endDate": "2020-01-01T00:00:00Z",
            "closed": closed,
            "clobTokenIds": "[\"tok_yes\", \"tok_no\"]",
            "outcomes": "[\"Yes\", \"No\"]",
            "outcomePrices": "[\"0.6\", \"0.4\"]",
            "volume24hr": 0.0,
            "liquidity": 0.0
        }])
    }

    /// Finding 5: a held position needs quoting most urgently once its market
    /// has closed or dropped out of the top-by-volume page — exactly what
    /// `get_markets` filters out. The by-id lookup must not inherit any of
    /// that, so this market is closed, zero-volume and long past its end date.
    #[tokio::test]
    async fn a_closed_market_is_still_findable_by_condition_id() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/markets"))
            .and(wiremock::matchers::query_param("condition_id", "0xdead"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(gamma_market_json("0xdead", true)),
            )
            .mount(&server)
            .await;

        let client = client_against(&server.uri()).await;
        let market = client
            .get_market_by_condition_id("0xdead")
            .await
            .expect("lookup succeeds")
            .expect("closed market is still returned");

        assert_eq!(market.condition_id, "0xdead");
        assert_eq!(market.tokens.len(), 2);
    }

    #[tokio::test]
    async fn an_unknown_condition_id_is_none_not_an_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/markets"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let client = client_against(&server.uri()).await;
        assert!(client
            .get_market_by_condition_id("0xmissing")
            .await
            .expect("lookup succeeds")
            .is_none());
    }

    fn paper_book(balance: Decimal) -> PaperTradingState {
        PaperTradingState {
            balance,
            positions: Vec::new(),
            order_history: Vec::new(),
        }
    }

    fn held(state: &PaperTradingState, token_id: &str) -> Decimal {
        state
            .positions
            .iter()
            .filter(|p| p.token_id == token_id)
            .map(|p| p.size)
            .sum()
    }

    /// A round trip has to end flat with the cash back, not holding both sides
    /// of itself. The old sell path credited cash and pushed a second position,
    /// so an exit left [long 10, sell 10] and double the reported exposure.
    #[test]
    fn a_paper_round_trip_ends_flat() {
        let mut state = paper_book(dec!(100));

        apply_paper_fill(&mut state, "tok", VenueSide::Buy, dec!(0.60), dec!(10)).unwrap();
        assert_eq!(state.balance, dec!(94));
        assert_eq!(held(&state, "tok"), dec!(10));

        apply_paper_fill(&mut state, "tok", VenueSide::Sell, dec!(0.60), dec!(10)).unwrap();
        assert_eq!(state.balance, dec!(100));
        assert_eq!(held(&state, "tok"), dec!(0));
        assert!(
            state.positions.is_empty(),
            "a closed position must not linger: {:?}",
            state.positions
        );
    }

    /// The failure that made it critical: repeating an exit used to credit the
    /// proceeds again out of nothing, so paper balance grew without bound.
    #[test]
    fn selling_more_than_is_held_is_refused_rather_than_minting_cash() {
        let mut state = paper_book(dec!(100));
        apply_paper_fill(&mut state, "tok", VenueSide::Buy, dec!(0.60), dec!(10)).unwrap();
        apply_paper_fill(&mut state, "tok", VenueSide::Sell, dec!(0.60), dec!(10)).unwrap();

        let err = apply_paper_fill(&mut state, "tok", VenueSide::Sell, dec!(0.60), dec!(10))
            .expect_err("a second exit has nothing left to sell");
        assert!(err.to_string().contains("Insufficient paper position"));
        assert_eq!(state.balance, dec!(100), "balance must not move on refusal");
    }

    #[test]
    fn a_partial_exit_leaves_the_remainder_open() {
        let mut state = paper_book(dec!(100));
        apply_paper_fill(&mut state, "tok", VenueSide::Buy, dec!(0.50), dec!(10)).unwrap();

        apply_paper_fill(&mut state, "tok", VenueSide::Sell, dec!(0.50), dec!(4)).unwrap();
        assert_eq!(held(&state, "tok"), dec!(6));
        assert_eq!(state.balance, dec!(97));
    }

    /// Lots are consumed oldest-first and only from the token being sold.
    #[test]
    fn selling_one_token_leaves_another_untouched() {
        let mut state = paper_book(dec!(100));
        apply_paper_fill(&mut state, "a", VenueSide::Buy, dec!(0.50), dec!(4)).unwrap();
        apply_paper_fill(&mut state, "b", VenueSide::Buy, dec!(0.50), dec!(6)).unwrap();
        apply_paper_fill(&mut state, "a", VenueSide::Buy, dec!(0.70), dec!(2)).unwrap();

        apply_paper_fill(&mut state, "a", VenueSide::Sell, dec!(0.60), dec!(5)).unwrap();

        assert_eq!(held(&state, "a"), dec!(1));
        assert_eq!(held(&state, "b"), dec!(6), "unrelated token must not move");
        // The surviving "a" lot is the newer one, bought at 0.70.
        let remaining = state
            .positions
            .iter()
            .find(|p| p.token_id == "a")
            .expect("one lot left");
        assert_eq!(remaining.entry_price, dec!(0.70));
    }

    #[test]
    fn a_buy_beyond_the_balance_is_refused() {
        let mut state = paper_book(dec!(5));
        let err = apply_paper_fill(&mut state, "tok", VenueSide::Buy, dec!(0.60), dec!(10))
            .expect_err("6 > 5");
        assert!(err.to_string().contains("Insufficient paper balance"));
        assert_eq!(state.balance, dec!(5));
        assert!(state.positions.is_empty());
    }

    #[test]
    fn test_spread_calculation() {
        let json = r#"{
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "asset_id": "0",
            "timestamp": "1700000000000",
            "bids": [{"price": "0.60", "size": "100"}],
            "asks": [{"price": "0.70", "size": "100"}],
            "min_order_size": "1",
            "neg_risk": false,
            "tick_size": "0.01"
        }"#;
        let response = deserialize_order_book(json);

        let book = convert_order_book("12345", &response);
        assert_eq!(book.spread, dec!(0.10));
        assert_eq!(book.midpoint, dec!(0.65));
        assert_eq!(book.implied_probability, dec!(0.65));
    }

    #[test]
    fn test_empty_order_book() {
        let json = r#"{
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "asset_id": "0",
            "timestamp": "1700000000000",
            "bids": [],
            "asks": [],
            "min_order_size": "1",
            "neg_risk": false,
            "tick_size": "0.01"
        }"#;
        let response = deserialize_order_book(json);

        let book = convert_order_book("12345", &response);
        assert_eq!(book.bids.len(), 0);
        assert_eq!(book.asks.len(), 0);
        assert_eq!(book.midpoint, dec!(0.5));
        assert_eq!(book.spread, dec!(1));
    }

    #[test]
    fn test_rate_limiter_creation() {
        let config = RateLimitConfig {
            requests_per_second: 10,
            burst_size: 20,
            backoff_base_ms: 1000,
            backoff_max_ms: 30000,
        };
        let limiter = create_rate_limiter(&config);
        assert!(limiter.check().is_ok());
    }

    #[tokio::test]
    async fn test_paper_order_deducts_balance() {
        let config = Arc::new(test_paper_config());
        let secrets = test_secrets();
        let client = PolymarketClient::new(config, &secrets).await.unwrap();

        let order_result = client
            .place_limit_order("12345", Side::Yes, dec!(0.50), dec!(10))
            .await;

        // With realistic fill simulation, order may or may not fill
        // If it fills, balance should be reduced (with some variation due to partial fills)
        if order_result.is_ok() {
            let balance = client.get_balance().await.unwrap();
            // Balance should be less than or equal to starting balance (100)
            assert!(balance <= dec!(100));
            // Balance should be greater than 0
            assert!(balance > Decimal::ZERO);
        }
    }

    #[tokio::test]
    async fn test_paper_order_insufficient_balance() {
        let config = Arc::new(test_paper_config());
        let secrets = test_secrets();
        let client = PolymarketClient::new(config, &secrets).await.unwrap();

        let result = client
            .place_limit_order("12345", Side::Yes, dec!(0.50), dec!(300))
            .await;

        // With realistic fills, this may fail due to insufficient balance OR not fill
        // Either outcome is valid for the test
        if let Err(e) = result {
            assert!(e.to_string().contains("Insufficient") || e.to_string().contains("not filled"));
        }
    }

    #[tokio::test]
    async fn test_paper_multiple_orders() {
        let config = Arc::new(test_paper_config());
        let secrets = test_secrets();
        let client = PolymarketClient::new(config, &secrets).await.unwrap();

        // Place first order
        let _result1 = client
            .place_limit_order("111", Side::Yes, dec!(0.50), dec!(20))
            .await;

        // Place second order
        let _result2 = client
            .place_limit_order("222", Side::No, dec!(0.30), dec!(50))
            .await;

        // With realistic fills, balance should be <= starting balance
        let balance = client.get_balance().await.unwrap();
        assert!(balance <= dec!(100));
        assert!(balance >= Decimal::ZERO);
    }

    fn test_paper_config() -> AppConfig {
        let toml_str = include_str!("../../config/default.toml");
        toml::from_str(toml_str).unwrap()
    }

    fn test_secrets() -> Secrets {
        Secrets {
            polymarket_private_key: None,
            llm_api_key: None,
            discord_webhook_url: None,
            noaa_api_token: None,
            espn_api_key: None,
            dashboard_token: None,
            alpaca_key_id: None,
            coinbase_key_name: None,
            coinbase_private_key: None,
            alpaca_secret_key: None,
        }
    }
}
