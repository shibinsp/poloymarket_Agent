//! Historical market data loading for backtesting.
//!
//! Loads market snapshots from CSV files or generates synthetic data
//! for testing. Each snapshot represents one "tick" that the backtester
//! replays through the agent pipeline.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

use crate::market::models::{
    Market, MarketCandidate, MarketCategory, OrderBookSnapshot, PriceLevel, TokenInfo,
};

/// A historical market snapshot representing one point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoricalSnapshot {
    pub timestamp: DateTime<Utc>,
    pub market_id: String,
    pub question: String,
    pub category: String,
    pub yes_price: Decimal,
    pub no_price: Decimal,
    pub volume_24h: Decimal,
    pub spread: Decimal,
    pub end_date: DateTime<Utc>,
    /// The actual resolved outcome: 1.0 = YES, 0.0 = NO.
    pub resolved_outcome: Option<Decimal>,
}

/// Load historical snapshots from a CSV file.
///
/// Expected CSV columns: timestamp, market_id, question, category,
/// yes_price, no_price, volume_24h, spread, end_date, resolved_outcome
pub fn load_from_csv(path: &Path) -> Result<Vec<HistoricalSnapshot>> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;

    let mut snapshots = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if i == 0 {
            continue; // Skip header
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        match parse_csv_line(line) {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(e) => {
                tracing::warn!(line = i + 1, error = %e, "Skipping malformed CSV line");
            }
        }
    }

    Ok(snapshots)
}

fn parse_csv_line(line: &str) -> Result<HistoricalSnapshot> {
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() < 9 {
        anyhow::bail!("Expected at least 9 CSV columns, got {}", fields.len());
    }

    let timestamp = DateTime::parse_from_rfc3339(fields[0].trim())
        .with_context(|| format!("Invalid timestamp: {}", fields[0]))?
        .with_timezone(&Utc);

    let end_date = DateTime::parse_from_rfc3339(fields[8].trim())
        .with_context(|| format!("Invalid end_date: {}", fields[8]))?
        .with_timezone(&Utc);

    let resolved_outcome = if fields.len() > 9 && !fields[9].trim().is_empty() {
        Some(
            Decimal::from_str(fields[9].trim())
                .with_context(|| format!("Invalid resolved_outcome: {}", fields[9]))?,
        )
    } else {
        None
    };

    Ok(HistoricalSnapshot {
        timestamp,
        market_id: fields[1].trim().to_string(),
        question: fields[2].trim().to_string(),
        category: fields[3].trim().to_string(),
        yes_price: Decimal::from_str(fields[4].trim())?,
        no_price: Decimal::from_str(fields[5].trim())?,
        volume_24h: Decimal::from_str(fields[6].trim())?,
        spread: Decimal::from_str(fields[7].trim())?,
        end_date,
        resolved_outcome,
    })
}

/// Generate synthetic historical data for testing the backtester.
///
/// Creates `count` market snapshots with randomized-but-plausible prices.
/// Uses a simple deterministic pattern (not truly random) for reproducibility.
pub fn generate_synthetic(count: usize) -> Vec<HistoricalSnapshot> {
    let mut snapshots = Vec::with_capacity(count);
    let base_time = Utc::now() - chrono::Duration::days(30);

    for i in 0..count {
        // Deterministic "random" prices using index-based pattern
        let cycle = (i % 100) as u32;
        let yes_price = dec!(0.30) + Decimal::from(cycle) * dec!(0.004);
        let yes_price = yes_price.min(dec!(0.90)); // Cap at 0.90
        let no_price = Decimal::ONE - yes_price;
        let spread = dec!(0.02) + Decimal::from(i % 5) * dec!(0.005);

        // Outcome: markets resolve YES if final yes_price > 0.50
        let resolved = if i % 3 == 0 {
            None // Some markets haven't resolved yet
        } else if yes_price > dec!(0.55) {
            Some(Decimal::ONE) // YES wins
        } else {
            Some(Decimal::ZERO) // NO wins
        };

        let timestamp = base_time + chrono::Duration::minutes(i as i64 * 10);
        let end_date = timestamp + chrono::Duration::days(7);

        let categories = ["crypto", "politics", "sports", "weather"];
        let category = categories[i % categories.len()].to_string();

        snapshots.push(HistoricalSnapshot {
            timestamp,
            market_id: format!("market_{i:04}"),
            question: format!("Will event {i} happen?"),
            category,
            yes_price,
            no_price,
            volume_24h: dec!(10000) + Decimal::from((i * 500) as u64),
            spread,
            end_date,
            resolved_outcome: resolved,
        });
    }

    snapshots
}

