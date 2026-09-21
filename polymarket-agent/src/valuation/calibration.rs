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
/// Calibration key for a continuous-asset position.
///
/// The table was built for prediction markets and keys on `market_id`; a
/// venue symbol is the same thing for this purpose, namespaced so two venues
/// quoting "BTC/USD" cannot be confused for one forecast.
pub fn directional_key(venue_id: &str, symbol: &str) -> String {
    format!("{venue_id}:{symbol}")
}

/// Record a directional forecast so it can be scored when the position closes.
///
/// `p_up` is the forecast probability the price is higher at the horizon —
/// the continuous-asset analogue of a prediction market's fair value, and the
/// number a Brier score is computed over.
///
/// Recording only. Feeding calibration back into sizing — the return
/// shrinkage and the Brier gate — is Phase 5; without this, though, the
/// paper window's "Brier ≤0.24 on ≥30 closed positions" criterion cannot be
/// evaluated at all, because nothing on the venue path wrote a forecast down.
pub async fn record_directional_prediction(
    pool: &SqlitePool,
    venue_id: &str,
    symbol: &str,
    confidence: Decimal,
    p_up: Decimal,
    entry_price: Decimal,
) -> Result<()> {
    record_prediction(
        pool,
        &directional_key(venue_id, symbol),
        confidence,
        p_up,
        entry_price,
    )
    .await
}

/// Score a directional forecast against what the price actually did.
///
/// Long-only, so the forecast was "higher at the horizon" and the outcome is
/// whether the exit beat the entry. A flat close counts as *not* higher: the
/// forecast said up, and it did not go up.
pub async fn resolve_directional_prediction(
    pool: &SqlitePool,
    venue_id: &str,
    symbol: &str,
    entry_price: Decimal,
    exit_price: Decimal,
) -> Result<()> {
    let outcome = if exit_price > entry_price {
        Decimal::ONE
    } else {
        Decimal::ZERO
    };
    record_resolution(pool, &directional_key(venue_id, symbol), outcome).await
}

