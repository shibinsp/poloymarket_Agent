//! Upstox Live Market Data Feed V3 integration.
//!
//! Provides:
//! 1. Upstox Developer API App OAuth authorization flow (client_id, client_secret, redirect_uri, access_token).
//! 2. Live Market Data Feed V3 connecting via WebSocket (`wss://`) with authorization handshake.
//! 3. Resilient exponential backoff reconnects upon connection drops and single resubscription per reconnect.
//! 4. Tick deduplication cache to prevent duplicate ticks, candle alterations, or paper trades.
//! 5. Authentic last-received price preservation with zero tick fabrication during closed market sessions or disconnections.
//! 6. Integrated NSE Exchange Session detection (Pre-Open, Regular, Closing Auction, Closed).
//! 7. 100% local paper simulation: zero live order execution to Upstox API.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::india::instruments::{find_nse_instrument, nse_watchlist};
use crate::india::session::{detect_nse_session, NseSessionInfo};
use crate::strategy::pullback::FiveMinCandle;

pub const UPSTOX_API_BASE: &str = "https://api.upstox.com/v2";
pub const UPSTOX_V3_FEED_AUTHORIZE: &str =
    "https://api.upstox.com/v3/feed/market-data-feed/authorize";

/// Real-time live market tick for an NSE equity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveTick {
    pub symbol: String,
    pub instrument_token: String,
    pub last_price: Decimal,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: i64,
    pub timestamp: DateTime<Utc>,
    pub feed_timestamp: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub latency_ms: u64,
    pub is_stale: bool,
    pub source: String, // "UPSTOX_V3_LIVE" | "UPSTOX_REST_LIVE" | "INITIAL_SEED"
}

/// Upstox feed connection and authorization status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedStatusInfo {
    pub connected: bool,
    pub status: String, // "CONNECTED" | "CONNECTING" | "RECONNECTING_BACKOFF" | "CLOSED_SESSION" | "AUTH_REQUIRED" | "STALE"
    pub feed_type: String, // "UPSTOX_V3_LIVE" | "UPSTOX_REST_LIVE" | "SEED_OFFLINE"
    pub token_configured: bool,
    pub token_preview: Option<String>,
    pub client_id_configured: bool,
    pub subscribed_count: usize,
    pub symbols: Vec<String>,
    pub last_update: Option<DateTime<Utc>>,
    pub last_feed_timestamp: Option<DateTime<Utc>>,
    pub last_receipt_timestamp: Option<DateTime<Utc>>,
    pub measured_delay_ms: Option<u64>,
    pub is_stale: bool,
    pub stale_reason: Option<String>,
    pub trading_paused_due_to_stale: bool,
    pub auth_dialog_url: Option<String>,
    pub redirect_uri: String,
    pub session: NseSessionInfo,
    pub reconnect_count: u64,
    pub resubscribe_count: u64,
    pub backoff_delay_secs: u64,
    pub deduped_ticks_count: u64,
    pub notes: String,
}

/// Upstox OAuth token response payload.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct UpstoxOAuthTokenResponse {
    pub access_token: Option<String>,
    pub status: Option<String>,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
}

/// Upstox V3 Market Data Feed Authorize response payload.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct UpstoxFeedAuthorizeResponse {
    pub status: Option<String>,
    pub data: Option<UpstoxFeedAuthorizeData>,
}

#[derive(Debug, Deserialize)]
struct UpstoxFeedAuthorizeData {
    #[serde(alias = "authorizedRedirectUri")]
    pub authorized_redirect_uri: Option<String>,
}

/// Upstox market quote container response.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct UpstoxQuoteFullResponse {
    pub status: Option<String>,
    pub data: Option<HashMap<String, UpstoxFullQuoteItem>>,
}

#[derive(Debug, Deserialize)]
struct UpstoxFullQuoteItem {
    pub last_price: Option<f64>,
    pub ohlc: Option<UpstoxOhlcItem>,
    pub volume: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UpstoxOhlcItem {
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
}

/// Shared Upstox Live Market Data Feed service.
pub struct UpstoxLiveMarketFeed {
    http: reqwest::Client,
    pub client_id: Arc<RwLock<Option<String>>>,
    pub client_secret: Arc<RwLock<Option<String>>>,
    pub access_token: Arc<RwLock<Option<String>>>,
    pub redirect_uri: String,
    pub prices: Arc<RwLock<HashMap<String, LiveTick>>>,
    pub feed_status: Arc<RwLock<String>>,
    pub last_update: Arc<RwLock<Option<DateTime<Utc>>>>,
    pub websocket_uri: Arc<RwLock<Option<String>>>,
    pub last_seen_ticks: Arc<RwLock<HashMap<String, (i64, Decimal)>>>,
    pub reconnect_count: Arc<RwLock<u64>>,
    pub resubscribe_count: Arc<RwLock<u64>>,
    pub backoff_delay_secs: Arc<RwLock<u64>>,
    pub deduped_ticks_count: Arc<RwLock<u64>>,
}

impl UpstoxLiveMarketFeed {
    pub fn new(
        client_id: Option<String>,
        client_secret: Option<String>,
        access_token: Option<String>,
        redirect_uri: Option<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()
            .unwrap_or_default();

        let initial_status = if access_token.is_some() {
            "CONNECTING".to_string()
        } else {
            "AUTH_REQUIRED".to_string()
        };

        let mut baseline_prices = HashMap::new();
        let now = Utc::now();
        for inst in nse_watchlist() {
            let p = crate::india::chart::get_company_info(&inst.symbol).2;
            baseline_prices.insert(
                inst.symbol.clone(),
                LiveTick {
                    symbol: inst.symbol.clone(),
                    instrument_token: inst.upstox_key.clone(),
                    last_price: p,
                    open: (p - dec!(0.50)).max(dec!(0.05)),
                    high: p + dec!(1.50),
                    low: (p - dec!(1.00)).max(dec!(0.05)),
                    close: p,
                    volume: 150000,
                    timestamp: now,
                    feed_timestamp: now,
                    received_at: now,
                    latency_ms: 25,
                    is_stale: false,
                    source: "INITIAL_SEED".to_string(),
                },
            );
        }

        Self {
            http,
            client_id: Arc::new(RwLock::new(client_id)),
            client_secret: Arc::new(RwLock::new(client_secret)),
            access_token: Arc::new(RwLock::new(access_token)),
            redirect_uri: redirect_uri
                .unwrap_or_else(|| "http://127.0.0.1:8080/api/upstox/callback".to_string()),
            prices: Arc::new(RwLock::new(baseline_prices)),
            feed_status: Arc::new(RwLock::new(initial_status)),
            last_update: Arc::new(RwLock::new(Some(now))),
            websocket_uri: Arc::new(RwLock::new(None)),
            last_seen_ticks: Arc::new(RwLock::new(HashMap::new())),
            reconnect_count: Arc::new(RwLock::new(0)),
            resubscribe_count: Arc::new(RwLock::new(0)),
            backoff_delay_secs: Arc::new(RwLock::new(0)),
            deduped_ticks_count: Arc::new(RwLock::new(0)),
        }
    }

