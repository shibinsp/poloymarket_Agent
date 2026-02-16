//! Programmatic data quality assessment.
//!
//! Computes data quality from actual data characteristics (source count,
//! freshness, confidence) instead of relying on Claude's self-report.

use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::data::DataPoint;
use crate::valuation::fair_value::DataQuality;

/// Compute data quality programmatically from data point characteristics.
///
/// Scoring factors:
/// - Coverage: number of distinct sources (capped at 5)
/// - Freshness: fraction of data points less than 24 hours old
/// - Confidence: average self-assessed confidence from data sources
///
/// Weights: coverage 40%, freshness 30%, confidence 30%.
pub fn compute_data_quality(data_points: &[DataPoint]) -> DataQuality {
    if data_points.is_empty() {
        return DataQuality::Low;
    }

    let now = Utc::now();

    // Coverage: how many distinct sources contributed
    let mut sources: Vec<&str> = data_points.iter().map(|dp| dp.source.as_str()).collect();
    sources.sort();
    sources.dedup();
    let source_count = sources.len();
    let coverage_score = (source_count as f64).min(5.0) / 5.0;

    // Freshness: fraction of data points less than 24 hours old
    let recent_count = data_points
        .iter()
        .filter(|dp| (now - dp.timestamp).num_hours() < 24)
        .count();
    let freshness_score = recent_count as f64 / data_points.len() as f64;

    // Confidence: average of source-level confidence scores
    let total_confidence: Decimal = data_points.iter().map(|dp| dp.confidence).sum();
    let avg_confidence = total_confidence / Decimal::from(data_points.len() as u64);
    let confidence_f64 = avg_confidence
        .to_string()
        .parse::<f64>()
        .unwrap_or(0.5);

    // Weighted composite score
    let quality_score = (coverage_score * 0.4) + (freshness_score * 0.3) + (confidence_f64 * 0.3);

    if quality_score >= 0.7 {
        DataQuality::High
    } else if quality_score >= 0.4 {
        DataQuality::Medium
    } else {
        DataQuality::Low
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::models::MarketCategory;
    use chrono::Duration;

    fn make_data_point(source: &str, hours_ago: i64, confidence: Decimal) -> DataPoint {
        DataPoint {
            source: source.to_string(),
            category: MarketCategory::Crypto,
            timestamp: Utc::now() - Duration::hours(hours_ago),
            payload: serde_json::json!({"test": true}),
            confidence,
            relevance_to: vec!["test".to_string()],
        }
    }

    #[test]
    fn test_empty_data_is_low() {
        assert_eq!(compute_data_quality(&[]), DataQuality::Low);
    }

    #[test]
    fn test_high_quality_multi_source_fresh() {
        let points = vec![
            make_data_point("noaa", 1, dec!(0.9)),
            make_data_point("espn", 2, dec!(0.85)),
            make_data_point("coingecko", 1, dec!(0.95)),
            make_data_point("google_news", 3, dec!(0.5)),
            make_data_point("extra", 1, dec!(0.8)),
        ];
        assert_eq!(compute_data_quality(&points), DataQuality::High);
    }

    #[test]
    fn test_single_stale_source_is_low() {
        let points = vec![make_data_point("noaa", 48, dec!(0.3))];
        assert_eq!(compute_data_quality(&points), DataQuality::Low);
    }

    #[test]
    fn test_medium_quality() {
        let points = vec![
            make_data_point("noaa", 6, dec!(0.7)),
            make_data_point("espn", 30, dec!(0.5)),
        ];
        assert_eq!(compute_data_quality(&points), DataQuality::Medium);
    }
}
