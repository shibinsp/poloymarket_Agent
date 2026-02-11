//! NOAA weather data source.
//!
//! Fetches forecasts from api.weather.gov and detects forecast changes
//! that could create edge in weather-related prediction markets.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use rust_decimal_macros::dec;
use serde::Deserialize;

use crate::data::{DataPoint, DataSource, MarketQuery};
use crate::market::models::MarketCategory;

/// Major US cities for weather market scanning.
const DEFAULT_STATIONS: &[(&str, f64, f64)] = &[
    ("New York", 40.7128, -74.0060),
    ("Los Angeles", 33.9425, -118.2551),
    ("Chicago", 41.8781, -87.6298),
    ("Miami", 25.7617, -80.1918),
    ("Houston", 29.7604, -95.3698),
];

pub struct WeatherSource {
    client: reqwest::Client,
}

impl WeatherSource {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("polymarket-agent/0.1 (contact@example.com)")
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        Self { client }
    }

    async fn fetch_forecast(&self, lat: f64, lon: f64) -> Result<NoaaForecast> {
        // Step 1: Get the forecast URL for this point
        let points_url = format!("https://api.weather.gov/points/{lat:.4},{lon:.4}");
        let points: PointsResponse = self
            .client
            .get(&points_url)
            .send()
            .await
            .context("NOAA points request failed")?
            .json()
            .await
            .context("Failed to parse NOAA points response")?;

        // Step 2: Fetch the actual forecast
        let forecast: NoaaForecast = self
            .client
            .get(&points.properties.forecast)
            .send()
            .await
            .context("NOAA forecast request failed")?
            .json()
            .await
            .context("Failed to parse NOAA forecast")?;

        Ok(forecast)
    }
}

#[async_trait]
impl DataSource for WeatherSource {
    async fn fetch(&self, queries: &[MarketQuery]) -> Result<Vec<DataPoint>> {
        let mut points = Vec::new();

        for (city, lat, lon) in DEFAULT_STATIONS {
            // Check if any queries mention this city (case-insensitive)
            let relevant_ids: Vec<String> = queries
                .iter()
                .filter(|q| q.question.to_lowercase().contains(&city.to_lowercase()))
                .map(|q| q.condition_id.clone())
                .collect();

            // Also fetch for general weather markets even without city match
            match self.fetch_forecast(*lat, *lon).await {
                Ok(forecast) => {
                    for period in &forecast.properties.periods {
                        let payload = serde_json::json!({
                            "city": city,
                            "period_name": period.name,
                            "temperature": period.temperature,
                            "temperature_unit": period.temperature_unit,
                            "wind_speed": period.wind_speed,
                            "short_forecast": period.short_forecast,
                            "detailed_forecast": period.detailed_forecast,
                            "precipitation_probability": period.probability_of_precipitation.as_ref().map(|p| p.value),
                            "is_daytime": period.is_daytime,
                        });

                        let mut relevance = relevant_ids.clone();
                        // Also match any query mentioning temperature/weather keywords
                        for q in queries {
                            let ql = q.question.to_lowercase();
                            if (ql.contains("temperature") || ql.contains("weather") || ql.contains("hurricane") || ql.contains("rain"))
                                && !relevance.contains(&q.condition_id)
                            {
                                relevance.push(q.condition_id.clone());
                            }
                        }

                        points.push(DataPoint {
                            source: "noaa".to_string(),
                            category: MarketCategory::Weather,
                            timestamp: Utc::now(),
                            payload,
                            confidence: dec!(0.9), // NOAA is authoritative
                            relevance_to: relevance,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(city, error = %e, "Failed to fetch weather for city");
                }
            }
        }

        Ok(points)
    }

    fn category(&self) -> MarketCategory {
        MarketCategory::Weather
    }

    fn freshness_window(&self) -> Duration {
        Duration::from_secs(3600) // 1 hour — NOAA updates hourly
    }

    fn name(&self) -> &str {
        "noaa_weather"
    }
}

// --- NOAA API Response Types ---

#[derive(Debug, Deserialize)]
struct PointsResponse {
    properties: PointsProperties,
}

#[derive(Debug, Deserialize)]
struct PointsProperties {
    forecast: String,
}

#[derive(Debug, Deserialize)]
struct NoaaForecast {
    properties: ForecastProperties,
}

#[derive(Debug, Deserialize)]
struct ForecastProperties {
    periods: Vec<ForecastPeriod>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForecastPeriod {
    name: String,
    temperature: i32,
    temperature_unit: String,
    wind_speed: String,
    short_forecast: String,
    detailed_forecast: String,
    probability_of_precipitation: Option<PrecipitationProbability>,
    is_daytime: bool,
}

#[derive(Debug, Deserialize)]
struct PrecipitationProbability {
    value: Option<i32>,
}
