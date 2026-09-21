//! Bounds on the worst day the agent can have.
//!
//! Every other risk control in this crate is per-trade: Kelly caps one
//! position, `portfolio_block` caps total exposure, the stop caps one loss.
//! None of them bounds a *sequence*. Twenty trades that each lose exactly
//! their stop is twenty correctly-sized losses and a ruined account, and
//! nothing on the per-trade path would object to a single one of them.
//!
//! This module is the layer that counts. It answers one question — may the
//! agent open anything right now — and it answers it from numbers that were
//! chosen before the losing streak started, which is the only time anyone
//! chooses them honestly.
//!
//! Note what is *not* here: exits. A tripped breaker stops entries and
//! nothing else. Refusing to close a position because the day went badly is
//! how a bounded loss becomes an unbounded one.

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::config::RiskConfig;

/// Equity marks for one UTC day, as persisted in `daily_equity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayMarks {
    pub day: NaiveDate,
    /// Account value at the first cycle of this UTC day.
    pub starting_equity: Decimal,
    /// The highest equity ever recorded, across every day — not only this one.
    ///
    /// Drawdown is peak-to-trough over the life of the account. Measuring it
    /// from an intraday high would reset the yardstick every midnight, so an
    /// account bleeding 3% a day for a fortnight would never report a
    /// drawdown worth halting on while losing a third of its value.
    pub high_water_mark: Decimal,
}

/// Why the agent stopped opening positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakerTrip {
    /// Down more than `max_daily_loss_pct` of the day's starting equity.
    DailyLossPct {
        lost_pct: Decimal,
        limit_pct: Decimal,
    },
    /// Down more than `max_daily_loss_usd` in absolute cash terms. Separate
    /// from the percentage because at micro capital a percentage is a
    /// rounding error and the cash number is the one that was actually agreed.
    DailyLossUsd { lost: Decimal, limit: Decimal },
    /// Equity is more than `max_drawdown_pct` below its all-time high.
    Drawdown {
        drawdown_pct: Decimal,
        limit_pct: Decimal,
    },
    /// Already opened `max_trades_per_day` positions today.
    TradeCount { today: u32, limit: u32 },
    /// `max_consecutive_losses` closed positions in a row were losses.
    ConsecutiveLosses { count: u32, limit: u32 },
}

/// How long a trip keeps the agent halted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltScope {
    /// Lifts by itself when the UTC day rolls over. The condition that caused
    /// it is defined per-day, so holding the halt past midnight would be
    /// punishing the agent for a number that no longer exists.
    RestOfDay,
    /// Stays until a human clears it. Reserved for conditions that a new day
    /// does not fix.
    UntilResume,
}

impl BreakerTrip {
    pub fn scope(&self) -> HaltScope {
        match self {
            // A drawdown from the all-time high is not a property of today.
            // Sleeping on it changes nothing, so only a person can clear it.
            Self::Drawdown { .. } => HaltScope::UntilResume,
            Self::DailyLossPct { .. }
            | Self::DailyLossUsd { .. }
            | Self::TradeCount { .. }
            | Self::ConsecutiveLosses { .. } => HaltScope::RestOfDay,
        }
    }

    /// Stable identifier for dedupe keys, alerts and `/api/health`. Must not
    /// drift with the `Debug` formatting.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DailyLossPct { .. } => "daily_loss_pct",
            Self::DailyLossUsd { .. } => "daily_loss_usd",
            Self::Drawdown { .. } => "drawdown",
            Self::TradeCount { .. } => "trade_count",
            Self::ConsecutiveLosses { .. } => "consecutive_losses",
        }
    }
}

impl std::fmt::Display for BreakerTrip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DailyLossPct {
                lost_pct,
                limit_pct,
            } => write!(
                f,
                "down {:.2}% on the day, limit {:.2}%",
                lost_pct * Decimal::ONE_HUNDRED,
                limit_pct * Decimal::ONE_HUNDRED
            ),
            Self::DailyLossUsd { lost, limit } => {
                write!(f, "down ${lost} on the day, limit ${limit}")
            }
            Self::Drawdown {
                drawdown_pct,
                limit_pct,
            } => write!(
                f,
                "{:.2}% below the all-time equity high, limit {:.2}%",
                drawdown_pct * Decimal::ONE_HUNDRED,
                limit_pct * Decimal::ONE_HUNDRED
            ),
            Self::TradeCount { today, limit } => {
                write!(f, "{today} positions opened today, limit {limit}")
            }
            Self::ConsecutiveLosses { count, limit } => {
                write!(f, "{count} losing closes in a row, limit {limit}")
            }
        }
    }
}

