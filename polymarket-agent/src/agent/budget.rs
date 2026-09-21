//! The day's valuation spend, reserved *before* it is incurred.
//!
//! The old check read the day's total from SQLite once at the top of the
//! cycle and then spawned a batch of valuations in parallel. Every one of
//! them was launched against the same pre-batch number, so ten concurrent
//! calls could each observe "$0.40 of $0.50 spent, fine" and collectively
//! spend $1.30. Nothing was wrong with the arithmetic; it was just being done
//! before the spending rather than during it.
//!
//! A reservation closes that window. Cost is claimed against the day's budget
//! at the moment a call is *decided on*, under a lock, and converted to real
//! spend when the call returns. A call that fails releases its claim on drop,
//! so a dead endpoint cannot silently consume the day's budget.
//!
//! What this cannot do is bound the overshoot of a single call: the estimate
//! is reserved, the actual is settled, and a call that returns far more
//! tokens than estimated books the difference. One call's worth of overshoot
//! is the designed-in error, and it is why the estimate is taken from the
//! provider's own per-million pricing rather than a guess.

use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

/// A reservation was refused because the day's budget is committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetExhausted {
    pub requested: Decimal,
    pub spent: Decimal,
    pub reserved: Decimal,
    pub budget: Decimal,
}

impl std::fmt::Display for BudgetExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "daily valuation budget exhausted: ${} spent + ${} reserved + ${} requested exceeds ${}",
            self.spent, self.reserved, self.requested, self.budget
        )
    }
}

impl std::error::Error for BudgetExhausted {}

#[derive(Debug)]
struct LedgerState {
    day: NaiveDate,
    spent: Decimal,
    reserved: Decimal,
}

/// Tracks one UTC day's spend against `agent.daily_api_budget`.
#[derive(Debug)]
pub struct BudgetLedger {
    budget: Decimal,
    state: Mutex<LedgerState>,
}

impl BudgetLedger {
    /// `already_spent` is the day's spend recovered from `api_costs`, so a
    /// restart mid-day does not hand the agent a fresh budget.
    pub fn new(budget: Decimal, day: NaiveDate, already_spent: Decimal) -> Self {
        Self {
            budget,
            state: Mutex::new(LedgerState {
                day,
                spent: already_spent,
                reserved: Decimal::ZERO,
            }),
        }
    }

    pub fn budget(&self) -> Decimal {
        self.budget
    }

    /// Roll the day over if `today` has moved on.
    ///
    /// Spend resets to zero because a new UTC day has, by definition, no
    /// spend yet. In-flight reservations are deliberately *not* cleared: the
    /// calls they belong to are still running and will settle into the new
    /// day, which is where their cost is actually incurred.
    fn roll_to(state: &mut LedgerState, today: NaiveDate) {
        if state.day != today {
            state.day = today;
            state.spent = Decimal::ZERO;
        }
    }

    /// Budget not yet spent or claimed.
    pub fn remaining(&self, today: NaiveDate) -> Decimal {
        let mut state = self.lock();
        Self::roll_to(&mut state, today);
        (self.budget - state.spent - state.reserved).max(Decimal::ZERO)
    }

    /// How many calls of `estimate` each still fit in the day's budget.
    ///
    /// Used to bound a parallel batch before it is spawned, so the agent
    /// starts the number of valuations it can pay for rather than starting
    /// ten and discovering the limit on the seventh.
    pub fn affordable_calls(&self, today: NaiveDate, estimate: Decimal) -> usize {
        if estimate <= Decimal::ZERO {
            // A free provider imposes no budget limit. The caller's own
            // `max_evaluations` is then the only bound, which is correct —
            // and is warned about at startup, because it also makes the
            // edge-justifies-cost gate inert.
            return usize::MAX;
        }
        let remaining = self.remaining(today);
        if remaining <= Decimal::ZERO {
            return 0;
        }
        // Saturating rather than wrapping: a very cheap provider against a
        // large budget overflows u64 long before it runs out of money, and
        // "as many as you like" is the right answer there.
        (remaining / estimate)
            .floor()
            .to_u64()
            .map_or(usize::MAX, |n| usize::try_from(n).unwrap_or(usize::MAX))
    }

