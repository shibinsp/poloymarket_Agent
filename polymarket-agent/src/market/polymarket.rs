//! Polymarket CLOB API client wrapper.
//!
//! Wraps `polymarket-client-sdk` with rate limiting, paper trading,
//! retry logic, and domain type conversion.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};
use polymarket_client_sdk::clob::types::request::{
    OrderBookSummaryRequest, PriceHistoryRequest,
};
use polymarket_client_sdk::clob::types::response::OrderBookSummaryResponse;
use polymarket_client_sdk::clob::types::{Interval, TimeRange};
use polymarket_client_sdk::clob::Client as ClobClient;
use polymarket_client_sdk::auth::state::Unauthenticated;
use polymarket_client_sdk::types::U256;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use std::str::FromStr;
use tokio::sync::Mutex;
use tracing::{info, instrument, warn};

use crate::config::{AgentMode, AppConfig, RateLimitConfig, Secrets};
use crate::market::models::{
    Market, MarketCategory, OrderBookSnapshot, PriceHistoryPoint, PriceLevel, Side, TokenInfo,
};

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
struct PaperPosition {
    pub token_id: String,
    pub side: Side,
    pub size: Decimal,
    pub entry_price: Decimal,
}

/// Paper trading simulated order.
#[derive(Debug, Clone)]
struct PaperOrder {
    pub order_id: String,
    pub token_id: String,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
    pub filled: bool,
}

/// Tracks simulated state for paper trading.
struct PaperTradingState {
    balance: Decimal,
    positions: Vec<PaperPosition>,
    order_history: Vec<PaperOrder>,
}

pub struct PolymarketClient {
    config: Arc<AppConfig>,
    /// CLOB client (unauthenticated — for market data)
    clob: ClobClient<Unauthenticated>,
    /// HTTP client for direct Gamma API calls (bypasses SDK deserialization issues)
    http: reqwest::Client,
    /// Gamma API base URL
    gamma_base_url: String,
    /// Rate limiter
    limiter: Arc<Limiter>,
    /// Paper trading state (only in Paper mode)
    paper_state: Option<Mutex<PaperTradingState>>,
}

impl PolymarketClient {
    pub async fn new(config: Arc<AppConfig>, _secrets: &Secrets) -> Result<Self> {
        let clob = ClobClient::new(
            &config.polymarket.clob_base_url,
            polymarket_client_sdk::clob::Config::default(),
        )
        .context("Failed to create CLOB client")?;

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        let gamma_base_url = config.polymarket.gamma_base_url.trim_end_matches('/').to_string();

        let limiter = create_rate_limiter(&config.rate_limit);

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

    // === Order Placement ===

    /// Place a limit order. In paper mode, simulates the order.
    #[instrument(skip(self), fields(token_id = %token_id, side = %side, price = %price, size = %size))]
    pub async fn place_limit_order(
        &self,
        token_id: &str,
        side: Side,
        price: Decimal,
        size: Decimal,
    ) -> Result<String> {
        match self.config.agent.mode {
            AgentMode::Paper => self.paper_place_order(token_id, side, price, size).await,
            AgentMode::Live => {
                bail!("Live order placement requires authenticated client (Phase 6)")
            }
            AgentMode::Backtest => {
                // In backtest mode, simulate orders same as paper trading
                self.paper_place_order(token_id, side, price, size).await
            }
        }
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
            AgentMode::Live => {
                bail!("Live cancel requires authenticated client (Phase 6)")
            }
            AgentMode::Backtest => Ok(()),
        }
    }

    // === Balance ===

    /// Get available balance. In paper mode, returns simulated balance.
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
            AgentMode::Live => {
                // balance_allowance requires authentication — will be implemented in Phase 6
                // For now, return zero for live mode until authenticated client is available
                warn!("Live balance query requires authenticated client (Phase 6)");
                Ok(Decimal::ZERO)
            }
            AgentMode::Backtest => Ok(Decimal::ZERO),
        }
    }

    // === Paper Trading ===

    async fn paper_place_order(
        &self,
        token_id: &str,
        side: Side,
        price: Decimal,
        size: Decimal,
    ) -> Result<String> {
        let Some(ref state_mutex) = self.paper_state else {
            bail!("Paper trading state not initialized");
        };

        let mut state = state_mutex.lock().await;
        let cost = price * size;

        if cost > state.balance {
            bail!(
                "Insufficient paper balance: {} < cost {}",
                state.balance,
                cost
            );
        }

        let order_id = uuid::Uuid::new_v4().to_string();

        // Simulate immediate fill at limit price (optimistic for paper)
        state.balance -= cost;
        state.positions.push(PaperPosition {
            token_id: token_id.to_string(),
            side,
            size,
            entry_price: price,
        });
        state.order_history.push(PaperOrder {
            order_id: order_id.clone(),
            token_id: token_id.to_string(),
            side,
            price,
            size,
            filled: true,
        });

        info!(
            order_id = %order_id,
            balance = %state.balance,
            "Paper order filled"
        );

        Ok(order_id)
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

                    let backoff_ms = std::cmp::min(
                        base_ms.saturating_mul(2u64.pow(attempt - 1)),
                        max_ms,
                    );

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

    let category = MarketCategory::Other("unknown".to_string());

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

#[cfg(test)]
mod tests {
    use super::*;

    fn deserialize_order_book(json: &str) -> OrderBookSummaryResponse {
        serde_json::from_str(json).expect("valid order book JSON")
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

        let order_id = client
            .place_limit_order("12345", Side::Yes, dec!(0.50), dec!(10))
            .await
            .unwrap();

        assert!(!order_id.is_empty());

        let balance = client.get_balance().await.unwrap();
        assert_eq!(balance, dec!(95));
    }

    #[tokio::test]
    async fn test_paper_order_insufficient_balance() {
        let config = Arc::new(test_paper_config());
        let secrets = test_secrets();
        let client = PolymarketClient::new(config, &secrets).await.unwrap();

        let result = client
            .place_limit_order("12345", Side::Yes, dec!(0.50), dec!(300))
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Insufficient"));
    }

    #[tokio::test]
    async fn test_paper_multiple_orders() {
        let config = Arc::new(test_paper_config());
        let secrets = test_secrets();
        let client = PolymarketClient::new(config, &secrets).await.unwrap();

        // Place first order: cost = 0.50 * 20 = 10
        client
            .place_limit_order("111", Side::Yes, dec!(0.50), dec!(20))
            .await
            .unwrap();

        // Place second order: cost = 0.30 * 50 = 15
        client
            .place_limit_order("222", Side::No, dec!(0.30), dec!(50))
            .await
            .unwrap();

        let balance = client.get_balance().await.unwrap();
        // 100 - 10 - 15 = 75
        assert_eq!(balance, dec!(75));
    }

    fn test_paper_config() -> AppConfig {
        let toml_str = include_str!("../../config/default.toml");
        toml::from_str(toml_str).unwrap()
    }

    fn test_secrets() -> Secrets {
        Secrets {
            polymarket_private_key: None,
            anthropic_api_key: None,
            discord_webhook_url: None,
            noaa_api_token: None,
            espn_api_key: None,
        }
    }
}