/// Mean squared error of the forecast probabilities against their outcomes.
///
/// Zero is perfect; 0.25 is what always guessing 50% scores, which is the
/// number to beat before believing any of this has edge. Returns `None` when
/// too few forecasts have resolved to say anything — the promotion criterion
/// asks for ≥30, and a Brier over four closes is noise wearing a decimal
/// point.
pub async fn brier_score(pool: &SqlitePool, minimum: usize) -> Result<Option<(Decimal, usize)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT fair_value, actual_outcome FROM confidence_calibration
         WHERE resolved = 1 AND actual_outcome IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .context("Failed to read resolved forecasts")?;

    let mut total = Decimal::ZERO;
    let mut n = 0usize;
    for (forecast, outcome) in &rows {
        let (Ok(f), Ok(o)) = (Decimal::from_str(forecast), Decimal::from_str(outcome)) else {
            warn!(forecast, outcome, "Unparseable calibration row — skipped");
            continue;
        };
        let err = f - o;
        total += err * err;
        n += 1;
    }

    if n < minimum {
        return Ok(None);
    }
    Ok(Some((total / Decimal::from(n), n)))
}

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
        (empirical_accuracy / avg_confidence)
            .min(Decimal::ONE)
            .max(MIN_DISCOUNT)
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

    /// A throwaway database. The pool is leaked with the `Store` so the
    /// in-memory database outlives the borrow — dropping the store closes it.
    async fn pool() -> SqlitePool {
        let store = Box::leak(Box::new(Store::new(":memory:").await.unwrap()));
        store.pool().clone()
    }

    /// Always guessing 50% scores 0.25. That is the number every other
    /// result has to be read against — a Brier of 0.24 is barely better than
    /// having no view at all, which is exactly why the promotion criterion
    /// sits there.
    #[tokio::test]
    async fn a_coin_flip_forecast_scores_a_quarter() {
        let pool = pool().await;
        for i in 0..30 {
            let key = format!("alpaca:SYM{i}");
            record_prediction(&pool, &key, dec!(0.5), dec!(0.5), dec!(100))
                .await
                .unwrap();
            let outcome = if i % 2 == 0 {
                Decimal::ONE
            } else {
                Decimal::ZERO
            };
            record_resolution(&pool, &key, outcome).await.unwrap();
        }
        let (score, n) = brier_score(&pool, 30)
            .await
            .unwrap()
            .expect("enough samples");
        assert_eq!(n, 30);
        assert_eq!(score, dec!(0.25));
    }

    #[tokio::test]
    async fn a_perfect_forecast_scores_zero() {
        let pool = pool().await;
        for i in 0..30 {
            let key = format!("alpaca:SYM{i}");
            record_prediction(&pool, &key, dec!(0.9), Decimal::ONE, dec!(100))
                .await
                .unwrap();
            record_resolution(&pool, &key, Decimal::ONE).await.unwrap();
        }
        let (score, _) = brier_score(&pool, 30).await.unwrap().unwrap();
        assert_eq!(score, Decimal::ZERO);
    }

    /// Too few closes is `None`, not a number. Reported as a figure it would
    /// be read as a pass on a sample that says nothing.
    #[tokio::test]
    async fn too_small_a_sample_reports_nothing_rather_than_a_flattering_number() {
        let pool = pool().await;
        record_prediction(&pool, "alpaca:BTC/USD", dec!(0.9), Decimal::ONE, dec!(100))
            .await
            .unwrap();
        record_resolution(&pool, "alpaca:BTC/USD", Decimal::ONE)
            .await
            .unwrap();
        assert!(brier_score(&pool, 30).await.unwrap().is_none());
        // And with the bar lowered, the same data does score.
        assert!(brier_score(&pool, 1).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn unresolved_forecasts_are_not_scored() {
        let pool = pool().await;
        record_prediction(&pool, "alpaca:BTC/USD", dec!(0.9), dec!(0.8), dec!(100))
            .await
            .unwrap();
        assert!(
            brier_score(&pool, 1).await.unwrap().is_none(),
            "an open position has no outcome to score against"
        );
    }

    /// Long-only: the forecast is "higher at the horizon", so a flat close
    /// did not come true.
    #[tokio::test]
    async fn a_directional_forecast_is_scored_against_the_price_move() {
        let pool = pool().await;

        record_directional_prediction(&pool, "alpaca", "BTC/USD", dec!(0.8), dec!(0.7), dec!(100))
            .await
            .unwrap();
        resolve_directional_prediction(&pool, "alpaca", "BTC/USD", dec!(100), dec!(110))
            .await
            .unwrap();

        record_directional_prediction(&pool, "alpaca", "ETH/USD", dec!(0.8), dec!(0.7), dec!(100))
            .await
            .unwrap();
        resolve_directional_prediction(&pool, "alpaca", "ETH/USD", dec!(100), dec!(100))
            .await
            .unwrap();

        let (score, n) = brier_score(&pool, 2).await.unwrap().unwrap();
        assert_eq!(n, 2);
        // (0.7-1)^2 = 0.09 for the winner; (0.7-0)^2 = 0.49 for the flat one.
        assert_eq!(score, dec!(0.29));
    }

    #[tokio::test]
    async fn two_venues_quoting_the_same_symbol_are_separate_forecasts() {
        assert_ne!(
            directional_key("alpaca", "BTC/USD"),
            directional_key("coinbase", "BTC/USD")
        );
    }

    #[tokio::test]
    async fn test_default_discount_with_no_data() {
        let store = Store::new(":memory:").await.unwrap();
        let discount = compute_discount(store.pool(), 100).await.unwrap();
        assert_eq!(discount, DEFAULT_DISCOUNT);
    }

    #[tokio::test]
    async fn test_record_and_resolve_prediction() {
        let store = Store::new(":memory:").await.unwrap();

        record_prediction(store.pool(), "market_1", dec!(0.85), dec!(0.70), dec!(0.50))
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