    /// Claim `estimate` against the day's budget.
    ///
    /// Fails atomically: either the claim is recorded and a `Reservation` is
    /// returned, or nothing changes. Two callers racing cannot both succeed
    /// on the last dollar.
    pub fn reserve(
        self: &Arc<Self>,
        today: NaiveDate,
        estimate: Decimal,
    ) -> Result<Reservation, BudgetExhausted> {
        let mut state = self.lock();
        Self::roll_to(&mut state, today);

        // A negative estimate would *credit* the ledger. Clamp rather than
        // trust the caller's arithmetic.
        let estimate = estimate.max(Decimal::ZERO);

        if state.spent + state.reserved + estimate > self.budget {
            return Err(BudgetExhausted {
                requested: estimate,
                spent: state.spent,
                reserved: state.reserved,
                budget: self.budget,
            });
        }

        state.reserved += estimate;
        drop(state);

        Ok(Reservation {
            ledger: Arc::clone(self),
            amount: estimate,
            settled: false,
        })
    }

    fn release(&self, amount: Decimal) {
        let mut state = self.lock();
        state.reserved = (state.reserved - amount).max(Decimal::ZERO);
    }

    fn settle(&self, reserved: Decimal, actual: Decimal) {
        let mut state = self.lock();
        state.reserved = (state.reserved - reserved).max(Decimal::ZERO);
        state.spent += actual.max(Decimal::ZERO);
    }