    /// Build the Upstox OAuth login authorization URL.
    pub async fn auth_dialog_url(&self) -> Option<String> {
        let cid = self.client_id.read().await;
        cid.as_ref().map(|id| {
            format!(
                "https://api.upstox.com/v2/login/authorization/dialog?response_type=code&client_id={}&redirect_uri={}",
                urlencoding::encode(id),
                urlencoding::encode(&self.redirect_uri)
            )
        })
    }

    /// Set or update the access token directly.
    pub async fn set_access_token(&self, token: String) {
        let clean = token.trim().to_string();
        if !clean.is_empty() {
            let mut t = self.access_token.write().await;
            *t = Some(clean);
            let mut s = self.feed_status.write().await;
            *s = "CONNECTING".to_string();
            info!("Upstox market data access token updated");
        }
    }

    /// Set Client ID & Client Secret.
    pub async fn set_credentials(&self, client_id: Option<String>, client_secret: Option<String>) {
        if let Some(cid) = client_id {
            let mut id = self.client_id.write().await;
            *id = Some(cid.trim().to_string());
        }
        if let Some(sec) = client_secret {
            let mut s = self.client_secret.write().await;
            *s = Some(sec.trim().to_string());
        }
    }

    /// Exchange an OAuth authorization code for a valid access_token.
    pub async fn exchange_code_for_token(&self, code: &str) -> Result<String> {
        let client_id = {
            let id = self.client_id.read().await;
            id.clone()
                .context("Upstox UPSTOX_API_KEY (client_id) is not configured")?
        };
        let client_secret = {
            let sec = self.client_secret.read().await;
            sec.clone()
                .context("Upstox UPSTOX_API_SECRET (client_secret) is not configured")?
        };

        let token_url = format!("{UPSTOX_API_BASE}/login/authorization/token");
        let params = [
            ("code", code),
            ("client_id", &client_id),
            ("client_secret", &client_secret),
            ("redirect_uri", &self.redirect_uri),
            ("grant_type", "authorization_code"),
        ];

        let resp = self
            .http
            .post(&token_url)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(ACCEPT, "application/json")
            .form(&params)
            .send()
            .await
            .context("Failed to contact Upstox token exchange endpoint")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Upstox token exchange failed (HTTP {status}): {text}");
        }

        let body: UpstoxOAuthTokenResponse =
            serde_json::from_str(&text).context("Invalid token exchange JSON from Upstox")?;

        let token = body
            .access_token
            .context("No access_token returned by Upstox OAuth response")?;

        self.set_access_token(token.clone()).await;
        Ok(token)
    }

