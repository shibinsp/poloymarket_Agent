//! Upstox sandbox REST client with host allowlist and safety isolation.
//!
//! Enforces:
//! 1. Host allowlist (`api.upstox.com`).
//! 2. Sandbox-only execution gating: live order placement is strictly blocked by construction.
//! 3. High-fidelity in-memory mock sandbox fallback when credentials are not configured.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::india::instruments::find_nse_instrument;
use crate::venue::types::Candle;
use crate::venue::upstox::types::*;

pub const UPSTOX_API_BASE: &str = "https://api.upstox.com/v2";
pub const ALLOWED_UPSTOX_HOST: &str = "api.upstox.com";

/// Internal mock sandbox state for isolated local testing without API credentials.
#[derive(Debug, Clone)]
struct MockSandboxState {
    pub cash: Decimal,
    pub positions: HashMap<String, UpstoxPositionRecord>,
    pub orders: HashMap<String, UpstoxOrderRecord>,
    pub prices: HashMap<String, Decimal>,
}

impl Default for MockSandboxState {
    fn default() -> Self {
        let mut prices = HashMap::new();
        // Baseline reasonable prices for our top NSE watchlist
        prices.insert("RELIANCE".to_string(), dec!(2500.00));
        prices.insert("TCS".to_string(), dec!(3800.00));
        prices.insert("INFY".to_string(), dec!(1850.00));
        prices.insert("HDFCBANK".to_string(), dec!(1650.00));
        prices.insert("ICICIBANK".to_string(), dec!(1250.00));
        prices.insert("SBIN".to_string(), dec!(800.00));
        prices.insert("BHARTIARTL".to_string(), dec!(1600.00));
        prices.insert("ITC".to_string(), dec!(470.00));
        prices.insert("LT".to_string(), dec!(3500.00));
        prices.insert("TATAMOTORS".to_string(), dec!(950.00));

        Self {
            cash: dec!(1000.00), // Default ₹1,000 sandbox margin
            positions: HashMap::new(),
            orders: HashMap::new(),
            prices,
        }
    }
}

/// Upstox sandbox API client.
#[derive(Clone)]
pub struct UpstoxSandboxClient {
    http: reqwest::Client,
    token: Option<Arc<SecretString>>,
    is_mock: bool,
    mock_state: Arc<RwLock<MockSandboxState>>,
}

impl UpstoxSandboxClient {
    pub fn new(token: Option<SecretString>) -> Result<Self> {
        let is_mock = token.is_none();
        if is_mock {
            info!("Upstox sandbox client initialized in MOCK SANDBOX mode (no UPSTOX_SANDBOX_TOKEN configured)");
        } else {
            info!("Upstox sandbox client initialized with sandbox credentials (host: {ALLOWED_UPSTOX_HOST})");
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()
            .context("Failed to build HTTP client for Upstox")?;

        Ok(Self {
            http,
            token: token.map(Arc::new),
            is_mock,
            mock_state: Arc::new(RwLock::new(MockSandboxState::default())),
        })
    }

    /// True if running in simulated in-memory mock mode.
    pub fn is_mock(&self) -> bool {
        self.is_mock
    }

    /// Fetch real live intraday candles directly from Upstox API v2.
    pub async fn fetch_intraday_candles(&self, instrument_key: &str) -> Result<Vec<Candle>> {
        let Some(ref token) = self.token else {
            bail!("No Upstox token configured for intraday candle polling");
        };

        let encoded_key = urlencoding::encode(instrument_key);
        let url = format!("{UPSTOX_API_BASE}/historical-candle/intraday/{encoded_key}/30minute");
        self.validate_url(&url)?;

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let resp = self
            .http
            .get(&url)
            .headers(headers)
            .send()
            .await
            .context("Network error fetching Upstox intraday candles")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            bail!("Upstox candles request failed {status}: {txt}");
        }

        let body: serde_json::Value = resp.json().await?;
        let candle_rows = body
            .get("data")
            .and_then(|d| d.get("candles"))
            .and_then(|c| c.as_array())
            .context("Invalid candles response structure from Upstox API")?;

        let mut candles = Vec::new();
        for row in candle_rows.iter().rev() {
            if let Some(arr) = row.as_array() {
                if arr.len() >= 6 {
                    let ts_str = arr[0].as_str().unwrap_or_default();
                    let ts = chrono::DateTime::parse_from_rfc3339(ts_str)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now());
                    let open = arr[1]
                        .as_f64()
                        .and_then(Decimal::from_f64_retain)
                        .unwrap_or(Decimal::ZERO);
                    let high = arr[2]
                        .as_f64()
                        .and_then(Decimal::from_f64_retain)
                        .unwrap_or(Decimal::ZERO);
                    let low = arr[3]
                        .as_f64()
                        .and_then(Decimal::from_f64_retain)
                        .unwrap_or(Decimal::ZERO);
                    let close = arr[4]
                        .as_f64()
                        .and_then(Decimal::from_f64_retain)
                        .unwrap_or(Decimal::ZERO);
                    let volume = arr[5]
                        .as_f64()
                        .and_then(Decimal::from_f64_retain)
                        .unwrap_or(Decimal::ZERO);

                    if close > Decimal::ZERO {
                        candles.push(Candle {
                            ts,
                            open,
                            high,
                            low,
                            close,
                            volume,
                        });
                    }
                }
            }
        }

