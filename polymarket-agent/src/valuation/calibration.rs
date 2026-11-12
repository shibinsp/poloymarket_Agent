//! Probability and confidence calibration.
//!
//! Tracks forecasts against resolved outcomes. Probability forecasts are
//! blended with the market prior using a bounded, regularized Brier-loss fit;
//! the separate confidence discount reduces Kelly sizing when self-reported
//! confidence has been overoptimistic.

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
/// Start by giving the model half the distance from the market prior. A model
/// with no proven incremental value should not get to overwrite the market.
pub const DEFAULT_MARKET_MODEL_WEIGHT: Decimal = dec!(0.50);
const MIN_MARKET_CALIBRATION_SAMPLES: usize = 30;
/// Regularizes the fitted blend toward the default until substantially more
/// resolved observations exist.
const MARKET_WEIGHT_PRIOR_SAMPLES: usize = 30;

/// Record a prediction for calibration tracking.
pub async fn record_prediction(
    pool: &SqlitePool,
    market_id: &str,
    claude_confidence: Decimal,
    fair_value: Decimal,
    market_price_at_entry: Decimal,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO confidence_calibration
         (market_id, claude_confidence, fair_value, market_price_at_entry, resolved,
          market_yes_price, model_probability)
         VALUES (?, ?, ?, ?, 0, ?, ?)",
    )
    .bind(market_id)
    .bind(claude_confidence.to_string())
    .bind(fair_value.to_string())
    .bind(market_price_at_entry.to_string())
    .bind(market_price_at_entry.to_string())
    .bind(fair_value.to_string())
    .execute(pool)
    .await
    .context("Failed to record calibration prediction")?;

    Ok(())
}

/// Record a prediction with both the raw model output and the executable
/// YES-market prior. `fair_value` is the calibrated probability used for the
/// decision and Brier score; `model_probability` is preserved to measure
/// whether the model adds value beyond the market.
pub async fn record_prediction_with_market_prior(
    pool: &SqlitePool,
    market_id: &str,
    confidence: Decimal,
    model_probability: Decimal,
    fair_value: Decimal,
    entry_price: Decimal,
    market_yes_price: Decimal,
) -> Result<()> {
    let updated = sqlx::query(
        "UPDATE confidence_calibration
         SET claude_confidence = ?, fair_value = ?, market_price_at_entry = ?,
             market_yes_price = ?, model_probability = ?
         WHERE market_id = ? AND resolved = 0",
    )
    .bind(confidence.to_string())
    .bind(fair_value.to_string())
    .bind(entry_price.to_string())
    .bind(market_yes_price.to_string())
    .bind(model_probability.to_string())
    .bind(market_id)
    .execute(pool)
    .await
    .context("Failed to update market-relative calibration prediction")?;

    if updated.rows_affected() == 0 {
        sqlx::query(
            "INSERT INTO confidence_calibration
             (market_id, claude_confidence, fair_value, market_price_at_entry, resolved,
              market_yes_price, model_probability)
             VALUES (?, ?, ?, ?, 0, ?, ?)",
        )
        .bind(market_id)
        .bind(confidence.to_string())
        .bind(fair_value.to_string())
        .bind(entry_price.to_string())
        .bind(market_yes_price.to_string())
        .bind(model_probability.to_string())
        .execute(pool)
        .await
        .context("Failed to record market-relative calibration prediction")?;
    }

    Ok(())
}

