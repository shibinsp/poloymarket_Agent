//! Confidence calibration system.
//!
//! Tracks Claude's self-reported confidence against actual trade outcomes
//! to compute a calibration discount. If Claude is systematically overconfident,
//! the discount reduces future confidence values used in Kelly sizing.

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;
use std::str::FromStr;
use tracing::{info, warn};

/// Default confidence discount applied before enough calibration data is collected.
const DEFAULT_DISCOUNT: Decimal = dec!(0.85);

/// Minimum number of resolved trades before using empirical calibration.
const MIN_CALIBRATION_SAMPLES: usize = 50;

/// Floor for the calibration discount (never reduce confidence by more than 70%).
const MIN_DISCOUNT: Decimal = dec!(0.30);

/// Record a prediction for calibration tracking.
pub async fn record_prediction(
    pool: &SqlitePool,
    market_id: &str,
    claude_confidence: Decimal,
    fair_value: Decimal,
    market_price_at_entry: Decimal,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO confidence_calibration (market_id, claude_confidence, fair_value, market_price_at_entry, resolved)
         VALUES (?, ?, ?, ?, 0)",
    )
    .bind(market_id)
    .bind(claude_confidence.to_string())
    .bind(fair_value.to_string())
    .bind(market_price_at_entry.to_string())
    .execute(pool)
    .await
    .context("Failed to record calibration prediction")?;

    Ok(())
}

/// Record the resolution of a prediction for calibration.
pub async fn record_resolution(
    pool: &SqlitePool,
    market_id: &str,
    actual_outcome: Decimal, // 1.0 for YES, 0.0 for NO
) -> Result<()> {
    // Find unresolved prediction for this market
    let row: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT id, fair_value, market_price_at_entry FROM confidence_calibration
         WHERE market_id = ? AND resolved = 0
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(market_id)
    .fetch_optional(pool)
    .await
    .context("Failed to look up calibration record")?;

    if let Some((id, fair_value_str, _entry_price_str)) = row {
        let fair_value = match Decimal::from_str(&fair_value_str) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    market_id = %market_id,
                    fair_value_str = %fair_value_str,
                    error = %e,
                    "Corrupted fair_value in calibration record — skipping resolution"
                );
                return Ok(());
            }
        };

        // Did Claude's directional call match the outcome?
        // If fair_value > 0.5 and outcome = 1.0 → correct
        // If fair_value < 0.5 and outcome = 0.0 → correct
        // If fair_value == 0.5, there was no directional prediction → mark as incorrect
        let forecast_correct = (fair_value > dec!(0.5) && actual_outcome == Decimal::ONE)
            || (fair_value < dec!(0.5) && actual_outcome == Decimal::ZERO);

        sqlx::query(
            "UPDATE confidence_calibration
             SET actual_outcome = ?, forecast_correct = ?, resolved = 1, resolved_at = datetime('now')
             WHERE id = ?",
        )
        .bind(actual_outcome.to_string())
        .bind(forecast_correct)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to update calibration resolution")?;
    }

    Ok(())
}

/// Compute the confidence discount factor based on historical calibration data.
///
/// Returns a value between `MIN_DISCOUNT` and `1.0` that should multiply
/// Claude's self-reported confidence before it's used in Kelly sizing.
///
/// If fewer than `MIN_CALIBRATION_SAMPLES` resolved trades exist,
/// returns `DEFAULT_DISCOUNT` (0.85).
pub async fn compute_discount(pool: &SqlitePool, lookback: usize) -> Result<Decimal> {
    let rows: Vec<(String, bool)> = sqlx::query_as(
        "SELECT claude_confidence, forecast_correct FROM confidence_calibration
         WHERE resolved = 1
         ORDER BY resolved_at DESC
         LIMIT ?",
    )
    .bind(lookback as i64)
    .fetch_all(pool)
    .await
    .context("Failed to fetch calibration data")?;

    if rows.len() < MIN_CALIBRATION_SAMPLES {
        info!(
            samples = rows.len(),
            required = MIN_CALIBRATION_SAMPLES,
            discount = %DEFAULT_DISCOUNT,
            "Insufficient calibration data — using default discount"
        );
        return Ok(DEFAULT_DISCOUNT);
    }

    // Empirical accuracy: fraction of correct directional calls
    let correct_count = rows.iter().filter(|(_, correct)| *correct).count();
    let empirical_accuracy = Decimal::from(correct_count as u64) / Decimal::from(rows.len() as u64);

    // Average reported confidence
    let total_confidence: Decimal = rows
        .iter()
        .filter_map(|(c, _)| Decimal::from_str(c).ok())
        .sum();
    let avg_confidence = total_confidence / Decimal::from(rows.len() as u64);

    // Discount = empirical_accuracy / avg_confidence (capped at 1.0, floored at MIN_DISCOUNT)
    let discount = if avg_confidence > Decimal::ZERO {
        (empirical_accuracy / avg_confidence).min(Decimal::ONE).max(MIN_DISCOUNT)
    } else {
        DEFAULT_DISCOUNT
    };

    info!(
        samples = rows.len(),
        empirical_accuracy = %empirical_accuracy,
        avg_confidence = %avg_confidence,
        discount = %discount,
        "Calibration discount computed"
    );

    Ok(discount)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::store::Store;

    #[tokio::test]
    async fn test_default_discount_with_no_data() {
        let store = Store::new(":memory:").await.unwrap();
        let discount = compute_discount(store.pool(), 100).await.unwrap();
        assert_eq!(discount, DEFAULT_DISCOUNT);
    }

    #[tokio::test]
    async fn test_record_and_resolve_prediction() {
        let store = Store::new(":memory:").await.unwrap();

        record_prediction(
            store.pool(),
            "market_1",
            dec!(0.85),
            dec!(0.70),
            dec!(0.50),
        )
        .await
        .unwrap();

        record_resolution(store.pool(), "market_1", Decimal::ONE)
            .await
            .unwrap();

        // Still below MIN_CALIBRATION_SAMPLES
        let discount = compute_discount(store.pool(), 100).await.unwrap();
        assert_eq!(discount, DEFAULT_DISCOUNT);
    }
}
