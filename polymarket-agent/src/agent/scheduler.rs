//! When the agent should wake up next.
//!
//! The flat `cycle_interval` sleep this replaces has two failure modes once
//! more than one venue exists. Overnight and at weekends it wakes every ten
//! minutes to rediscover that the equity market is shut — burning cycles, log
//! lines and, if a scan slips through, API budget. And at the open it starts
//! up to a full interval late, which on a ten-minute cadence means missing the
//! first ten minutes of the session: the widest spreads and the most movement
//! of the day.
//!
//! So: wake on the normal cadence while anything is tradeable, sleep until the
//! bell when nothing is, and never sleep so long that positions go unmarked
//! and orders unreconciled.

use chrono::{DateTime, Duration, Utc};

use crate::venue::VenueRegistry;

/// Lower bound on the cadence. A misconfigured `cycle_interval_seconds = 0`
/// would otherwise spin the loop as fast as the venue APIs allow, which is a
/// rate-limit ban and, once the valuation model is wired in, real money.
const MIN_CADENCE: Duration = Duration::seconds(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    /// Something is tradeable right now — run again on the normal cadence.
    Cycle,
    /// Every venue is shut. Sleep until the first one reopens.
    SessionOpen,
    /// Every venue is shut and none reopens within `max_sleep`. Wake anyway:
    /// open positions still need marking, outstanding orders still need
    /// reconciling, and the health endpoint still needs a heartbeat or the
    /// watchdog will call a sleeping agent a stalled one.
    Heartbeat,
}

impl WakeReason {
    /// For logs — the distinction only matters when reading them.
    pub fn as_str(&self) -> &'static str {
        match self {
            WakeReason::Cycle => "cycle",
            WakeReason::SessionOpen => "session_open",
            WakeReason::Heartbeat => "heartbeat",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakePlan {
    pub at: DateTime<Utc>,
    pub reason: WakeReason,
}

impl WakePlan {
    /// How long to sleep from `now`. A wake time already in the past means
    /// "immediately" rather than a panic on a negative duration — clock jumps
    /// and a cycle that outran its own interval both produce one.
    pub fn sleep_from(&self, now: DateTime<Utc>) -> std::time::Duration {
        (self.at - now)
            .to_std()
            .unwrap_or(std::time::Duration::ZERO)
    }
}

/// Decide the next wake.
///
/// `max_sleep` bounds the closed-market sleep only; it never shortens the
/// trading cadence, because an operator who sets a long cadence means it.
pub fn next_wake(
    registry: &VenueRegistry,
    now: DateTime<Utc>,
    cycle_interval: Duration,
    max_sleep: Duration,
) -> WakePlan {
    let cadence = cycle_interval.max(MIN_CADENCE);

    // No venues configured means the legacy Polymarket path, which has no
    // session of its own and simply runs on the cadence.
    if registry.is_empty() || registry.trades_at(now) {
        return WakePlan {
            at: now + cadence,
            reason: WakeReason::Cycle,
        };
    }

    let heartbeat = now + max_sleep.max(MIN_CADENCE);
    match registry.next_open_after(now) {
        // Waking a touch early is harmless; waking late means trading into a
        // session that has already moved without us.
        Some(open) if open <= heartbeat => WakePlan {
            at: open,
            reason: WakeReason::SessionOpen,
        },
        _ => WakePlan {
            at: heartbeat,
            reason: WakeReason::Heartbeat,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    use crate::venue::session::TradingSession;
    use crate::venue::test_support::StubVenue;
    use crate::venue::types::AssetClass;
    use crate::venue::Venue;

    fn equities_only(id: &str) -> Box<dyn Venue> {
        Box::new(StubVenue::with_classes(
            id,
            TradingSession::us_equity_regular(),
            &[("AAPL", AssetClass::Equity)],
            false,
        ))
    }

    fn nse_equities(id: &str) -> Box<dyn Venue> {
        Box::new(StubVenue::with_classes(
            id,
            TradingSession::nse(),
            &[("RELIANCE", AssetClass::Equity)],
            false,
        ))
    }

    /// Alpaca's real shape: one venue, an equity session, and a crypto book
    /// that ignores it.
    fn equities_and_crypto(id: &str) -> Box<dyn Venue> {
        Box::new(StubVenue::with_classes(
            id,
            TradingSession::us_equity_regular(),
            &[
                ("AAPL", AssetClass::Equity),
                ("BTC/USD", AssetClass::CryptoSpot),
            ],
            false,
        ))
    }

    const CADENCE: Duration = Duration::minutes(10);
    const MAX_SLEEP: Duration = Duration::hours(1);

    /// Wednesday 2026-09-16, 15:00 UTC = 11:00 ET — the US market is open.
    fn during_session() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 16, 15, 0, 0).unwrap()
    }

    /// Saturday 2026-09-19, 15:00 UTC — no equity session for two days.
    fn weekend() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 19, 15, 0, 0).unwrap()
    }