/// Decide whether the agent may open a new position.
///
/// `equity` is account value including the marked value of open positions —
/// not free cash. Feeding free cash in would read every entry as an instant
/// loss of the full notional and trip the daily-loss breaker on a flat book.
///
/// Every limit is a strict `>` on losses and a `>=` on counts, so a limit of
/// zero means "no loss at all is tolerated" and "no trades at all", rather
/// than silently meaning "disabled". To switch a check off, set it high.
pub fn evaluate(
    equity: Decimal,
    marks: &DayMarks,
    trades_today: u32,
    consecutive_losses: u32,
    cfg: &RiskConfig,
) -> Option<BreakerTrip> {
    // Drawdown is checked first on purpose. It is the only trip that outlives
    // the day, so when two conditions hold at once the operator should be
    // shown the one that will still be there tomorrow.
    if marks.high_water_mark > Decimal::ZERO {
        let drawdown_pct = (marks.high_water_mark - equity) / marks.high_water_mark;
        if drawdown_pct > cfg.max_drawdown_pct {
            return Some(BreakerTrip::Drawdown {
                drawdown_pct,
                limit_pct: cfg.max_drawdown_pct,
            });
        }
    }

    let lost = marks.starting_equity - equity;

    if lost > cfg.max_daily_loss_usd {
        return Some(BreakerTrip::DailyLossUsd {
            lost,
            limit: cfg.max_daily_loss_usd,
        });
    }

    // A starting equity of zero has no percentage to measure against; the
    // cash limit above is the check that still means something there.
    if marks.starting_equity > Decimal::ZERO {
        let lost_pct = lost / marks.starting_equity;
        if lost_pct > cfg.max_daily_loss_pct {
            return Some(BreakerTrip::DailyLossPct {
                lost_pct,
                limit_pct: cfg.max_daily_loss_pct,
            });
        }
    }

    if trades_today >= cfg.max_trades_per_day {
        return Some(BreakerTrip::TradeCount {
            today: trades_today,
            limit: cfg.max_trades_per_day,
        });
    }

    if consecutive_losses >= cfg.max_consecutive_losses {
        return Some(BreakerTrip::ConsecutiveLosses {
            count: consecutive_losses,
            limit: cfg.max_consecutive_losses,
        });
    }

    None
}

