//! Crypto market data source.
//!
//! Fetches price data from CoinGecko's free API to inform
//! crypto-related prediction markets (e.g. "Will BTC exceed $X by date Y?").

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;

use crate::data::{DataPoint, DataSource, MarketQuery};
use crate::market::models::MarketCategory;

/// Top cryptocurrencies to track for prediction markets.
const TRACKED_COINS: &[&str] = &["bitcoin", "ethereum", "solana", "dogecoin", "ripple"];

pub struct CryptoSource {
    client: reqwest::Client,
}

impl CryptoSource {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        Self { client }
    }

    async fn fetch_prices(&self) -> Result<Vec<CoinGeckoPrice>> {
        let ids = TRACKED_COINS.join(",");
        let url = format!(
            "https://api.coingecko.com/api/v3/coins/markets?vs_currency=usd&ids={ids}&order=market_cap_desc&sparkline=false&price_change_percentage=24h,7d"
        );

        let prices: Vec<CoinGeckoPrice> = self
            .client
            .get(&url)
            .send()
            .await
            .context("CoinGecko request failed")?
            .json()
            .await
            .context("Failed to parse CoinGecko response")?;

        Ok(prices)
    }
}

#[async_trait]
impl DataSource for CryptoSource {
    async fn fetch(&self, queries: &[MarketQuery]) -> Result<Vec<DataPoint>> {
        let mut points = Vec::new();

        let prices = self.fetch_prices().await?;

        for coin in &prices {
            let payload = serde_json::json!({
                "coin_id": coin.id,
                "symbol": coin.symbol,
                "name": coin.name,
                "current_price": coin.current_price,
                "market_cap": coin.market_cap,
                "total_volume_24h": coin.total_volume,
                "price_change_24h": coin.price_change_percentage_24h,
                "price_change_7d": coin.price_change_percentage_7d_in_currency,
                "high_24h": coin.high_24h,
                "low_24h": coin.low_24h,
                "ath": coin.ath,
                "ath_change_pct": coin.ath_change_percentage,
            });

            // Match to relevant market queries by coin name/symbol
            let coin_lower = coin.name.to_lowercase();
            let symbol_lower = coin.symbol.to_lowercase();
            let relevance: Vec<String> = queries
                .iter()
                .filter(|q| {
                    let ql = q.question.to_lowercase();
                    ql.contains(&coin_lower)
                        || ql.contains(&symbol_lower)
                        || (symbol_lower == "bitcoin" && ql.contains("btc"))
                        || (symbol_lower == "ethereum" && ql.contains("eth"))
                })
                .map(|q| q.condition_id.clone())
                .collect();

            // Data quality depends on market cap rank (higher cap = more reliable price)
            let confidence = if coin.market_cap.unwrap_or(Decimal::ZERO) > dec!(10_000_000_000) {
                dec!(0.95)
            } else {
                dec!(0.80)
            };

            points.push(DataPoint {
                source: "coingecko".to_string(),
                category: MarketCategory::Crypto,
                timestamp: Utc::now(),
                payload,
                confidence,
                relevance_to: relevance,
            });
        }

        Ok(points)
    }

    fn category(&self) -> MarketCategory {
        MarketCategory::Crypto
    }

    fn freshness_window(&self) -> Duration {
        Duration::from_secs(120) // 2 minutes — crypto prices move fast
    }

    fn name(&self) -> &str {
        "coingecko_crypto"
    }
}

// --- CoinGecko API Response Types ---

#[derive(Debug, Deserialize)]
struct CoinGeckoPrice {
    id: String,
    symbol: String,
    name: String,
    current_price: Option<Decimal>,
    market_cap: Option<Decimal>,
    total_volume: Option<Decimal>,
    high_24h: Option<Decimal>,
    low_24h: Option<Decimal>,
    price_change_percentage_24h: Option<f64>,
    #[serde(rename = "price_change_percentage_7d_in_currency")]
    price_change_percentage_7d_in_currency: Option<f64>,
    ath: Option<Decimal>,
    ath_change_percentage: Option<f64>,
}