/// Convert a historical snapshot into a MarketCandidate for pipeline processing.
pub fn snapshot_to_candidate(snapshot: &HistoricalSnapshot) -> MarketCandidate {
    let category = match snapshot.category.to_lowercase().as_str() {
        "crypto" => MarketCategory::Crypto,
        "sports" => MarketCategory::Sports,
        "weather" => MarketCategory::Weather,
        "politics" => MarketCategory::Politics,
        other => MarketCategory::Other(other.to_string()),
    };

    let market = Market {
        condition_id: snapshot.market_id.clone(),
        question: snapshot.question.clone(),
        outcomes: vec!["Yes".to_string(), "No".to_string()],
        tokens: vec![
            TokenInfo {
                token_id: format!("{}_yes", snapshot.market_id),
                outcome: "Yes".to_string(),
                price: snapshot.yes_price,
            },
            TokenInfo {
                token_id: format!("{}_no", snapshot.market_id),
                outcome: "No".to_string(),
                price: snapshot.no_price,
            },
        ],
        end_date: snapshot.end_date,
        category,
        volume_24h: snapshot.volume_24h,
        active: true,
    };

    let midpoint = (snapshot.yes_price + (Decimal::ONE - snapshot.no_price)) / dec!(2);
    let ask_price = midpoint + snapshot.spread / dec!(2);
    let bid_price = midpoint - snapshot.spread / dec!(2);

    let order_book = OrderBookSnapshot {
        token_id: format!("{}_yes", snapshot.market_id),
        bids: vec![PriceLevel {
            price: bid_price.max(dec!(0.01)),
            size: dec!(500),
        }],
        asks: vec![PriceLevel {
            price: ask_price.min(dec!(0.99)),
            size: dec!(500),
        }],
        spread: snapshot.spread,
        midpoint,
        implied_probability: snapshot.yes_price,
        timestamp: snapshot.timestamp,
    };

    MarketCandidate {
        market,
        order_book,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_synthetic() {
        let snapshots = generate_synthetic(100);
        assert_eq!(snapshots.len(), 100);

        // Check basic properties
        for snap in &snapshots {
            assert!(snap.yes_price > Decimal::ZERO);
            assert!(snap.yes_price < Decimal::ONE);
            assert!(snap.no_price > Decimal::ZERO);
            assert!(snap.spread > Decimal::ZERO);
            assert!(snap.volume_24h > Decimal::ZERO);
        }

        // Some should be resolved, some not
        let resolved_count = snapshots.iter().filter(|s| s.resolved_outcome.is_some()).count();
        assert!(resolved_count > 0);
        assert!(resolved_count < 100);
    }

    #[test]
    fn test_snapshot_to_candidate() {
        let snap = HistoricalSnapshot {
            timestamp: Utc::now(),
            market_id: "test_market".to_string(),
            question: "Will it rain?".to_string(),
            category: "weather".to_string(),
            yes_price: dec!(0.65),
            no_price: dec!(0.35),
            volume_24h: dec!(25000),
            spread: dec!(0.03),
            end_date: Utc::now() + chrono::Duration::days(3),
            resolved_outcome: Some(Decimal::ONE),
        };

        let candidate = snapshot_to_candidate(&snap);
        assert_eq!(candidate.market.condition_id, "test_market");
        assert_eq!(candidate.market.question, "Will it rain?");
        assert_eq!(candidate.market.tokens.len(), 2);
        assert_eq!(candidate.order_book.spread, dec!(0.03));
    }

    #[test]
    fn test_parse_csv_line() {
        let line = "2025-01-01T00:00:00Z,m1,Will BTC hit 100k?,crypto,0.65,0.35,50000,0.03,2025-01-08T00:00:00Z,1.0";
        let snap = parse_csv_line(line).unwrap();
        assert_eq!(snap.market_id, "m1");
        assert_eq!(snap.yes_price, dec!(0.65));
        assert_eq!(snap.resolved_outcome, Some(Decimal::ONE));
    }

    #[test]
    fn test_parse_csv_line_no_outcome() {
        let line = "2025-01-01T00:00:00Z,m2,Test?,politics,0.50,0.50,10000,0.02,2025-01-08T00:00:00Z,";
        let snap = parse_csv_line(line).unwrap();
        assert_eq!(snap.market_id, "m2");
        assert!(snap.resolved_outcome.is_none());
    }
}