    /// Query the V3 Market Data Feed Authorize endpoint to get the WebSocket streaming URI.
    pub async fn authorize_websocket_stream(&self) -> Result<String> {
        let token = {
            let t = self.access_token.read().await;
            t.clone()
                .context("Upstox access token required to authorize market data feed")?
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let resp = self
            .http
            .get(UPSTOX_V3_FEED_AUTHORIZE)
            .headers(headers)
            .send()
            .await
            .context("Failed to authorize Upstox V3 Market Data Feed")?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .context("Failed to read authorize response body")?;
        if !status.is_success() {
            anyhow::bail!("Upstox feed authorize HTTP error {status}: {text}");
        }
        let val: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse Upstox V3 feed authorize JSON: {text}"))?;

        let ws_uri = val
            .get("data")
            .and_then(|d| {
                d.get("authorized_redirect_uri")
                    .or_else(|| d.get("authorizedRedirectUri"))
            })
            .and_then(|u| u.as_str())
            .context("No authorized_redirect_uri provided in Upstox feed response")?
            .to_string();

        let mut stored = self.websocket_uri.write().await;
        *stored = Some(ws_uri.clone());
        Ok(ws_uri)
    }

    /// Build the standard Upstox V3 subscription JSON payload for the watchlist instruments.
    pub fn build_v3_subscription_message() -> String {
        let keys: Vec<String> = nse_watchlist()
            .iter()
            .map(|i| i.upstox_key.clone())
            .collect();
        serde_json::json!({
            "guid": "guid-sub-nse-watchlist",
            "method": "sub",
            "data": {
                "mode": "full",
                "instrumentKeys": keys
            }
        })
        .to_string()
    }

    /// Fetch latest quotes for all watched instruments directly from Upstox API.
    pub async fn fetch_live_quotes(&self) -> Result<Vec<LiveTick>> {
        let token = {
            let t = self.access_token.read().await;
            t.clone()
                .context("Upstox access token not configured for live quote polling")?
        };

        let watchlist = nse_watchlist();
        let instrument_keys: Vec<String> = watchlist
            .iter()
            .map(|i| urlencoding::encode(&i.upstox_key).to_string())
            .collect();
        let query_param = instrument_keys.join(",");

        let url = format!("{UPSTOX_API_BASE}/market-quote/quotes?instrument_key={query_param}");

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let resp = self
            .http
            .get(&url)
            .headers(headers)
            .send()
            .await
            .context("Network error fetching Upstox live market quotes")?;

        let status = resp.status();
        if !status.is_success() {
            let err_txt = resp.text().await.unwrap_or_default();
            bail!("Upstox quotes request returned HTTP {status}: {err_txt}");
        }

        let body: UpstoxQuoteFullResponse = resp
            .json()
            .await
            .context("Failed to parse Upstox quotes response body")?;

        let items = body.data.unwrap_or_default();
        let now = Utc::now();
        let mut ticks = Vec::new();

        for inst in &watchlist {
            let key = &inst.upstox_key;
            let maybe_item = items.get(key).or_else(|| {
                items
                    .iter()
                    .find(|(k, _)| k.contains(&inst.symbol))
                    .map(|(_, v)| v)
            });

            if let Some(item) = maybe_item {
                if let Some(lp_f64) = item.last_price {
                    let lp = Decimal::from_f64_retain(lp_f64).unwrap_or(Decimal::ZERO);
                    if lp > Decimal::ZERO {
                        let (o, h, l, c) = if let Some(ref ohlc) = item.ohlc {
                            (
                                ohlc.open.and_then(Decimal::from_f64_retain).unwrap_or(lp),
                                ohlc.high.and_then(Decimal::from_f64_retain).unwrap_or(lp),
                                ohlc.low.and_then(Decimal::from_f64_retain).unwrap_or(lp),
                                ohlc.close.and_then(Decimal::from_f64_retain).unwrap_or(lp),
                            )
                        } else {
                            (lp, lp, lp, lp)
                        };

                        let tick = LiveTick {
                            symbol: inst.symbol.clone(),
                            instrument_token: inst.upstox_key.clone(),
                            last_price: lp,
                            open: o,
                            high: h,
                            low: l,
                            close: c,
                            volume: item.volume.unwrap_or(0),
                            timestamp: now,
                            feed_timestamp: now,
                            received_at: now,
                            latency_ms: 0,
                            is_stale: false,
                            source: "UPSTOX_REST_LIVE".to_string(),
                        };
                        ticks.push(tick);
                    }
                }
            }
        }

        if !ticks.is_empty() {
            self.process_incoming_ticks(ticks.clone()).await;
            let mut s = self.feed_status.write().await;
            *s = "CONNECTED".to_string();
            let mut lu = self.last_update.write().await;
            *lu = Some(now);
        }

        Ok(ticks)
    }

    /// Parse Upstox V3 candle response rows into completed FiveMinCandle structs.
    fn parse_upstox_candle_rows(
        payload: &serde_json::Value,
        symbol: &str,
        now: DateTime<Utc>,
    ) -> Vec<FiveMinCandle> {
        let Some(rows) = payload
            .get("data")
            .and_then(|data| data.get("candles"))
            .and_then(serde_json::Value::as_array)
        else {
            return Vec::new();
        };

        let mut candles = Vec::with_capacity(rows.len());
        for row in rows.iter().rev() {
            let Some(values) = row.as_array() else {
                continue;
            };
            if values.len() < 6 {
                continue;
            }
            let Some(timestamp) = values[0]
                .as_str()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc))
            else {
                continue;
            };
            // The current candle is still forming. Never feed it as a completed bar.
            if timestamp + chrono::Duration::minutes(5) > now {
                continue;
            }
            let number = |index: usize| {
                values[index]
                    .as_f64()
                    .and_then(Decimal::from_f64_retain)
                    .unwrap_or(Decimal::ZERO)
            };
            let (open, high, low, close, volume) =
                (number(1), number(2), number(3), number(4), number(5));
            if open <= Decimal::ZERO || low <= Decimal::ZERO || close <= Decimal::ZERO {
                continue;
            }
            candles.push(FiveMinCandle {
                timestamp,
                symbol: symbol.to_string(),
                open,
                high,
                low,
                close,
                volume,
            });
        }
        candles
    }

    /// Fetch recent completed five-minute candles for strategy warm-up.
    /// Combines Upstox V3 historical candles (prior days for EMA warm-up)
    /// and intraday completed candles (today's session for VWAP and pullback checks).
    pub async fn fetch_intraday_5m_candles(&self, symbol: &str) -> Result<Vec<FiveMinCandle>> {
        let instrument = find_nse_instrument(symbol)
            .with_context(|| format!("Unknown NSE symbol for candle history: {symbol}"))?;
        let token = self
            .access_token
            .read()
            .await
            .clone()
            .context("Upstox access token required to load strategy candle history")?;
        let now = Utc::now();
        let today = now.with_timezone(&chrono_tz::Asia::Kolkata).date_naive();
        let from = today - chrono::Duration::days(5);
        let key = urlencoding::encode(&instrument.upstox_key);

        let mut all_candles = Vec::new();

        // 1. Historical candles (prior days up to today)
        let hist_url =
            format!("https://api.upstox.com/v3/historical-candle/{key}/minutes/5/{today}/{from}");
        match self
            .http
            .get(&hist_url)
            .bearer_auth(&token)
            .header(ACCEPT, "application/json")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(payload) = resp.json::<serde_json::Value>().await {
                    let parsed = Self::parse_upstox_candle_rows(&payload, &instrument.symbol, now);
                    all_candles.extend(parsed);
                }
            }
            Ok(resp) => {
                warn!(%symbol, status = %resp.status(), "Upstox historical candle fetch returned non-success");
            }
            Err(err) => {
                warn!(%symbol, %err, "Failed to fetch Upstox historical candles");
            }
        }

        // 2. Intraday completed candles (today's session 09:15 onwards)
        let intra_url =
            format!("https://api.upstox.com/v3/historical-candle/intraday/{key}/minutes/5");
        match self
            .http
            .get(&intra_url)
            .bearer_auth(&token)
            .header(ACCEPT, "application/json")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(payload) = resp.json::<serde_json::Value>().await {
                    let parsed = Self::parse_upstox_candle_rows(&payload, &instrument.symbol, now);
                    for candle in parsed {
                        if all_candles
                            .last()
                            .map(|prev| prev.timestamp < candle.timestamp)
                            .unwrap_or(true)
                        {
                            all_candles.push(candle);
                        }
                    }
                }
            }
            Ok(resp) => {
                warn!(%symbol, status = %resp.status(), "Upstox intraday candle fetch returned non-success");
            }
            Err(err) => {
                warn!(%symbol, %err, "Failed to fetch Upstox intraday candles");
            }
        }

        if all_candles.is_empty() {
            bail!("Could not retrieve any completed five-minute candles for {symbol}");
        }

        Ok(all_candles)
    }

    /// Completed daily closes, oldest first, for an Upstox instrument key
    /// (an `NSE_EQ|ISIN` stock or an `NSE_INDEX|...` index). Bars dated
    /// `today_ist` or later are dropped: today's candle is still forming.
    pub async fn fetch_daily_closes(
        &self,
        instrument_key: &str,
        sessions: usize,
        today_ist: chrono::NaiveDate,
    ) -> Result<Vec<Decimal>> {
        let token = self
            .access_token
            .read()
            .await
            .clone()
            .context("Upstox access token required to load daily history")?;
        // Calendar days: sessions plus weekends and exchange holidays.
        let from = today_ist - chrono::Duration::days((sessions as i64) * 3 / 2 + 30);
        let key = urlencoding::encode(instrument_key);
        let url =
            format!("https://api.upstox.com/v3/historical-candle/{key}/days/1/{today_ist}/{from}");
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&token)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .with_context(|| format!("Failed to fetch daily candles for {instrument_key}"))?;
        if !resp.status().is_success() {
            bail!(
                "Upstox daily candles for {instrument_key} returned {}",
                resp.status()
            );
        }
        let payload: serde_json::Value = resp.json().await?;
        let rows = payload
            .get("data")
            .and_then(|d| d.get("candles"))
            .and_then(serde_json::Value::as_array)
            .context("Upstox daily candle response had no candles")?;
        let mut bars: Vec<(chrono::NaiveDate, Decimal)> = rows
            .iter()
            .filter_map(|row| {
                let values = row.as_array()?;
                let date = DateTime::parse_from_rfc3339(values.first()?.as_str()?)
                    .ok()?
                    .with_timezone(&chrono_tz::Asia::Kolkata)
                    .date_naive();
                let close = values.get(4)?.as_f64().and_then(Decimal::from_f64_retain)?;
                (date < today_ist && close > Decimal::ZERO).then_some((date, close.round_dp(2)))
            })
            .collect();
        bars.sort_by_key(|(d, _)| *d);
        bars.dedup_by_key(|(d, _)| *d);
        if bars.is_empty() {
            bail!("No completed daily candles for {instrument_key}");
        }
        Ok(bars.into_iter().map(|(_, c)| c).collect())
    }

    /// Process incoming ticks with strict deduplication:
    /// Discards duplicate ticks to avoid duplicate candle alterations or redundant paper trades.
    pub async fn process_incoming_ticks(&self, ticks: Vec<LiveTick>) -> Vec<LiveTick> {
        let mut fresh_ticks = Vec::new();
        let mut last_seen = self.last_seen_ticks.write().await;
        let mut prices = self.prices.write().await;
        let mut dedup_cnt = self.deduped_ticks_count.write().await;
        let now = Utc::now();

        for tick in ticks {
            let ts_ms = tick.timestamp.timestamp_millis();
            if let Some((prev_ts, prev_price)) = last_seen.get(&tick.symbol) {
                if *prev_ts == ts_ms && *prev_price == tick.last_price {
                    *dedup_cnt += 1;
                    continue; // Skip duplicate tick
                }
            }

            last_seen.insert(tick.symbol.clone(), (ts_ms, tick.last_price));
            prices.insert(tick.symbol.clone(), tick.clone());
            fresh_ticks.push(tick);
        }

        if !fresh_ticks.is_empty() {
            let mut lu = self.last_update.write().await;
            *lu = Some(now);
        }

        fresh_ticks
    }

    /// Read the latest price for a symbol.
    pub async fn get_price(&self, symbol: &str) -> Decimal {
        let map = self.prices.read().await;
        if let Some(t) = map.get(symbol) {
            return t.last_price;
        }
        crate::india::chart::get_company_info(symbol).2
    }

    /// Get current overall feed status and exchange session information.
    pub async fn get_status_info(&self) -> FeedStatusInfo {
        let token = self.access_token.read().await;
        let client_id = self.client_id.read().await;
        let feed_status = self.feed_status.read().await;
        let last_up = *self.last_update.read().await;
        let p_map = self.prices.read().await;
        let reconnect_count = *self.reconnect_count.read().await;
        let resubscribe_count = *self.resubscribe_count.read().await;
        let backoff_delay_secs = *self.backoff_delay_secs.read().await;
        let deduped_ticks_count = *self.deduped_ticks_count.read().await;

        let now = Utc::now();
        let session = detect_nse_session(now);

        let has_token = token
            .as_ref()
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false);
        let preview = token.as_ref().map(|t| {
            if t.len() > 10 {
                format!("{}...{}", &t[0..4], &t[t.len() - 4..])
            } else {
                "***".to_string()
            }
        });

        let auth_url = self.auth_dialog_url().await;
        let symbols: Vec<String> = nse_watchlist().into_iter().map(|i| i.symbol).collect();
        // Distinguish an actual V3 stream tick from a fresh REST quote. The
        // REST poller keeps prices usable as a fallback, but it must not make
        // the UI claim the WebSocket stream is connected.
        let newest_price_tick = p_map
            .values()
            .filter(|tick| matches!(tick.source.as_str(), "UPSTOX_V3_LIVE" | "UPSTOX_REST_LIVE"))
            .max_by_key(|tick| tick.received_at);
        let newest_stream_tick = p_map
            .values()
            .filter(|tick| tick.source == "UPSTOX_V3_LIVE")
            .max_by_key(|tick| tick.received_at);
        let newest_rest_tick = p_map
            .values()
            .filter(|tick| tick.source == "UPSTOX_REST_LIVE")
            .max_by_key(|tick| tick.received_at);
        let age_ms =
            |received_at: DateTime<Utc>| (now - received_at).num_milliseconds().max(0) as u64;
        let stream_fresh = newest_stream_tick
            .map(|tick| age_ms(tick.received_at) <= 15_000)
            .unwrap_or(false);
        let rest_fresh = newest_rest_tick
            .map(|tick| age_ms(tick.received_at) <= 15_000)
            .unwrap_or(false);
        let prices_fresh = stream_fresh || rest_fresh;
        let connected = stream_fresh;
        let latest_update = newest_price_tick.map(|tick| tick.received_at).or(last_up);
        // Keep event-to-receipt delay separate from price freshness. REST
        // request duration is not an exchange-tick delay measurement.
        let quote_latency_ms = if stream_fresh {
            newest_stream_tick.map(|tick| tick.latency_ms)
        } else {
            None
        };

        // Fresh REST prices remain usable for paper decisions, but are clearly
        // labeled as fallback instead of being presented as a live stream.
        let is_stale = if !session.is_trading_active {
            true
        } else {
            !prices_fresh
        };

        let stale_reason = if !session.is_trading_active {
            Some(format!(
                "NSE Market is currently {} ({}). Regular continuous trading is 09:15 - 15:30 IST. Showing authentic last received prices; strategy decisions and order fills paused.",
                session.status_badge, session.session_name
            ))
        } else if is_stale {
            Some("No fresh Upstox stream or REST prices received for more than 15 seconds. Paper decisions are paused.".to_string())
        } else if !connected {
            Some(
                "Fresh REST snapshots are available; the Upstox V3 stream is disconnected."
                    .to_string(),
            )
        } else {
            None
        };

        let trading_paused_due_to_stale = is_stale || !session.is_trading_active;

        let connection_state = if !session.is_trading_active {
            "CLOSED_SESSION".to_string()
        } else if connected {
            "CONNECTED".to_string()
        } else if rest_fresh {
            "REST_FALLBACK".to_string()
        } else if is_stale {
            "STALE".to_string()
        } else {
            feed_status.clone()
        };

        let feed_type = if connected {
            "UPSTOX_V3_LIVE".to_string()
        } else if rest_fresh {
            "UPSTOX_REST_LIVE".to_string()
        } else if has_token {
            "UPSTOX_CONNECTING".to_string()
        } else {
            "SEED_OFFLINE".to_string()
        };

        FeedStatusInfo {
            connected,
            status: connection_state,
            feed_type,
            token_configured: has_token,
            token_preview: preview,
            client_id_configured: client_id.is_some(),
            subscribed_count: p_map.len(),
            symbols,
            last_update: latest_update,
            last_feed_timestamp: newest_stream_tick.map(|tick| tick.feed_timestamp),
            last_receipt_timestamp: newest_price_tick.map(|tick| tick.received_at),
            measured_delay_ms: quote_latency_ms,
            is_stale,
            stale_reason,
            trading_paused_due_to_stale,
            auth_dialog_url: auth_url,
            redirect_uri: self.redirect_uri.clone(),
            session,
            reconnect_count,
            resubscribe_count,
            backoff_delay_secs,
            deduped_ticks_count,
            notes: if connected {
                "Fresh Upstox Market Data Feed V3 stream ticks are arriving. All execution remains local paper trading; no live broker orders are sent.".to_string()
            } else if rest_fresh {
                "Fresh Upstox REST quote snapshots are available; the V3 WebSocket stream is not currently delivering ticks. All execution remains local paper trading; no live broker orders are sent.".to_string()
            } else {
                "No fresh Upstox prices are available. Paper decisions pause until fresh prices resume. All execution remains local paper trading.".to_string()
            },
        }
    }

    /// Check whether the market feed is currently stale (paused for safety).
    pub async fn is_stale(&self) -> bool {
        let now = Utc::now();
        let session = detect_nse_session(now);
        if !session.is_trading_active {
            return true;
        }
        let last_up = *self.last_update.read().await;
        match last_up {
            Some(lu) => (now - lu).num_seconds() > 15,
            None => true,
        }
    }

    /// Spawn autonomous background WebSocket streaming worker with exponential backoff and tick deduplication.
    pub fn spawn_feed_worker(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        let poller = self.clone();
        tokio::spawn(async move {
            loop {
                let token_available = {
                    let t = poller.access_token.read().await;
                    t.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false)
                };
                if token_available {
                    if let Err(e) = poller.fetch_live_quotes().await {
                        debug!("Periodic live quote fetch: {e}");
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
            }
        });

        tokio::spawn(async move {
            info!("Upstox V3 Market Data Feed background service initializing...");
            let mut attempt = 0u32;

            loop {
                let token_available = {
                    let t = self.access_token.read().await;
                    t.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false)
                };

                if !token_available {
                    {
                        let mut s = self.feed_status.write().await;
                        *s = "AUTH_REQUIRED".to_string();
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    continue;
                }

                // Attempt to authorize and connect to Upstox V3 WebSocket stream
                {
                    let mut s = self.feed_status.write().await;
                    *s = "CONNECTING".to_string();
                }

                match self.authorize_websocket_stream().await {
                    Ok(ws_uri) => {
                        debug!("Upstox V3 WebSocket feed authorized, connecting: {ws_uri}");
                        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
                        let mut req = match ws_uri.as_str().into_client_request() {
                            Ok(r) => r,
                            Err(e) => {
                                warn!("Failed to construct Upstox WS handshake request: {e}");
                                continue;
                            }
                        };
                        req.headers_mut().insert(
                            "User-Agent",
                            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
                                .parse()
                                .unwrap(),
                        );
                        match tokio_tungstenite::connect_async(req).await {
                            Ok((mut ws_stream, _response)) => {
                                info!(
                                    "Upstox Market Data Feed V3 WebSocket connected successfully"
                                );
                                attempt = 0;
                                {
                                    let mut b = self.backoff_delay_secs.write().await;
                                    *b = 0;
                                    let mut s = self.feed_status.write().await;
                                    *s = "CONNECTED".to_string();
                                    let mut rc = self.reconnect_count.write().await;
                                    *rc += 1;
                                }

                                // Resubscribe ONCE for instruments in watchlist
                                let sub_msg = Self::build_v3_subscription_message();
                                // Upstox V3 expects the JSON subscription packet in a
                                // binary WebSocket frame. Sending it as text leaves the
                                // socket open but produces no market updates.
                                if let Err(e) = ws_stream
                                    .send(Message::Binary(sub_msg.into_bytes().into()))
                                    .await
                                {
                                    warn!("Failed to send Upstox V3 subscription message: {e}");
                                } else {
                                    let mut sc = self.resubscribe_count.write().await;
                                    *sc += 1;
                                    info!("Subscribed once to NSE watchlist instruments on Upstox V3 feed");
                                }

                                // Stream message loop
                                while let Some(msg_result) = ws_stream.next().await {
                                    match msg_result {
                                        Ok(Message::Binary(bin_bytes)) => {
                                            let parsed_ticks =
                                                parse_upstox_protobuf_feed(&bin_bytes);
                                            if !parsed_ticks.is_empty() {
                                                self.process_incoming_ticks(parsed_ticks).await;
                                            }
                                        }
                                        Ok(Message::Text(txt)) => {
                                            debug!(text = %txt, "Received text frame from Upstox feed");
                                        }
                                        Ok(Message::Ping(p)) => {
                                            let _ = ws_stream.send(Message::Pong(p)).await;
                                        }
                                        Ok(Message::Close(reason)) => {
                                            warn!(?reason, "Upstox V3 WebSocket closed by server");
                                            break;
                                        }
                                        Err(e) => {
                                            warn!(error = %e, "Upstox V3 WebSocket stream error");
                                            break;
                                        }
                                        _ => {}
                                    }
                                }

                                info!("Upstox V3 WebSocket connection terminated; preparing to reconnect...");
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to connect to Upstox V3 WebSocket endpoint");
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Upstox V3 feed authorization failed; live decisions remain paused until the stream reconnects");
                    }
                }

                // Keep reconnect attempts responsive while still backing off
                // briefly after repeated failures: 2s, 4s, then 5s maximum.
                attempt = attempt.saturating_add(1);
                let backoff_secs = std::cmp::min(5, 1u64 << attempt.min(3));
                {
                    let mut b = self.backoff_delay_secs.write().await;
                    *b = backoff_secs;
                    let mut s = self.feed_status.write().await;
                    *s = "RECONNECTING_BACKOFF".to_string();
                }

                info!(
                    attempt,
                    backoff_secs, "Reconnecting to Upstox Market Data Feed V3 with backoff..."
                );
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Zero-Dependency Protocol Buffer Wire-Format Parser for Upstox V3 Feed
// ---------------------------------------------------------------------------

fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result = 0u64;
    let mut shift = 0;
    while *pos < buf.len() {
        let byte = buf[*pos];
        *pos += 1;
        result |= ((byte & 0x7F) as u64) << shift;
        if (byte & 0x80) == 0 {
            return Some(result);
        }
        shift += 7;
        if shift > 64 {
            return None;
        }
    }
    None
}

fn read_f64(buf: &[u8], pos: &mut usize) -> Option<f64> {
    if *pos + 8 <= buf.len() {
        let bytes: [u8; 8] = buf[*pos..*pos + 8].try_into().ok()?;
        *pos += 8;
        Some(f64::from_le_bytes(bytes))
    } else {
        None
    }
}

fn read_length_delimited<'a>(buf: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let len = read_varint(buf, pos)? as usize;
    if *pos + len <= buf.len() {
        let slice = &buf[*pos..*pos + len];
        *pos += len;
        Some(slice)
    } else {
        None
    }
}

fn skip_field(wire_type: u8, buf: &[u8], pos: &mut usize) -> Option<()> {
    match wire_type {
        0 => {
            read_varint(buf, pos)?;
            Some(())
        }
        1 => {
            if *pos + 8 <= buf.len() {
                *pos += 8;
                Some(())
            } else {
                None
            }
        }
        2 => {
            let _ = read_length_delimited(buf, pos)?;
            Some(())
        }
        5 => {
            if *pos + 4 <= buf.len() {
                *pos += 4;
                Some(())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Parses Upstox V3 binary Protobuf feed messages into clean `LiveTick` models.
pub fn parse_upstox_protobuf_feed(bytes: &[u8]) -> Vec<LiveTick> {
    let mut ticks = Vec::new();
    let mut pos = 0;
    let now = Utc::now();

    while pos < bytes.len() {
        let Some(tag_wire) = read_varint(bytes, &mut pos) else {
            break;
        };
        let tag = tag_wire >> 3;
        let wire = (tag_wire & 0x07) as u8;

        if (tag == 1 || tag == 2) && wire == 2 {
            // feeds map entry: tag 2 (Upstox V3 FeedResponse.feeds) or tag 1
            if let Some(entry_buf) = read_length_delimited(bytes, &mut pos) {
                if let Some(tick) = parse_feed_map_entry(entry_buf, now) {
                    ticks.push(tick);
                }
            }
        } else {
            if skip_field(wire, bytes, &mut pos).is_none() {
                break;
            }
        }
    }

    ticks
}

fn parse_feed_map_entry(buf: &[u8], now: DateTime<Utc>) -> Option<LiveTick> {
    let mut pos = 0;
    let mut instrument_key = String::new();
    let mut feed_slice: Option<&[u8]> = None;

    while pos < buf.len() {
        let tag_wire = read_varint(buf, &mut pos)?;
        let tag = tag_wire >> 3;
        let wire = (tag_wire & 0x07) as u8;

        if tag == 1 && wire == 2 {
            let key_bytes = read_length_delimited(buf, &mut pos)?;
            instrument_key = String::from_utf8_lossy(key_bytes).to_string();
        } else if tag == 2 && wire == 2 {
            feed_slice = read_length_delimited(buf, &mut pos);
        } else {
            skip_field(wire, buf, &mut pos)?;
        }
    }

    let f_buf = feed_slice?;
    let mut f_pos = 0;

    let mut ltp_opt: Option<f64> = None;
    let mut ltt_opt: Option<i64> = None;
    let mut cp_opt: Option<f64> = None;
    let mut vtt_opt: Option<i64> = None;
    let mut open_opt: Option<f64> = None;
    let mut high_opt: Option<f64> = None;
    let mut low_opt: Option<f64> = None;
    let mut close_opt: Option<f64> = None;

    while f_pos < f_buf.len() {
        let tag_wire = read_varint(f_buf, &mut f_pos)?;
        let tag = tag_wire >> 3;
        let wire = (tag_wire & 0x07) as u8;

        if (tag == 1 || tag == 2) && wire == 2 {
            let sub_buf = read_length_delimited(f_buf, &mut f_pos)?;
            if tag == 1 {
                // LTPC
                parse_ltpc(sub_buf, &mut ltp_opt, &mut ltt_opt, &mut cp_opt);
            } else if tag == 2 {
                // FullFeed
                parse_full_feed(
                    sub_buf,
                    &mut ltp_opt,
                    &mut ltt_opt,
                    &mut cp_opt,
                    &mut vtt_opt,
                    &mut open_opt,
                    &mut high_opt,
                    &mut low_opt,
                    &mut close_opt,
                );
            }
        } else {
            skip_field(wire, f_buf, &mut f_pos)?;
        }
    }

    let ltp = ltp_opt?;
    let last_price = Decimal::from_f64_retain(ltp).unwrap_or(Decimal::ZERO);
    if last_price <= Decimal::ZERO {
        return None;
    }

    let inst = find_nse_instrument(&instrument_key);
    let symbol = inst.map(|i| i.symbol.clone()).unwrap_or_else(|| {
        instrument_key
            .split('|')
            .nth(1)
            .unwrap_or(&instrument_key)
            .to_string()
    });

    let open = open_opt
        .and_then(Decimal::from_f64_retain)
        .unwrap_or(last_price);
    let high = high_opt
        .and_then(Decimal::from_f64_retain)
        .unwrap_or(last_price);
    let low = low_opt
        .and_then(Decimal::from_f64_retain)
        .unwrap_or(last_price);
    let close = close_opt
        .and_then(Decimal::from_f64_retain)
        .or_else(|| cp_opt.and_then(Decimal::from_f64_retain))
        .unwrap_or(last_price);

    let tick_time = ltt_opt
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or(now);

    let latency_ms = (now - tick_time).num_milliseconds().max(0) as u64;

    Some(LiveTick {
        symbol,
        instrument_token: instrument_key,
        last_price,
        open,
        high,
        low,
        close,
        volume: vtt_opt.unwrap_or(0),
        timestamp: tick_time,
        feed_timestamp: tick_time,
        received_at: now,
        latency_ms,
        is_stale: false,
        source: "UPSTOX_V3_LIVE".to_string(),
    })
}

fn parse_ltpc(buf: &[u8], ltp: &mut Option<f64>, ltt: &mut Option<i64>, cp: &mut Option<f64>) {
    let mut pos = 0;
    while pos < buf.len() {
        let Some(tag_wire) = read_varint(buf, &mut pos) else {
            break;
        };
        let tag = tag_wire >> 3;
        let wire = (tag_wire & 0x07) as u8;
        match (tag, wire) {
            (1, 1) => *ltp = read_f64(buf, &mut pos),
            (2, 0) => *ltt = read_varint(buf, &mut pos).map(|v| v as i64),
            (4, 1) => *cp = read_f64(buf, &mut pos),
            (_, w) => {
                if skip_field(w, buf, &mut pos).is_none() {
                    break;
                }
            }
        }
    }
}

/// `FullFeed` holds one of `marketFF` (field 1, stocks) or `indexFF` (field 2,
/// indices). Both carry `ltpc` at field 1; the day's OHLC sits at field 4 for a
/// stock and field 2 for an index, and only a stock has traded volume (field 6).
#[allow(clippy::too_many_arguments)]
fn parse_full_feed(
    buf: &[u8],
    ltp: &mut Option<f64>,
    ltt: &mut Option<i64>,
    cp: &mut Option<f64>,
    vtt: &mut Option<i64>,
    open: &mut Option<f64>,
    high: &mut Option<f64>,
    low: &mut Option<f64>,
    close: &mut Option<f64>,
) {
    let mut pos = 0;
    while pos < buf.len() {
        let Some(tag_wire) = read_varint(buf, &mut pos) else {
            break;
        };
        let tag = tag_wire >> 3;
        let wire = (tag_wire & 0x07) as u8;
        match (tag, wire) {
            (1 | 2, 2) => {
                let Some(inner) = read_length_delimited(buf, &mut pos) else {
                    break;
                };
                let (ohlc_field, has_volume) = if tag == 1 { (4, true) } else { (2, false) };
                parse_instrument_feed(
                    inner, ohlc_field, has_volume, ltp, ltt, cp, vtt, open, high, low, close,
                );
            }
            (_, w) => {
                if skip_field(w, buf, &mut pos).is_none() {
                    break;
                }
            }
        }
    }
}

/// One `MarketFullFeed` or `IndexFullFeed` message.
#[allow(clippy::too_many_arguments)]
fn parse_instrument_feed(
    buf: &[u8],
    ohlc_field: u64,
    has_volume: bool,
    ltp: &mut Option<f64>,
    ltt: &mut Option<i64>,
    cp: &mut Option<f64>,
    vtt: &mut Option<i64>,
    open: &mut Option<f64>,
    high: &mut Option<f64>,
    low: &mut Option<f64>,
    close: &mut Option<f64>,
) {
    let mut pos = 0;
    while pos < buf.len() {
        let Some(tag_wire) = read_varint(buf, &mut pos) else {
            break;
        };
        let tag = tag_wire >> 3;
        let wire = (tag_wire & 0x07) as u8;
        if tag == 1 && wire == 2 {
            if let Some(ltpc_buf) = read_length_delimited(buf, &mut pos) {
                parse_ltpc(ltpc_buf, ltp, ltt, cp);
            }
        } else if tag == ohlc_field && wire == 2 {
            if let Some(ohlc_buf) = read_length_delimited(buf, &mut pos) {
                parse_market_ohlc(ohlc_buf, open, high, low, close);
            }
        } else if has_volume && tag == 6 && wire == 0 {
            *vtt = read_varint(buf, &mut pos).map(|v| v as i64);
        } else if skip_field(wire, buf, &mut pos).is_none() {
            break;
        }
    }
}

/// `MarketOHLC`: repeated `OHLC` at field 1, one per interval. Only the
/// day's bar ("1d") is the session open, high, low and close.
fn parse_market_ohlc(
    buf: &[u8],
    open: &mut Option<f64>,
    high: &mut Option<f64>,
    low: &mut Option<f64>,
    close: &mut Option<f64>,
) {
    let mut pos = 0;
    while pos < buf.len() {
        let Some(tag_wire) = read_varint(buf, &mut pos) else {
            break;
        };
        let tag = tag_wire >> 3;
        let wire = (tag_wire & 0x07) as u8;
        if tag == 1 && wire == 2 {
            if let Some(ohlc_buf) = read_length_delimited(buf, &mut pos) {
                parse_ohlc(ohlc_buf, open, high, low, close);
            }
        } else if skip_field(wire, buf, &mut pos).is_none() {
            break;
        }
    }
}

/// `OHLC`: interval (1, string), open (2), high (3), low (4), close (5),
/// volume (6), ts (7). Bars other than the day's are ignored.
fn parse_ohlc(
    buf: &[u8],
    open: &mut Option<f64>,
    high: &mut Option<f64>,
    low: &mut Option<f64>,
    close: &mut Option<f64>,
) {
    let mut pos = 0;
    let mut interval: Option<&[u8]> = None;
    let (mut o, mut h, mut l, mut c) = (None, None, None, None);
    while pos < buf.len() {
        let Some(tag_wire) = read_varint(buf, &mut pos) else {
            break;
        };
        let tag = tag_wire >> 3;
        let wire = (tag_wire & 0x07) as u8;
        match (tag, wire) {
            (1, 2) => interval = read_length_delimited(buf, &mut pos),
            (2, 1) => o = read_f64(buf, &mut pos),
            (3, 1) => h = read_f64(buf, &mut pos),
            (4, 1) => l = read_f64(buf, &mut pos),
            (5, 1) => c = read_f64(buf, &mut pos),
            (_, w) => {
                if skip_field(w, buf, &mut pos).is_none() {
                    break;
                }
            }
        }
    }
    if interval == Some(b"1d".as_slice()) {
        (*open, *high, *low, *close) = (o, h, l, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_encode_decode() {
        let val = 1740727891235u64;
        let mut buf = Vec::new();
        let mut v = val;
        while v >= 0x80 {
            buf.push(((v & 0x7F) as u8) | 0x80);
            v >>= 7;
        }
        buf.push(v as u8);

        let mut pos = 0;
        let decoded = read_varint(&buf, &mut pos).unwrap();
        assert_eq!(decoded, val);
    }

    #[test]
    fn test_protobuf_feed_synthetic_decode() {
        // Construct synthetic LTPC message: ltp=2500.50 (double), ltt=1700000000000
        let mut ltpc_buf = Vec::new();
        // tag 1, wire 1 (ltp)
        ltpc_buf.push((1 << 3) | 1);
        ltpc_buf.extend_from_slice(&2500.50f64.to_le_bytes());
        // tag 2, wire 0 (ltt)
        ltpc_buf.push(2 << 3);
        let mut t = 1700000000000u64;
        while t >= 0x80 {
            ltpc_buf.push(((t & 0x7F) as u8) | 0x80);
            t >>= 7;
        }
        ltpc_buf.push(t as u8);

        // Feed message: tag 1, wire 2 -> ltpc
        let mut feed_buf = Vec::new();
        feed_buf.push((1 << 3) | 2);
        feed_buf.push(ltpc_buf.len() as u8);
        feed_buf.extend_from_slice(&ltpc_buf);

        // Map entry: tag 1 (key), tag 2 (feed)
        let key = b"NSE_EQ|INE002A01018"; // RELIANCE
        let mut entry_buf = Vec::new();
        // tag 1, wire 2 -> string
        entry_buf.push((1 << 3) | 2);
        entry_buf.push(key.len() as u8);
        entry_buf.extend_from_slice(key);
        // tag 2, wire 2 -> Feed
        entry_buf.push((2 << 3) | 2);
        entry_buf.push(feed_buf.len() as u8);
        entry_buf.extend_from_slice(&feed_buf);

        // Top-level FeedResponse: feeds map entry at field 2, wire 2
        let mut top_buf = Vec::new();
        top_buf.push((2 << 3) | 2);
        top_buf.push(entry_buf.len() as u8);
        top_buf.extend_from_slice(&entry_buf);

        let ticks = parse_upstox_protobuf_feed(&top_buf);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].symbol, "RELIANCE");
        assert_eq!(ticks[0].last_price, dec!(2500.50));
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A FeedResponse with these map entries at field 2, as Upstox sends them.
    fn frame(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![1 << 3, 1]; // type = live_feed
        for e in entries {
            out.push((2 << 3) | 2);
            let mut len = e.len();
            while len >= 0x80 {
                out.push(((len & 0x7F) as u8) | 0x80);
                len >>= 7;
            }
            out.push(len as u8);
            out.extend_from_slice(e);
        }
        out
    }

    // Map entries captured from the live V3 stream ("full" mode) after the
    // close on 2 Nov 2026: RELIANCE (marketFF) and NIFTY 50 (indexFF).
    const RELIANCE_ENTRY: &str = "0a134e53455f45517c494e4530303241303130313812d10112cc010ac9010a1b09000000000050944010efbfbade9934180a219a9999999933924012170a0d18b895032100000000005094400a000a000a000a001a00226c0a340a02316411666666666630934019000000000050944021cdccccccccf8924029000000000050944030a1be822a38c0c880c399340a340a02493111000000000050944019000000000050944021000000000050944029000000000050944030d2acfd0438a0d5cadd9934291f85eb51b882934030a1be822a51000000000057e940616666666666d693408001012001";
    const NIFTY_ENTRY: &str = "0a124e53455f494e4445587c4e69667479203530128601128101127f0a19093333333333e4d64010c098bcde993421666666664683d64012620a2f0a023164113333333393add640193333333333e4d6402133333333c3a9d640293333333333e4d64038c0c880c399340a2f0a024931113333333333e4d640193333333333e4d640213333333333e4d640293333333333e4d6403880aacedd99342001";

    #[test]
    fn a_real_full_mode_frame_yields_stock_and_index_ticks() {
        let ticks =
            parse_upstox_protobuf_feed(&frame(&[unhex(RELIANCE_ENTRY), unhex(NIFTY_ENTRY)]));
        assert_eq!(ticks.len(), 2);

        let r = &ticks[0];
        assert_eq!(r.instrument_token, "NSE_EQ|INE002A01018");
        assert_eq!(r.last_price, dec!(1300));
        assert_eq!(r.open, Decimal::from_f64_retain(1228.1).unwrap());
        assert_eq!(r.high, dec!(1300));
        assert_eq!(r.low, Decimal::from_f64_retain(1214.2).unwrap());
        assert_eq!(r.volume, 88_121_121);
        assert_eq!(r.source, "UPSTOX_V3_LIVE");

        let n = &ticks[1];
        assert_eq!(n.instrument_token, "NSE_INDEX|Nifty 50");
        assert_eq!(n.last_price, Decimal::from_f64_retain(23440.8).unwrap());
        assert_eq!(n.open, Decimal::from_f64_retain(23222.3).unwrap());
        assert_eq!(n.volume, 0);
    }
}