/// Fit a bounded blend weight from resolved prediction-market forecasts.
///
/// The fitted parameter minimizes Brier loss for:
/// `market_prior + weight * (model_probability - market_prior)`.
/// It is constrained to [0, 1], so the model cannot extrapolate beyond its
/// own forecast. The estimate is regularized toward a conservative 50/50
/// blend; with fewer than 30 valid outcomes the default is used unchanged.
pub async fn market_model_weight(pool: &SqlitePool) -> Result<(Decimal, usize)> {
    let rows: Vec<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT model_probability, market_yes_price, actual_outcome
         FROM (
             SELECT model_probability, market_yes_price, actual_outcome,
                    ROW_NUMBER() OVER (
                        PARTITION BY market_id ORDER BY resolved_at DESC, id DESC
                    ) AS market_rank
             FROM confidence_calibration
             WHERE resolved = 1 AND actual_outcome IS NOT NULL
               AND model_probability IS NOT NULL AND market_yes_price IS NOT NULL
               AND market_id NOT LIKE ?
         )
         WHERE market_rank = 1",
    )
    .bind(format!("{DIRECTIONAL_PREFIX}%"))
    .fetch_all(pool)
    .await
    .context("Failed to fetch market-relative calibration data")?;

    let mut numerator = Decimal::ZERO;
    let mut denominator = Decimal::ZERO;
    let mut samples = 0usize;
    for (model, market, outcome) in rows {
        let parsed = model
            .as_deref()
            .and_then(|v| Decimal::from_str(v).ok())
            .zip(market.as_deref().and_then(|v| Decimal::from_str(v).ok()))
            .zip(outcome.as_deref().and_then(|v| Decimal::from_str(v).ok()));
        let Some(((model, market), outcome)) = parsed else {
            continue;
        };
        if !(Decimal::ZERO..=Decimal::ONE).contains(&model)
            || !(Decimal::ZERO..=Decimal::ONE).contains(&market)
            || !(Decimal::ZERO..=Decimal::ONE).contains(&outcome)
        {
            continue;
        }

        let model_delta = model - market;
        numerator += model_delta * (outcome - market);
        denominator += model_delta * model_delta;
        samples += 1;
    }

    if samples < MIN_MARKET_CALIBRATION_SAMPLES {
        return Ok((DEFAULT_MARKET_MODEL_WEIGHT, samples));
    }

    let fitted = if denominator.is_zero() {
        Decimal::ZERO
    } else {
        (numerator / denominator).clamp(Decimal::ZERO, Decimal::ONE)
    };
    let sample_count = Decimal::from(samples as u64);
    let prior_count = Decimal::from(MARKET_WEIGHT_PRIOR_SAMPLES as u64);
    let weight = ((fitted * sample_count + DEFAULT_MARKET_MODEL_WEIGHT * prior_count)
        / (sample_count + prior_count))
        .clamp(Decimal::ZERO, Decimal::ONE);
    Ok((weight, samples))
}

