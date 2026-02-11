pub mod crypto;
pub mod news;
pub mod sports;
pub mod weather;

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::market::models::MarketCategory;

/// Standardized data point output from any data source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPoint {
    /// Which source produced this data (e.g. "noaa", "espn", "coingecko").
    pub source: String,
    /// Market category this data relates to.
    pub category: MarketCategory,
    /// When the data was fetched/observed.
    pub timestamp: DateTime<Utc>,
    /// Flexible payload — schema varies by source.
    pub payload: serde_json::Value,
    /// Self-assessed data quality (0.0 = garbage, 1.0 = authoritative).
    pub confidence: Decimal,
    /// Which market condition_ids this data may inform.
    pub relevance_to: Vec<String>,
}

/// Trait for all external data sources.
/// Each source fetches data relevant to a market category.
#[async_trait]
pub trait DataSource: Send + Sync {
    /// Fetch the latest data points from this source.
    async fn fetch(&self, market_questions: &[MarketQuery]) -> Result<Vec<DataPoint>>;

    /// Which market category this source covers.
    fn category(&self) -> MarketCategory;

    /// How long fetched data remains fresh before re-fetching.
    fn freshness_window(&self) -> Duration;

    /// Human-readable name of this data source.
    fn name(&self) -> &str;
}

/// A market query that data sources can use to find relevant data.
#[derive(Debug, Clone)]
pub struct MarketQuery {
    pub condition_id: String,
    pub question: String,
    pub category: MarketCategory,
}

/// Aggregates data from multiple sources.
pub struct DataAggregator {
    sources: Vec<Box<dyn DataSource>>,
}

impl DataAggregator {
    pub fn new(sources: Vec<Box<dyn DataSource>>) -> Self {
        Self { sources }
    }

    /// Fetch data from all sources relevant to the given markets.
    pub async fn fetch_all(&self, queries: &[MarketQuery]) -> Vec<DataPoint> {
        let mut all_data = Vec::new();

        for source in &self.sources {
            // Only pass queries matching this source's category
            let relevant: Vec<MarketQuery> = queries
                .iter()
                .filter(|q| q.category == source.category())
                .cloned()
                .collect();

            if relevant.is_empty() {
                continue;
            }

            match source.fetch(&relevant).await {
                Ok(points) => {
                    tracing::info!(
                        source = source.name(),
                        points = points.len(),
                        "Data fetched"
                    );
                    all_data.extend(points);
                }
                Err(e) => {
                    tracing::warn!(
                        source = source.name(),
                        error = %e,
                        "Data source fetch failed"
                    );
                }
            }
        }

        all_data
    }
}
