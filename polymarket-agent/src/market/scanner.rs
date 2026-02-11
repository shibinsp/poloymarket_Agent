//! Market discovery and filtering.
//!
//! Scans Polymarket for trading candidates that pass liquidity,
//! spread, and resolution-date filters.

use anyhow::Result;
use std::sync::Arc;
use tracing::{info, instrument, warn};

use crate::config::ScanningConfig;
use crate::market::models::MarketCandidate;
use crate::market::polymarket::{MarketFilters, PolymarketClient};

pub struct MarketScanner {
    client: Arc<PolymarketClient>,
    config: ScanningConfig,
}

impl MarketScanner {
    pub fn new(client: Arc<PolymarketClient>, config: ScanningConfig) -> Self {
        Self { client, config }
    }

    /// Scan markets and return candidates worth evaluating.
    #[instrument(skip(self))]
    pub async fn scan(&self) -> Result<Vec<MarketCandidate>> {
        let filters = MarketFilters {
            min_volume_24h: self.config.min_volume_24h,
            max_resolution_days: self.config.max_resolution_days,
            max_markets: self.config.max_markets,
            max_spread_pct: self.config.max_spread_pct,
        };

        let markets = self.client.get_markets(&filters).await?;
        info!(count = markets.len(), "Markets discovered");

        let mut candidates = Vec::new();

        for market in markets {
            for token in &market.tokens {
                match self.client.get_order_book(&token.token_id).await {
                    Ok(book) => {
                        // Filter by spread
                        if book.spread <= self.config.max_spread_pct {
                            candidates.push(MarketCandidate {
                                market: market.clone(),
                                order_book: book,
                            });
                            // Only take one token per market for now (YES side)
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(
                            token_id = %token.token_id,
                            error = %e,
                            "Failed to get order book, skipping"
                        );
                    }
                }
            }
        }

        info!(candidates = candidates.len(), "Market candidates after filtering");
        Ok(candidates)
    }
}