    /// Wednesday 2026-09-16, 03:00 UTC = 23:00 ET Tuesday. The US open is
    /// 10.5 hours away; the NSE open is 45 minutes away.
    fn overnight() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 16, 3, 0, 0).unwrap()
    }

    #[test]
    fn no_venues_configured_runs_on_the_cadence() {
        let registry = VenueRegistry::new(Vec::new());
        let now = weekend();
        let plan = next_wake(&registry, now, CADENCE, MAX_SLEEP);
        assert_eq!(plan.reason, WakeReason::Cycle);
        assert_eq!(plan.at, now + CADENCE);
    }

    #[test]
    fn open_session_runs_on_the_cadence() {
        let registry = VenueRegistry::new(vec![equities_only("alpaca")]);
        let now = during_session();
        let plan = next_wake(&registry, now, CADENCE, MAX_SLEEP);
        assert_eq!(plan.reason, WakeReason::Cycle);
        assert_eq!(plan.at, now + CADENCE);
    }

    /// The regression this module exists to prevent. A venue whose *session*
    /// is shut but whose crypto book is not must keep cycling; sleeping to the
    /// equity open here would stop 24/7 trading every weekend — the same
    /// mistake that gating instrument discovery on the venue session made.
    #[test]
    fn crypto_keeps_cycling_while_the_equity_session_is_shut() {
        let registry = VenueRegistry::new(vec![equities_and_crypto("alpaca")]);
        let now = weekend();
        assert!(registry.open_at(now).is_empty(), "the session must be shut");

        let plan = next_wake(&registry, now, CADENCE, MAX_SLEEP);
        assert_eq!(plan.reason, WakeReason::Cycle);
        assert_eq!(plan.at, now + CADENCE);
    }

    #[test]
    fn sleeps_to_the_open_when_it_lands_inside_max_sleep() {
        let registry = VenueRegistry::new(vec![equities_only("alpaca")]);
        // 13:00 UTC Wednesday = 09:00 ET, half an hour before the bell.
        let now = Utc.with_ymd_and_hms(2026, 9, 16, 13, 0, 0).unwrap();
        let plan = next_wake(&registry, now, CADENCE, MAX_SLEEP);

        assert_eq!(plan.reason, WakeReason::SessionOpen);
        assert_eq!(
            plan.at,
            Utc.with_ymd_and_hms(2026, 9, 16, 13, 30, 0).unwrap()
        );
        // The point of the exercise: it does not wake a full cadence late.
        assert!(plan.at < now + MAX_SLEEP);
    }

    #[test]
    fn heartbeats_when_the_open_is_further_out_than_max_sleep() {
        let registry = VenueRegistry::new(vec![equities_only("alpaca")]);
        let now = overnight();
        let plan = next_wake(&registry, now, CADENCE, MAX_SLEEP);

        assert_eq!(plan.reason, WakeReason::Heartbeat);
        assert_eq!(plan.at, now + MAX_SLEEP);
    }

    #[test]
    fn weekend_heartbeats_rather_than_sleeping_two_days() {
        let registry = VenueRegistry::new(vec![equities_only("alpaca")]);
        let now = weekend();
        let plan = next_wake(&registry, now, CADENCE, MAX_SLEEP);

        assert_eq!(plan.reason, WakeReason::Heartbeat);
        assert_eq!(plan.at, now + MAX_SLEEP);
    }

    /// The earliest venue to reopen sets the alarm, not the first configured.
    #[test]
    fn wakes_for_the_earliest_reopening_venue() {
        let registry = VenueRegistry::new(vec![equities_only("alpaca"), nse_equities("zerodha")]);
        let now = overnight();
        let plan = next_wake(&registry, now, CADENCE, Duration::hours(12));

        assert_eq!(plan.reason, WakeReason::SessionOpen);
        // 03:45 UTC = 09:15 IST.
        assert_eq!(
            plan.at,
            Utc.with_ymd_and_hms(2026, 9, 16, 3, 45, 0).unwrap()
        );
    }

    #[test]
    fn a_zero_cadence_still_sleeps() {
        let registry = VenueRegistry::new(vec![equities_only("alpaca")]);
        let now = during_session();
        let plan = next_wake(&registry, now, Duration::zero(), MAX_SLEEP);
        assert_eq!(plan.at, now + MIN_CADENCE);
        assert!(plan.sleep_from(now) > std::time::Duration::ZERO);
    }

    #[test]
    fn a_wake_time_in_the_past_sleeps_for_nothing_rather_than_panicking() {
        let now = during_session();
        let plan = WakePlan {
            at: now - Duration::hours(1),
            reason: WakeReason::Cycle,
        };
        assert_eq!(plan.sleep_from(now), std::time::Duration::ZERO);
    }
}