        if candles.is_empty() {
            bail!("No valid candles parsed from Upstox response");
        }

        Ok(candles)
    }

    /// Enforce sandbox URL and host allowlist check.
    fn validate_url(&self, url: &str) -> Result<()> {
        let parsed = reqwest::Url::parse(url).with_context(|| format!("Invalid URL: {url}"))?;
        if parsed.host_str() != Some(ALLOWED_UPSTOX_HOST) {
            bail!(
                "CRITICAL SECURITY GATE: Blocked request to non-allowlisted host {:?}. Only {ALLOWED_UPSTOX_HOST} is permitted.",
                parsed.host_str()
            );
        }
        Ok(())
    }

    /// Place a simulated paper order. Live order placement to Upstox API is strictly blocked by construction.
    /// Orders are simulated locally using virtual cash and tracked in the paper portfolio.
    pub async fn place_order(&self, req: &UpstoxOrderRequestPayload) -> Result<String> {
        if req.quantity <= 0 {
            bail!(
                "Invalid order quantity {} — NSE cash equities require positive whole shares",
                req.quantity
            );
        }

        // All order routing is permanently isolated to the local paper simulator
        let mut state = self.mock_state.write().await;
        Self::execute_mock_order_internal(&mut state, req)
    }

    fn execute_mock_order_internal(
        state: &mut MockSandboxState,
        req: &UpstoxOrderRequestPayload,
    ) -> Result<String> {
        let order_id = format!("UPSTOX-SBX-{}", uuid::Uuid::new_v4().simple());

        let symbol = find_nse_instrument(&req.instrument_token)
            .map(|i| i.symbol)
            .unwrap_or_else(|| req.instrument_token.clone());

        let current_price = *state.prices.get(&symbol).unwrap_or(&req.price);
        let fill_price = if req.order_type == "MARKET" || req.price <= Decimal::ZERO {
            current_price
        } else {
            req.price
        };

        let notional = fill_price * Decimal::from(req.quantity);
        if req.transaction_type == "BUY" {
            if notional > state.cash {
                bail!(
                    "Order rejected: cost ₹{:.2} exceeds available INR cash ₹{:.2} (strictly isolated ₹1,000 India allocation)",
                    notional,
                    state.cash
                );
            }
            state.cash -= notional;
            let pos =
                state
                    .positions
                    .entry(symbol.clone())
                    .or_insert_with(|| UpstoxPositionRecord {
                        trading_symbol: symbol.clone(),
                        instrument_token: req.instrument_token.clone(),
                        quantity: 0,
                        buy_amount: Some(Decimal::ZERO),
                        sell_amount: Some(Decimal::ZERO),
                        buy_price: Some(fill_price),
                        sell_price: None,
                        last_price: Some(fill_price),
                        pnl: Some(Decimal::ZERO),
                        product: req.product.clone(),
                    });
            pos.quantity += req.quantity;
            pos.buy_amount = Some(pos.buy_amount.unwrap_or(Decimal::ZERO) + notional);
            pos.last_price = Some(fill_price);
        } else {
            state.cash += notional;
            if let Some(pos) = state.positions.get_mut(&symbol) {
                pos.quantity = (pos.quantity - req.quantity).max(0);
                pos.sell_amount = Some(pos.sell_amount.unwrap_or(Decimal::ZERO) + notional);
            }
        }

        let order_record = UpstoxOrderRecord {
            order_id: order_id.clone(),
            exchange: Some("NSE".to_string()),
            trading_symbol: Some(symbol.clone()),
            instrument_token: Some(req.instrument_token.clone()),
            status: "complete".to_string(),
            quantity: req.quantity,
            filled_quantity: Some(req.quantity),
            price: Some(req.price),
            average_price: Some(fill_price),
            order_timestamp: Some(chrono::Utc::now().to_rfc3339()),
            status_message: Some("Executed in Upstox Mock Sandbox".to_string()),
            tag: req.tag.clone(),
        };

        state.orders.insert(order_id.clone(), order_record);

        info!(
            order_id = %order_id,
            symbol = %symbol,
            qty = req.quantity,
            price = %fill_price,
            remaining_cash = %state.cash,
            "Placed order in Upstox Mock Sandbox"
        );
        Ok(order_id)
    }

    /// Retrieve order details.
    pub async fn get_order(&self, order_id: &str) -> Result<Option<UpstoxOrderRecord>> {
        if self.is_mock || order_id.starts_with("UPSTOX-SBX-") {
            let state = self.mock_state.read().await;
            return Ok(state.orders.get(order_id).cloned());
        }

        let url = format!("{UPSTOX_API_BASE}/order/history?order_id={order_id}");
        self.validate_url(&url)?;

        let mut headers = HeaderMap::new();
        if let Some(ref t) = self.token {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", t.expose_secret()))?,
            );
        }

        let resp = self.http.get(&url).headers(headers).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            let state = self.mock_state.read().await;
            return Ok(state.orders.get(order_id).cloned());
        }

        let body: UpstoxApiResponse<Vec<UpstoxOrderRecord>> = resp.json().await?;
        let maybe_order = body.data.and_then(|mut list| list.pop());
        if maybe_order.is_some() {
            return Ok(maybe_order);
        }

        let state = self.mock_state.read().await;
        Ok(state.orders.get(order_id).cloned())
    }

    /// Get current sandbox positions.
    /// Strictly isolated to the local simulated paper portfolio.
    pub async fn get_positions(&self) -> Result<Vec<UpstoxPositionRecord>> {
        let state = self.mock_state.read().await;
        Ok(state
            .positions
            .values()
            .filter(|p| p.quantity > 0)
            .cloned()
            .collect())
    }

    /// Get current sandbox funds and available margin in INR.
    /// Strictly isolated to the user's allocated virtual budget (₹1,000 max).
    pub async fn get_funds(&self) -> Result<Decimal> {
        let state = self.mock_state.read().await;
        Ok(state.cash.min(dec!(1000.0)))
    }

    /// Get live or simulated quote for an instrument key.
    pub async fn get_quote(&self, symbol: &str) -> Result<Decimal> {
        if self.is_mock {
            let state = self.mock_state.read().await;
            if let Some(p) = state.prices.get(symbol) {
                return Ok(*p);
            }
            return Ok(dec!(1000.00));
        }

        let nse = find_nse_instrument(symbol).context("Unknown NSE symbol")?;
        let url = format!(
            "{UPSTOX_API_BASE}/market-quote/quotes?instrument_key={}",
            urlencoding::encode(&nse.upstox_key)
        );
        self.validate_url(&url)?;

        let mut headers = HeaderMap::new();
        if let Some(ref t) = self.token {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", t.expose_secret()))?,
            );
        }

        if let Ok(resp) = self.http.get(&url).headers(headers).send().await {
            if let Ok(body) = resp
                .json::<UpstoxApiResponse<HashMap<String, UpstoxQuoteData>>>()
                .await
            {
                if let Some(ref errs) = body.errors {
                    if let Some(e) = errs.first() {
                        warn!("Upstox quote API error for {symbol}: {}", e.message);
                    }
                }
                if let Some(price) = body
                    .data
                    .and_then(|map| map.into_values().next())
                    .map(|q| q.last_price)
                {
                    if price > Decimal::ZERO {
                        return Ok(price);
                    }
                }
            }
        }

        let state = self.mock_state.read().await;
        if let Some(p) = state.prices.get(symbol) {
            return Ok(*p);
        }
        Ok(dec!(1000.00))
    }
}
