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
use tracing::warn;

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

/// Brier score of an uninformative forecast.
///
/// A constant 0.5 on a binary outcome scores exactly this, so it is the line
/// between "the model knows something" and "the model is guessing". Worse than
/// this is the only evidence that justifies trading smaller than configured.
pub const UNINFORMATIVE_BRIER: Decimal = dec!(0.25);

/// How much of the configured size a forecast record justifies, in `(floor, 1]`.
///
/// **Never above 1.0, by construction.** Evidence of skill does not buy extra
/// risk: the configured size is a ceiling the operator chose, and a lucky
/// streak is not a reason to exceed it. This can only make the agent smaller
/// than it was told to be.
///
/// `None` below `min_samples`, which means "no opinion" and leaves sizing
/// exactly as configured. That is deliberately unlike the old
/// `DEFAULT_DISCOUNT`, which applied a flat 15% haircut *before any evidence
/// existed* — a made-up number, not a prior.
///
/// Scoring: at or better than [`UNINFORMATIVE_BRIER`] the multiplier is 1.0.
/// Beyond it the multiplier falls linearly, reaching `floor` when the score is
/// twice the uninformative one. The curve is arbitrary in its steepness but
/// not in its shape — it is monotone, continuous at the threshold, and
/// bounded at both ends, which is what keeps one bad week from zeroing the
/// book and one good week from unbounding it.
pub async fn size_multiplier(
    pool: &SqlitePool,
    min_samples: usize,
    floor: Decimal,
) -> Result<Option<Decimal>> {
    let Some((brier, _n)) = brier_score(pool, min_samples).await? else {
        return Ok(None);
    };
    Ok(Some(multiplier_for_brier(brier, floor)))
}

/// The curve, separated from the query so it can be tested without a database.
pub fn multiplier_for_brier(brier: Decimal, floor: Decimal) -> Decimal {
    // Clamp the floor itself: a config of 2.0 must not become a licence to
    // double the size, and a negative floor must not produce a negative one.
    let floor = floor.clamp(Decimal::ZERO, Decimal::ONE);

    // How far past uninformative, as a fraction of one more whole
    // `UNINFORMATIVE_BRIER` of badness. Negative for a good record, which the
    // clamp below turns into 1.0 — there is deliberately no early return for
    // that case, because a branch the clamp already covers is a branch no
    // test can distinguish from its own absence.
    let excess = (brier - UNINFORMATIVE_BRIER) / UNINFORMATIVE_BRIER;
    let scaled = Decimal::ONE - excess * (Decimal::ONE - floor);
    scaled.clamp(floor, Decimal::ONE)
}

#[cfg(test)]
mod tests {
    /// Skill does not buy extra risk. The configured size is a ceiling the
    /// operator chose; a good streak is not a reason to exceed it, and a
    /// multiplier above 1.0 would silently turn calibration into leverage.
    #[test]
    fn a_good_forecast_record_never_earns_more_than_the_configured_size() {
        for brier in [dec!(0), dec!(0.05), dec!(0.2), UNINFORMATIVE_BRIER] {
            assert_eq!(
                multiplier_for_brier(brier, dec!(0.5)),
                Decimal::ONE,
                "brier {brier} must not exceed the configured size"
            );
        }
    }

    /// Worse than a coin flip is the only evidence that justifies shrinking.
    #[test]
    fn a_worse_than_uninformative_record_shrinks_the_size() {
        let floor = dec!(0.5);
        // Halfway to twice-uninformative: halfway from 1.0 to the floor.
        assert_eq!(multiplier_for_brier(dec!(0.375), floor), dec!(0.75));
        // Twice uninformative: exactly the floor.
        assert_eq!(multiplier_for_brier(dec!(0.5), floor), floor);
    }

    /// The worst possible record still trades the floor rather than zero: a
    /// multiplier of 0 is a halt dressed as a size, and halting is the kill
    /// switch's job, where it is visible and needs a human to clear.
    #[test]
    fn the_floor_holds_however_bad_the_record_is() {
        let floor = dec!(0.5);
        assert_eq!(multiplier_for_brier(dec!(0.9), floor), floor);
        assert_eq!(multiplier_for_brier(Decimal::ONE, floor), floor);
        assert_eq!(multiplier_for_brier(dec!(99), floor), floor);
    }

    /// A misconfigured floor must not become a licence to size up.
    #[test]
    fn an_out_of_range_floor_is_clamped_rather_than_obeyed() {
        assert_eq!(multiplier_for_brier(dec!(0.5), dec!(2)), Decimal::ONE);
        assert!(multiplier_for_brier(dec!(0.9), dec!(-1)) >= Decimal::ZERO);
    }

    /// The curve is continuous at the threshold: one resolution crossing the
    /// line must not step the size.
    #[test]
    fn the_curve_is_continuous_where_it_starts_to_bite() {
        let floor = dec!(0.5);
        let just_under = multiplier_for_brier(dec!(0.2499), floor);
        let just_over = multiplier_for_brier(dec!(0.2501), floor);
        assert_eq!(just_under, Decimal::ONE);
        assert!(
            (just_under - just_over).abs() < dec!(0.01),
            "a hair past the threshold should not step: {just_under} vs {just_over}"
        );
    }

    /// Below the sample minimum there is no opinion — sizing is left exactly
    /// as configured. A number invented from four resolutions is worse than
    /// no number, because it looks like evidence.
    #[tokio::test]
    async fn no_opinion_until_there_are_enough_resolutions() {
        let store = Store::new(":memory:").await.unwrap();
        assert_eq!(
            size_multiplier(store.pool(), 30, dec!(0.5)).await.unwrap(),
            None
        );
    }

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
}