/// Absolute cash ceilings that apply only when real money is at stake.
///
/// Deliberately separate from the percentage caps in `portfolio_block`. A
/// percentage of a paper balance is a number the operator never agreed to: a
/// 6% cap on a $100,000 paper account is a $6,000 position, and the first
/// live run on a $100 account inherits the same *code path* with none of the
/// same consequences. These are the numbers chosen for the live rollout, in
/// dollars, and they do not move when the balance does.
///
/// Returns the ceiling to apply to one new position, given what is already
/// committed. `None` means "no absolute ceiling" — paper mode.
pub fn live_notional_ceiling(
    live: bool,
    open_notional: Decimal,
    cfg: &RiskConfig,
) -> Option<Decimal> {
    if !live {
        return None;
    }
    let room_in_total = (cfg.max_live_total_notional_usd - open_notional).max(Decimal::ZERO);
    Some(room_in_total.min(cfg.max_live_notional_per_position_usd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn day(starting: Decimal, hwm: Decimal) -> DayMarks {
        DayMarks {
            day: NaiveDate::from_ymd_opt(2026, 9, 21).unwrap(),
            starting_equity: starting,
            high_water_mark: hwm,
        }
    }

    /// Limits chosen so each check can be tripped without tripping another —
    /// otherwise a test that thinks it is exercising the trade counter is
    /// really exercising whichever check happens to come first.
    fn cfg() -> RiskConfig {
        RiskConfig {
            kelly_fraction: dec!(0.5),
            max_position_pct: dec!(0.06),
            max_total_exposure_pct: dec!(0.30),
            max_positions_per_category: 3,
            min_position_usd: dec!(1),
            max_daily_loss_pct: dec!(0.05),
            max_daily_loss_usd: dec!(1000),
            max_drawdown_pct: dec!(0.15),
            max_trades_per_day: 10,
            max_consecutive_losses: 4,
            max_live_notional_per_position_usd: dec!(10),
            max_live_total_notional_usd: dec!(60),
        }
    }

    #[test]
    fn a_quiet_day_trips_nothing() {
        assert_eq!(
            evaluate(dec!(100), &day(dec!(100), dec!(100)), 0, 0, &cfg()),
            None
        );
    }

    #[test]
    fn a_profitable_day_is_not_a_loss() {
        // Guards the sign of `starting_equity - equity`. Flipped, this is the
        // breaker that halts the agent precisely when it is working.
        assert_eq!(
            evaluate(dec!(150), &day(dec!(100), dec!(150)), 0, 0, &cfg()),
            None
        );
    }

    #[test]
    fn the_daily_percentage_loss_trips_just_past_the_limit_and_not_at_it() {
        let c = cfg();
        // Exactly 5% down: at the limit, not past it.
        assert_eq!(
            evaluate(dec!(95), &day(dec!(100), dec!(100)), 0, 0, &c),
            None,
            "a loss equal to the limit is within it"
        );
        // 5.01% down.
        let trip = evaluate(dec!(94.99), &day(dec!(100), dec!(100)), 0, 0, &c)
            .expect("5.01% down must trip the daily loss breaker");
        assert!(
            matches!(trip, BreakerTrip::DailyLossPct { .. }),
            "expected a percentage trip, got {trip:?}"
        );
        assert_eq!(trip.scope(), HaltScope::RestOfDay);
    }

    #[test]
    fn the_cash_limit_binds_before_the_percentage_at_micro_capital() {
        // The case the absolute cap exists for: a large account where 5% is
        // $250 but the operator agreed to risk $5.
        let mut c = cfg();
        c.max_daily_loss_usd = dec!(5);
        let trip = evaluate(dec!(4994), &day(dec!(5000), dec!(5000)), 0, 0, &c)
            .expect("a $6 loss must trip a $5 cash limit even though it is 0.12%");
        match trip {
            BreakerTrip::DailyLossUsd { lost, limit } => {
                assert_eq!(lost, dec!(6));
                assert_eq!(limit, dec!(5));
            }
            other => panic!("expected a cash trip, got {other:?}"),
        }
    }

    #[test]
    fn drawdown_is_measured_from_the_all_time_high_not_todays_open() {
        let c = cfg();
        // Today opened at 80 and is flat, so the day shows no loss at all.
        // But the account peaked at 100, which is a 20% drawdown.
        let marks = day(dec!(80), dec!(100));
        let trip = evaluate(dec!(80), &marks, 0, 0, &c)
            .expect("20% below the all-time high must trip even on a flat day");
        match trip {
            BreakerTrip::Drawdown {
                drawdown_pct,
                limit_pct,
            } => {
                assert_eq!(drawdown_pct, dec!(0.20));
                assert_eq!(limit_pct, dec!(0.15));
            }
            other => panic!("expected a drawdown trip, got {other:?}"),
        }
        assert_eq!(
            trip.scope(),
            HaltScope::UntilResume,
            "a drawdown halt must not clear itself at midnight"
        );
    }

    #[test]
    fn drawdown_outranks_the_daily_loss_when_both_hold() {
        // Both conditions are true. The operator needs to be told about the
        // one that will still be true tomorrow.
        let trip = evaluate(dec!(50), &day(dec!(100), dec!(100)), 0, 0, &cfg())
            .expect("half the account gone must trip something");
        assert!(
            matches!(trip, BreakerTrip::Drawdown { .. }),
            "the until-resume trip must win over the rest-of-day one, got {trip:?}"
        );
    }

    #[test]
    fn a_zero_high_water_mark_does_not_divide_by_zero() {
        // A brand-new account with nothing recorded yet.
        assert_eq!(
            evaluate(dec!(0), &day(dec!(0), dec!(0)), 0, 0, &cfg()),
            None
        );
    }

    #[test]
    fn the_trade_counter_trips_on_reaching_the_limit_not_after_exceeding_it() {
        let c = cfg();
        assert_eq!(
            evaluate(dec!(100), &day(dec!(100), dec!(100)), 9, 0, &c),
            None,
            "the tenth trade of a ten-trade day is still allowed"
        );
        let trip = evaluate(dec!(100), &day(dec!(100), dec!(100)), 10, 0, &c)
            .expect("the eleventh must be refused");
        assert_eq!(
            trip,
            BreakerTrip::TradeCount {
                today: 10,
                limit: 10
            }
        );
    }

    #[test]
    fn a_losing_streak_trips_on_reaching_the_limit() {
        let c = cfg();
        assert_eq!(
            evaluate(dec!(100), &day(dec!(100), dec!(100)), 0, 3, &c),
            None
        );
        assert!(matches!(
            evaluate(dec!(100), &day(dec!(100), dec!(100)), 0, 4, &c),
            Some(BreakerTrip::ConsecutiveLosses { count: 4, limit: 4 })
        ));
    }

    #[test]
    fn paper_mode_has_no_absolute_ceiling() {
        assert_eq!(live_notional_ceiling(false, dec!(0), &cfg()), None);
    }

    #[test]
    fn the_live_ceiling_is_the_per_position_cap_until_the_total_runs_out() {
        let c = cfg();
        // Nothing open: the per-position cap binds.
        assert_eq!(live_notional_ceiling(true, dec!(0), &c), Some(dec!(10)));
        // $55 of $60 committed: only $5 of room left, less than the $10 cap.
        assert_eq!(live_notional_ceiling(true, dec!(55), &c), Some(dec!(5)));
        // Full.
        assert_eq!(live_notional_ceiling(true, dec!(60), &c), Some(dec!(0)));
    }

    #[test]
    fn the_live_ceiling_never_goes_negative() {
        // Overshooting the total cap — possible after a mark moves against a
        // position — must clamp to zero, not hand back a negative ceiling
        // that a `min` downstream would treat as the binding constraint.
        assert_eq!(live_notional_ceiling(true, dec!(75), &cfg()), Some(dec!(0)));
    }
}