    /// The lock guards three `Decimal`s and is never held across an await.
    /// A poisoned lock means a panic happened mid-update; the amounts are
    /// still individually valid, and refusing to spend anything for the rest
    /// of the process because of it would be a worse failure than continuing.
    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Spend and claims as they stand — for `/api/health` and tests.
    pub fn snapshot(&self, today: NaiveDate) -> BudgetSnapshot {
        let mut state = self.lock();
        Self::roll_to(&mut state, today);
        BudgetSnapshot {
            day: state.day,
            spent: state.spent,
            reserved: state.reserved,
            budget: self.budget,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BudgetSnapshot {
    pub day: NaiveDate,
    pub spent: Decimal,
    pub reserved: Decimal,
    pub budget: Decimal,
}

/// A claim on the day's budget, held for the duration of one model call.
///
/// Dropping without settling releases the claim. That is the path a failed
/// call takes, and it matters: without it, every timeout would permanently
/// consume its estimate and a provider outage would exhaust the day's budget
/// having produced nothing.
#[derive(Debug)]
pub struct Reservation {
    ledger: Arc<BudgetLedger>,
    amount: Decimal,
    settled: bool,
}

impl Reservation {
    /// Convert the claim into spend. `actual` is what the call really cost.
    pub fn settle(mut self, actual: Decimal) {
        self.ledger.settle(self.amount, actual);
        self.settled = true;
    }

    pub fn amount(&self) -> Decimal {
        self.amount
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.settled {
            self.ledger.release(self.amount);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()
    }

    fn tomorrow() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 22).unwrap()
    }

    fn ledger(budget: Decimal, spent: Decimal) -> Arc<BudgetLedger> {
        Arc::new(BudgetLedger::new(budget, today(), spent))
    }

    #[test]
    fn a_reservation_reduces_what_is_left_before_anything_is_spent() {
        // The whole point: the money is committed at decision time, not at
        // settlement time. If this reads $1.00 the concurrency hole is back.
        let l = ledger(dec!(1.00), Decimal::ZERO);
        let _r = l.reserve(today(), dec!(0.30)).expect("first reserve");
        assert_eq!(l.remaining(today()), dec!(0.70));
    }

    #[test]
    fn concurrent_reservations_cannot_both_take_the_last_dollar() {
        let l = ledger(dec!(1.00), Decimal::ZERO);
        let _a = l.reserve(today(), dec!(0.60)).expect("first fits");
        let b = l.reserve(today(), dec!(0.60));
        assert!(
            b.is_err(),
            "the second $0.60 claim against a $1.00 budget must be refused"
        );
        // And the refusal must not have consumed anything.
        assert_eq!(l.remaining(today()), dec!(0.40));
    }

    #[test]
    fn settling_for_less_than_estimated_returns_the_difference() {
        let l = ledger(dec!(1.00), Decimal::ZERO);
        let r = l.reserve(today(), dec!(0.30)).expect("reserve");
        r.settle(dec!(0.10));
        assert_eq!(l.remaining(today()), dec!(0.90));
        let snap = l.snapshot(today());
        assert_eq!(snap.spent, dec!(0.10));
        assert_eq!(snap.reserved, Decimal::ZERO);
    }

    #[test]
    fn settling_for_more_than_estimated_books_the_overshoot() {
        // Documented behaviour, not an accident: one call may exceed its
        // estimate, and the budget absorbs it rather than losing track of it.
        let l = ledger(dec!(1.00), Decimal::ZERO);
        let r = l.reserve(today(), dec!(0.10)).expect("reserve");
        r.settle(dec!(0.25));
        assert_eq!(l.snapshot(today()).spent, dec!(0.25));
        assert_eq!(l.remaining(today()), dec!(0.75));
    }

    #[test]
    fn a_dropped_reservation_releases_its_claim() {
        // The failed-call path. Without this a provider outage burns the
        // day's budget on calls that returned nothing.
        let l = ledger(dec!(1.00), Decimal::ZERO);
        {
            let _r = l.reserve(today(), dec!(0.90)).expect("reserve");
            assert_eq!(l.remaining(today()), dec!(0.10));
        }
        assert_eq!(
            l.remaining(today()),
            dec!(1.00),
            "dropping without settling must give the claim back"
        );
        assert_eq!(l.snapshot(today()).spent, Decimal::ZERO);
    }

    #[test]
    fn spend_carried_in_from_the_database_counts_against_the_budget() {
        // A restart mid-day must not hand the agent a fresh $0.50.
        let l = ledger(dec!(0.50), dec!(0.45));
        assert_eq!(l.remaining(today()), dec!(0.05));
        assert!(l.reserve(today(), dec!(0.10)).is_err());
    }

    #[test]
    fn the_budget_resets_when_the_utc_day_rolls_over() {
        let l = ledger(dec!(0.50), dec!(0.50));
        assert_eq!(l.remaining(today()), Decimal::ZERO);
        assert_eq!(l.remaining(tomorrow()), dec!(0.50));
        assert!(l.reserve(tomorrow(), dec!(0.40)).is_ok());
    }

    #[test]
    fn an_in_flight_reservation_survives_the_day_rolling_over() {
        // The call is still running; its cost will be incurred today, so its
        // claim must still be held today.
        let l = ledger(dec!(1.00), dec!(0.50));
        let _r = l.reserve(today(), dec!(0.20)).expect("reserve");
        assert_eq!(
            l.remaining(tomorrow()),
            dec!(0.80),
            "yesterday's spend clears but the live claim does not"
        );
    }

    #[test]
    fn a_call_that_exactly_fills_the_budget_is_allowed() {
        let l = ledger(dec!(1.00), Decimal::ZERO);
        assert!(
            l.reserve(today(), dec!(1.00)).is_ok(),
            "spending the budget exactly is not overspending it"
        );
    }

    #[test]
    fn a_negative_estimate_cannot_credit_the_ledger() {
        let l = ledger(dec!(1.00), dec!(1.00));
        let r = l.reserve(today(), dec!(-5.00)).expect("clamped to zero");
        assert_eq!(r.amount(), Decimal::ZERO);
        assert_eq!(l.remaining(today()), Decimal::ZERO);
    }

    #[test]
    fn affordable_calls_floors_rather_than_rounding() {
        let l = ledger(dec!(1.00), Decimal::ZERO);
        // $1.00 / $0.30 = 3.33 -> three calls, not four.
        assert_eq!(l.affordable_calls(today(), dec!(0.30)), 3);
    }

    #[test]
    fn affordable_calls_is_zero_once_the_budget_is_gone() {
        let l = ledger(dec!(1.00), dec!(1.00));
        assert_eq!(l.affordable_calls(today(), dec!(0.01)), 0);
    }

    #[test]
    fn a_free_provider_imposes_no_call_limit() {
        let l = ledger(dec!(0.50), dec!(0.50));
        assert_eq!(l.affordable_calls(today(), Decimal::ZERO), usize::MAX);
    }

    #[test]
    fn reservations_are_atomic_across_threads() {
        // The property the Mutex exists for, exercised the way it actually
        // fails: many threads racing for a budget that fits only some of them.
        let l = ledger(dec!(1.00), Decimal::ZERO);
        let granted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..32 {
            let l = Arc::clone(&l);
            let granted = Arc::clone(&granted);
            handles.push(std::thread::spawn(move || {
                if let Ok(r) = l.reserve(today(), dec!(0.10)) {
                    granted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    r.settle(dec!(0.10));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            granted.load(std::sync::atomic::Ordering::SeqCst),
            10,
            "exactly ten $0.10 calls fit in a $1.00 budget"
        );
        assert_eq!(l.snapshot(today()).spent, dec!(1.00));
    }
}