/// Blend a raw model probability toward the market prior using a bounded
/// weight. Invalid inputs are rejected rather than silently clamped.
pub fn blend_with_market_prior(
    model_probability: Decimal,
    market_prior: Decimal,
    model_weight: Decimal,
) -> Option<Decimal> {
    if !(Decimal::ZERO..=Decimal::ONE).contains(&model_probability)
        || !(Decimal::ZERO..=Decimal::ONE).contains(&market_prior)
        || !(Decimal::ZERO..=Decimal::ONE).contains(&model_weight)
    {
        return None;
    }
    Some(market_prior + model_weight * (model_probability - market_prior))
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

/// Prefix marking a row as a continuous-asset forecast.
const DIRECTIONAL_PREFIX: &str = "trade:";

/// Calibration key for a continuous-asset position.
///
/// Keyed by **trade id**, not by symbol.
///
/// Symbol keying looked natural and is wrong: nothing stops two positions
/// being open on one symbol — `busy` only blocks while an order is
/// *unresolved*, so the symbol frees up the moment the entry fills — and
/// `record_resolution` matches the newest unresolved row. With two forecasts
/// outstanding on BTC/USD, the first position to exit resolves the *second*
/// forecast, and every Brier term afterwards pairs a forecast with a
/// different trade's outcome. `created_at` has second granularity, so
/// same-cycle ties broke arbitrarily on top.
///
/// One row per position removes the matching problem rather than narrowing
/// it, and the prefix keeps these separable from the legacy prediction-market
/// rows that share the table.
pub fn directional_key(trade_id: i64) -> String {
    format!("{DIRECTIONAL_PREFIX}{trade_id}")
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
    trade_id: i64,
    confidence: Decimal,
    p_up: Decimal,
    entry_price: Decimal,
) -> Result<()> {
    record_prediction(
        pool,
        &directional_key(trade_id),
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
    trade_id: i64,
    entry_price: Decimal,
    exit_price: Decimal,
) -> Result<()> {
    let outcome = if exit_price > entry_price {
        Decimal::ONE
    } else {
        Decimal::ZERO
    };
    record_resolution(pool, &directional_key(trade_id), outcome).await
}

/// Mean squared error of the forecast probabilities against their outcomes.
///
/// Zero is perfect; 0.25 is what always guessing 50% scores, which is the
/// number to beat before believing any of this has edge. Returns `None` when
/// too few forecasts have resolved to say anything — the promotion criterion
/// asks for ≥30, and a Brier over four closes is noise wearing a decimal
/// point.
pub async fn brier_score(pool: &SqlitePool, minimum: usize) -> Result<Option<(Decimal, usize)>> {
    // Directional forecasts only. The table is shared with the legacy
    // prediction-market path, and an agent.db that has ever run that loop
    // carries resolved rows from a different strategy on a different asset
    // class — which would satisfy the ≥30 sample gate on day one and report
    // a score the promotion table says to read as the venue path's.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT fair_value, actual_outcome FROM confidence_calibration
         WHERE resolved = 1 AND actual_outcome IS NOT NULL
           AND market_id LIKE ?",
    )
    .bind(format!("{DIRECTIONAL_PREFIX}%"))
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

    // `n == 0` is checked independently of `minimum`: a caller passing 0
    // would otherwise divide zero by zero and panic, and the guard reads as
    // though it already covers the empty table.
    if n == 0 || n < minimum {
        return Ok(None);
    }
    Ok(Some((total / Decimal::from(n), n)))
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
            let key = directional_key(i);
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
            let key = directional_key(i);
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
        record_prediction(
            &pool,
            &directional_key(1),
            dec!(0.9),
            Decimal::ONE,
            dec!(100),
        )
        .await
        .unwrap();
        record_resolution(&pool, &directional_key(1), Decimal::ONE)
            .await
            .unwrap();
        assert!(brier_score(&pool, 30).await.unwrap().is_none());
        // And with the bar lowered, the same data does score.
        assert!(brier_score(&pool, 1).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn unresolved_forecasts_are_not_scored() {
        let pool = pool().await;
        record_prediction(&pool, &directional_key(1), dec!(0.9), dec!(0.8), dec!(100))
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

        record_directional_prediction(&pool, 1, dec!(0.8), dec!(0.7), dec!(100))
            .await
            .unwrap();
        resolve_directional_prediction(&pool, 1, dec!(100), dec!(110))
            .await
            .unwrap();

        record_directional_prediction(&pool, 2, dec!(0.8), dec!(0.7), dec!(100))
            .await
            .unwrap();
        resolve_directional_prediction(&pool, 2, dec!(100), dec!(100))
            .await
            .unwrap();

        let (score, n) = brier_score(&pool, 2).await.unwrap().unwrap();
        assert_eq!(n, 2);
        // (0.7-1)^2 = 0.09 for the winner; (0.7-0)^2 = 0.49 for the flat one.
        assert_eq!(score, dec!(0.29));
    }

    /// One row per position. Symbol keying collided whenever two positions
    /// were open on one symbol, and resolved them in the wrong order.
    #[tokio::test]
    async fn two_positions_on_one_symbol_keep_separate_forecasts() {
        assert_ne!(directional_key(1), directional_key(2));
    }

    /// The failure symbol keying produced: two forecasts outstanding on the
    /// same symbol, and the first exit resolving the *later* forecast.
    #[tokio::test]
    async fn each_position_is_scored_against_its_own_forecast() {
        let pool = pool().await;

        // Trade 1 forecast 0.60, trade 2 forecast 0.90, both on BTC/USD.
        record_directional_prediction(&pool, 1, dec!(0.8), dec!(0.60), dec!(100))
            .await
            .unwrap();
        record_directional_prediction(&pool, 2, dec!(0.8), dec!(0.90), dec!(100))
            .await
            .unwrap();

        // Trade 1 exits at a loss; trade 2 exits at a profit.
        resolve_directional_prediction(&pool, 1, dec!(100), dec!(90))
            .await
            .unwrap();
        resolve_directional_prediction(&pool, 2, dec!(100), dec!(120))
            .await
            .unwrap();

        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT fair_value, actual_outcome FROM confidence_calibration
             WHERE market_id = ? OR market_id = ? ORDER BY market_id",
        )
        .bind(directional_key(1))
        .bind(directional_key(2))
        .fetch_all(&pool)
        .await
        .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "0.60");
        assert_eq!(rows[0].1, "0", "the 0.60 forecast lost");
        assert_eq!(rows[1].0, "0.90");
        assert_eq!(rows[1].1, "1", "the 0.90 forecast won");
    }

    /// The table is shared with the legacy prediction-market path. An
    /// agent.db that ever ran that loop carries resolved rows from a
    /// different strategy on a different asset class, and counting them
    /// satisfies the ≥30 gate on day one with a number from somewhere else.
    #[tokio::test]
    async fn legacy_prediction_market_rows_are_not_scored() {
        let pool = pool().await;
        for i in 0..40 {
            let key = format!("0xcondition{i}");
            record_prediction(&pool, &key, dec!(0.9), Decimal::ONE, dec!(0.5))
                .await
                .unwrap();
            record_resolution(&pool, &key, Decimal::ONE).await.unwrap();
        }
        assert!(
            brier_score(&pool, 30).await.unwrap().is_none(),
            "40 legacy rows must not satisfy the venue path's sample gate"
        );
    }

    #[tokio::test]
    async fn a_zero_minimum_on_an_empty_table_does_not_divide_by_zero() {
        let pool = pool().await;
        assert!(brier_score(&pool, 0).await.unwrap().is_none());
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

    #[test]
    fn market_prior_blend_is_bounded_and_moves_toward_market() {
        assert_eq!(
            blend_with_market_prior(dec!(0.9), dec!(0.5), dec!(0.5)),
            Some(dec!(0.70))
        );
        assert_eq!(
            blend_with_market_prior(dec!(0.9), dec!(0.5), Decimal::ZERO),
            Some(dec!(0.5))
        );
        assert_eq!(
            blend_with_market_prior(dec!(1.1), dec!(0.5), dec!(0.5)),
            None
        );
    }

    #[tokio::test]
    async fn market_model_weight_uses_conservative_default_until_minimum_sample() {
        let pool = pool().await;
        for i in 0..29 {
            let key = format!("sample-{i}");
            record_prediction(&pool, &key, dec!(0.8), dec!(0.9), dec!(0.5))
                .await
                .unwrap();
            record_resolution(&pool, &key, Decimal::ONE).await.unwrap();
        }

        assert_eq!(
            market_model_weight(&pool).await.unwrap(),
            (DEFAULT_MARKET_MODEL_WEIGHT, 29)
        );
    }

    #[tokio::test]
    async fn repeated_open_market_forecasts_replace_instead_of_double_counting() {
        let pool = pool().await;
        record_prediction_with_market_prior(
            &pool,
            "same-market",
            dec!(0.7),
            dec!(0.8),
            dec!(0.65),
            dec!(0.51),
            dec!(0.52),
        )
        .await
        .unwrap();
        record_prediction_with_market_prior(
            &pool,
            "same-market",
            dec!(0.75),
            dec!(0.7),
            dec!(0.61),
            dec!(0.50),
            dec!(0.53),
        )
        .await
        .unwrap();

        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT fair_value, model_probability, market_yes_price
             FROM confidence_calibration WHERE market_id = ? AND resolved = 0",
        )
        .bind("same-market")
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0],
            ("0.61".to_string(), "0.7".to_string(), "0.53".to_string())
        );
    }

    #[tokio::test]
    async fn market_model_weight_reduces_overconfident_forecasts() {
        let pool = pool().await;
        for i in 0..40 {
            let key = format!("sample-{i}");
            // The forecast swings strongly, but outcomes are independent of
            // that swing; the market's 50/50 prior is the better predictor.
            let model = if i % 2 == 0 { dec!(0.9) } else { dec!(0.1) };
            record_prediction(&pool, &key, dec!(0.8), model, dec!(0.5))
                .await
                .unwrap();
            let outcome = if i % 2 == 0 {
                Decimal::ZERO
            } else {
                Decimal::ONE
            };
            record_resolution(&pool, &key, outcome).await.unwrap();
        }

        let (weight, samples) = market_model_weight(&pool).await.unwrap();
        assert_eq!(samples, 40);
        assert!(weight < DEFAULT_MARKET_MODEL_WEIGHT);
        assert!(weight >= Decimal::ZERO);
    }

    #[tokio::test]
    async fn market_model_weight_increases_only_when_model_beats_market_prior() {
        let pool = pool().await;
        for i in 0..40 {
            let key = format!("sample-{i}");
            let model = if i % 2 == 0 { dec!(0.9) } else { dec!(0.1) };
            record_prediction(&pool, &key, dec!(0.8), model, dec!(0.5))
                .await
                .unwrap();
            let outcome = if i % 2 == 0 {
                Decimal::ONE
            } else {
                Decimal::ZERO
            };
            record_resolution(&pool, &key, outcome).await.unwrap();
        }

        let (weight, samples) = market_model_weight(&pool).await.unwrap();
        assert_eq!(samples, 40);
        assert!(weight > DEFAULT_MARKET_MODEL_WEIGHT);
        assert!(weight <= Decimal::ONE);
    }
}
