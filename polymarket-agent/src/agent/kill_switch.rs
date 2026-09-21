//! Stopping the agent from opening positions, without stopping the process.
//!
//! Killing the process is the wrong tool. A dead agent leaves resting orders
//! on the book with nobody polling them, open positions with nobody marking
//! them, and no reconciliation until somebody restarts it. What an operator
//! actually wants at 3am is "stop buying things" — exits, order polling and
//! reconciliation must all keep running, because those are how the existing
//! exposure gets smaller rather than larger.
//!
//! Three ways in, because the one that works is whichever one is reachable:
//!
//! * a `HALT` file in the data directory — works over SSH with no token, and
//!   survives a restart, which is the property the other two lack;
//! * `POST /api/halt` — works from the dashboard;
//! * `SIGUSR1` — works from a supervisor or a shell with a PID and nothing else.
//!
//! And one way in that isn't a person: the circuit breaker and the
//! reconciliation pass trip the same switch, so "why is it halted" has one
//! answer in one place regardless of who decided it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, Utc};

use crate::risk::circuit_breaker::HaltScope;

/// Who stopped the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltSource {
    /// A `HALT` file appeared in the data directory.
    HaltFile,
    /// `POST /api/halt`.
    Api,
    /// `SIGUSR1`.
    Signal,
    /// A risk limit was breached.
    CircuitBreaker,
    /// Local records and the venue disagree.
    Reconciliation,
}

impl HaltSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HaltFile => "halt_file",
            Self::Api => "api",
            Self::Signal => "signal",
            Self::CircuitBreaker => "circuit_breaker",
            Self::Reconciliation => "reconciliation",
        }
    }
}

/// What reconciling with the `HALT` file changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSync {
    /// The file appeared and the switch is now tripped.
    Raised(Halt),
    /// The file was removed and the switch is now clear.
    Resumed,
    Unchanged,
}

/// A halt in force.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Halt {
    #[serde(serialize_with = "serialize_source")]
    pub source: HaltSource,
    #[serde(serialize_with = "serialize_scope")]
    pub scope: HaltScope,
    pub detail: String,
    pub at: DateTime<Utc>,
    /// The UTC day the halt was raised, so a rest-of-day halt knows which day
    /// it belongs to. Derived from `at`, kept explicit because that is the
    /// field the expiry check compares and deriving it at each comparison
    /// invites a timezone slip.
    pub day: NaiveDate,
}

fn serialize_source<S: serde::Serializer>(v: &HaltSource, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(v.as_str())
}

fn serialize_scope<S: serde::Serializer>(v: &HaltScope, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(match v {
        HaltScope::RestOfDay => "rest_of_day",
        HaltScope::UntilResume => "until_resume",
    })
}

impl Halt {
    /// Rebuild a halt from the row that outlived the last process.
    ///
    /// Returns `None` for a row this build cannot interpret — an unknown
    /// source or an unparseable timestamp. Refusing to guess matters here:
    /// inventing a scope would either resume a halt that should have held or
    /// hold one that should have expired, and the caller can say so out loud
    /// instead.
    pub fn from_stored(stored: &crate::db::store::StoredHalt) -> Option<Self> {
        let source = match stored.source.as_str() {
            "halt_file" => HaltSource::HaltFile,
            "api" => HaltSource::Api,
            "signal" => HaltSource::Signal,
            "circuit_breaker" => HaltSource::CircuitBreaker,
            "reconciliation" => HaltSource::Reconciliation,
            _ => return None,
        };
        let scope = match stored.scope.as_str() {
            "rest_of_day" => HaltScope::RestOfDay,
            "until_resume" => HaltScope::UntilResume,
            _ => return None,
        };
        let at = DateTime::parse_from_rfc3339(&stored.raised_at)
            .ok()?
            .with_timezone(&Utc);
        let day = stored.day.parse::<NaiveDate>().ok()?;
        Some(Self {
            source,
            scope,
            detail: stored.detail.clone().unwrap_or_default(),
            at,
            day,
        })
    }

    pub fn new(
        source: HaltSource,
        scope: HaltScope,
        detail: impl Into<String>,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            source,
            scope,
            detail: detail.into(),
            at,
            day: at.date_naive(),
        }
    }
}

/// The halt flag, shared between the agent loop, the dashboard and the signal
/// handler.
#[derive(Debug)]
pub struct KillSwitch {
    /// Read on every entry decision, so it is an atomic rather than a lock.
    tripped: AtomicBool,
    current: Mutex<Option<Halt>>,
    halt_file: PathBuf,
    /// Bumped on every trip, so the main loop can cut its sleep short.
    ///
    /// Without this the agent reacts to a halt only at its next scheduled
    /// wake — up to `cycle_interval_seconds`, and up to `max_sleep_seconds`
    /// (an hour by default) when every venue is closed. For the one control
    /// whose promise is "stop now", and whose go-live criterion is that
    /// resting orders are cancelled within one cycle, an hour of latency is
    /// not a detail.
    ///
    /// A version counter rather than a `Notify`, for two reasons. A `Notify`
    /// that reports "is or becomes tripped" fires immediately on every loop
    /// iteration while a halt is in force, turning the idle sleep into a busy
    /// spin that runs cycles back to back. And one that reports only
    /// transitions has a race between the caller's check and its await.
    /// Watch semantics have neither problem: the loop marks the version it
    /// has acted on, and `changed()` fires only when a *newer* one arrives.
    trips: tokio::sync::watch::Sender<u64>,
    /// Whether the agent loop has run the side effects for the current halt.
    ///
    /// Tripping is cheap and happens from four places, two of which are not
    /// the agent — an HTTP handler and a signal handler, neither of which can
    /// cancel an order or write to SQLite without racing a cycle. So they set
    /// the flag and the loop does the work, which means the loop needs to
    /// know whether that work is still outstanding. Without this, a halt from
    /// `/api/halt` or SIGUSR1 stopped entries but never cancelled the resting
    /// orders the dashboard promised it would.
    handled: AtomicBool,
}

impl KillSwitch {
    pub fn new(halt_file: PathBuf) -> Self {
        Self {
            tripped: AtomicBool::new(false),
            current: Mutex::new(None),
            halt_file,
            trips: tokio::sync::watch::Sender::new(0),
            handled: AtomicBool::new(false),
        }
    }

    /// The halt whose side effects are still outstanding, if any.
    ///
    /// Returns a given halt exactly once, so the caller can cancel resting
    /// orders, alert and persist without repeating any of it on the cycles
    /// that follow.
    /// Whether a halt is in force whose side effects have not run yet.
    ///
    /// Does not consume it — the loop uses this to decide not to go to sleep,
    /// and `take_unhandled` still has to hand the halt over exactly once.
    pub fn has_unhandled(&self) -> bool {
        self.lock().is_some() && !self.handled.load(Ordering::Acquire)
    }

    pub fn take_unhandled(&self) -> Option<Halt> {
        // The guard is held across the swap on purpose. Reading the halt and
        // then marking it handled as two steps leaves a window in which
        // `clear()` can empty `current` and reset `handled` — and the swap
        // then observes `false` and hands back a halt that is no longer in
        // force. The loop would cancel every resting order and write an audit
        // row for a halt the operator had just explicitly lifted.
        let current = self.lock();
        let halt = current.clone()?;
        if self.handled.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(halt)
    }

    /// A receiver that fires when a halt is raised.
    ///
    /// Call `borrow_and_update()` once the caller has acted on the current
    /// state; `changed()` then resolves only on a trip after that point.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.trips.subscribe()
    }

    pub fn halt_file(&self) -> &Path {
        &self.halt_file
    }

    /// The hot path: may the agent open a position?
    pub fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::Acquire)
    }

    pub fn current(&self) -> Option<Halt> {
        self.lock().clone()
    }

    /// Raise a halt.
    ///
    /// Returns `true` if this call is what tripped the switch, so the caller
    /// can do the one-time work — cancelling resting orders, sending the
    /// alert — without repeating it on every cycle that follows.
    ///
    /// An already-halted agent keeps its *first* reason. The first cause is
    /// the diagnostic one; overwriting it with whatever tripped last turns
    /// "why did this stop" into a question about ordering.
    pub fn trip(&self, halt: Halt) -> bool {
        let mut current = self.lock();
        if current.is_some() {
            return false;
        }
        *current = Some(halt);
        // Release-ordered, and written while the lock is held, so any thread
        // that observes `tripped` can also observe the reason.
        self.tripped.store(true, Ordering::Release);
        self.handled.store(false, Ordering::Release);
        drop(current);
        self.trips.send_modify(|v| *v += 1);
        true
    }

    /// Lift the halt. Returns what was cleared, if anything.
    ///
    /// Also removes the `HALT` file. Without that, resuming from the
    /// dashboard would appear to work and then be undone by the next poll —
    /// the operator would see the agent refuse to resume with no explanation.
    pub fn clear(&self) -> std::io::Result<Option<Halt>> {
        let mut current = self.lock();
        let was = current.take();
        self.tripped.store(false, Ordering::Release);
        self.handled.store(false, Ordering::Release);
        drop(current);

        match std::fs::remove_file(&self.halt_file) {
            Ok(()) => Ok(was),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(was),
            Err(e) => Err(e),
        }
    }

    /// Reconcile the switch with the `HALT` file on disk.
    ///
    /// Called on every wake. The file is authoritative in both directions:
    /// creating it halts, deleting it resumes. Deleting it does *not* lift a
    /// halt raised by anything else — a breaker trip is not cleared by
    /// removing a file that was never there.
    ///
    /// Returns what changed, so the caller can clear the persisted row on a
    /// resume as well as act on a new halt.
    pub fn sync_with_file(&self, now: DateTime<Utc>) -> FileSync {
        let exists = self.halt_file.exists();
        let current_source = self.lock().as_ref().map(|h| h.source);

        match (exists, current_source) {
            (true, None) => {
                let detail = std::fs::read_to_string(&self.halt_file)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("{} exists", self.halt_file.display()));
                let halt = Halt::new(HaltSource::HaltFile, HaltScope::UntilResume, detail, now);
                if self.trip(halt.clone()) {
                    return FileSync::Raised(halt);
                }
                FileSync::Unchanged
            }
            (false, Some(HaltSource::HaltFile)) => {
                // The operator removed the file. That is a resume — and the
                // caller must clear the persisted row, or the next restart
                // reinstates a halt that was lifted, cancelling orders and
                // firing a Critical alert for something long since resolved.
                let mut current = self.lock();
                *current = None;
                self.tripped.store(false, Ordering::Release);
                self.handled.store(false, Ordering::Release);
                FileSync::Resumed
            }
            _ => FileSync::Unchanged,
        }
    }

    /// Lift a rest-of-day halt once the UTC day has rolled over.
    ///
    /// Returns `true` if a halt was lifted. A halt whose file still exists is
    /// re-raised by the next `sync_with_file`, which is why this does not
    /// touch the file.
    pub fn expire_if_day_rolled(&self, today: NaiveDate) -> bool {
        let mut current = self.lock();
        let lift = matches!(
            current.as_ref(),
            Some(Halt {
                scope: HaltScope::RestOfDay,
                day,
                ..
            }) if *day != today
        );
        if lift {
            *current = None;
            self.tripped.store(false, Ordering::Release);
            self.handled.store(false, Ordering::Release);
        }
        lift
    }

    /// Poisoning means a panic happened while swapping an `Option<Halt>`.
    /// The flag is a separate atomic and is still correct, so recovering is
    /// strictly better than panicking the trading loop.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Halt>> {
        self.current.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: u32) -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(2026, 9, day)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
    }

    fn switch(dir: &tempfile::TempDir) -> KillSwitch {
        KillSwitch::new(dir.path().join("HALT"))
    }

    /// The signal exists so an idle loop does not sleep through a halt.
    #[tokio::test]
    async fn a_subscriber_is_woken_when_the_switch_trips() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        let mut rx = s.subscribe();
        rx.borrow_and_update();

        s.trip(Halt::new(
            HaltSource::Api,
            HaltScope::UntilResume,
            "x",
            at(21),
        ));

        // Already sent, so this resolves without needing a timeout.
        rx.changed().await.expect("the sender outlives this");
    }

    /// The bug this shape was chosen to avoid: a signal meaning "is tripped"
    /// rather than "has just tripped" fires on every loop iteration while a
    /// halt is in force, so the idle sleep never happens and the agent runs
    /// cycles back to back for as long as it is halted.
    #[tokio::test]
    async fn an_already_handled_halt_does_not_wake_the_loop_again() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        let mut rx = s.subscribe();

        s.trip(Halt::new(
            HaltSource::Api,
            HaltScope::UntilResume,
            "x",
            at(21),
        ));
        // The loop runs its cycle and marks what it has acted on.
        rx.changed().await.unwrap();
        rx.borrow_and_update();

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.changed())
                .await
                .is_err(),
            "a halt already acted on must not wake the loop again"
        );
    }

    #[tokio::test]
    async fn a_second_distinct_halt_after_a_resume_wakes_the_loop() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        let mut rx = s.subscribe();
        rx.borrow_and_update();

        s.trip(Halt::new(
            HaltSource::Api,
            HaltScope::UntilResume,
            "first",
            at(21),
        ));
        rx.changed().await.unwrap();
        rx.borrow_and_update();

        s.clear().unwrap();
        s.trip(Halt::new(
            HaltSource::CircuitBreaker,
            HaltScope::RestOfDay,
            "second",
            at(21),
        ));
        rx.changed()
            .await
            .expect("a genuinely new halt must wake the loop");
    }

    /// A trip that is not new — the switch was already halted — must not
    /// wake anyone: the loop has already acted on that halt.
    #[tokio::test]
    async fn a_redundant_trip_does_not_wake_the_loop() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        s.trip(Halt::new(
            HaltSource::Api,
            HaltScope::UntilResume,
            "first",
            at(21),
        ));
        let mut rx = s.subscribe();
        rx.borrow_and_update();

        assert!(
            !s.trip(Halt::new(
                HaltSource::Signal,
                HaltScope::UntilResume,
                "second",
                at(21)
            )),
            "already halted"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.changed())
                .await
                .is_err(),
            "a refused trip must not signal"
        );
    }

    /// The hole this closed: a halt from `/api/halt` or SIGUSR1 set the flag
    /// but never reached the code that cancels resting orders, sends the
    /// alert and writes the row that survives a restart — because that work
    /// keyed off the return value of `trip`, which only the agent's own
    /// callers ever saw. The dashboard meanwhile promised the cancellation.
    #[test]
    fn a_halt_raised_from_outside_the_loop_is_still_handed_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);

        // Exactly what the HTTP handler and the signal handler do.
        s.trip(Halt::new(
            HaltSource::Api,
            HaltScope::UntilResume,
            "operator",
            at(21),
        ));

        let pending = s
            .take_unhandled()
            .expect("the loop must be given work to do");
        assert_eq!(pending.source, HaltSource::Api);
    }

    #[test]
    fn a_halt_is_handed_over_exactly_once() {
        // Otherwise every cycle for the rest of the halt re-cancels orders
        // and re-sends the alert.
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        s.trip(Halt::new(
            HaltSource::Api,
            HaltScope::UntilResume,
            "x",
            at(21),
        ));

        assert!(s.take_unhandled().is_some());
        assert!(s.take_unhandled().is_none(), "second look must be empty");
        assert!(s.take_unhandled().is_none());
    }

    #[test]
    fn nothing_is_pending_when_nothing_is_halted() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        assert!(s.take_unhandled().is_none());
    }

    #[test]
    fn a_fresh_halt_after_a_resume_is_pending_again() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        s.trip(Halt::new(
            HaltSource::Api,
            HaltScope::UntilResume,
            "first",
            at(21),
        ));
        s.take_unhandled();
        s.clear().unwrap();

        s.trip(Halt::new(
            HaltSource::CircuitBreaker,
            HaltScope::RestOfDay,
            "second",
            at(21),
        ));
        let pending = s
            .take_unhandled()
            .expect("a new halt needs its own orders cancelled");
        assert_eq!(pending.source, HaltSource::CircuitBreaker);
    }

    #[test]
    fn a_halt_from_the_file_is_pending_too() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        std::fs::write(s.halt_file(), "stop").unwrap();
        s.sync_with_file(at(21));
        assert_eq!(
            s.take_unhandled().map(|h| h.source),
            Some(HaltSource::HaltFile)
        );
    }

    #[test]
    fn a_fresh_switch_is_not_tripped() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        assert!(!s.is_tripped());
        assert_eq!(s.current(), None);
    }

    #[test]
    fn tripping_reports_the_first_trip_and_only_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        let first = Halt::new(HaltSource::Api, HaltScope::UntilResume, "operator", at(21));
        assert!(s.trip(first.clone()), "the first trip is new");
        assert!(
            !s.trip(Halt::new(
                HaltSource::CircuitBreaker,
                HaltScope::RestOfDay,
                "drawdown",
                at(21)
            )),
            "a second trip while already halted is not new — the one-time \
             work of cancelling orders must not run twice"
        );
        assert_eq!(
            s.current().unwrap().source,
            HaltSource::Api,
            "the first reason is kept, not the latest"
        );
        assert!(s.is_tripped());
    }

    #[test]
    fn clearing_lifts_the_halt() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        s.trip(Halt::new(
            HaltSource::Api,
            HaltScope::UntilResume,
            "x",
            at(21),
        ));
        let cleared = s.clear().unwrap();
        assert_eq!(cleared.unwrap().source, HaltSource::Api);
        assert!(!s.is_tripped());
        assert_eq!(s.current(), None);
    }

    #[test]
    fn clearing_removes_the_halt_file_so_the_resume_sticks() {
        // Without the removal, resuming via the API is undone by the next
        // poll and the operator gets no explanation.
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        std::fs::write(s.halt_file(), "stop").unwrap();
        s.sync_with_file(at(21));
        assert!(s.is_tripped());

        s.clear().unwrap();
        assert!(!s.halt_file().exists(), "the file must be gone");
        assert_eq!(
            s.sync_with_file(at(21)),
            FileSync::Unchanged,
            "and the next poll must not re-raise it"
        );
        assert!(!s.is_tripped());
    }

    #[test]
    fn clearing_when_there_is_no_halt_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        s.trip(Halt::new(
            HaltSource::CircuitBreaker,
            HaltScope::RestOfDay,
            "x",
            at(21),
        ));
        assert!(s.clear().is_ok());
    }

    #[test]
    fn the_halt_file_trips_the_switch_and_carries_its_contents_as_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        std::fs::write(s.halt_file(), "  broker margin call  \n").unwrap();
        let FileSync::Raised(raised) = s.sync_with_file(at(21)) else {
            panic!("the file must trip it")
        };
        assert_eq!(raised.source, HaltSource::HaltFile);
        assert_eq!(raised.detail, "broker margin call");
        assert!(s.is_tripped());
    }

    #[test]
    fn an_empty_halt_file_still_trips_with_a_usable_reason() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        std::fs::write(s.halt_file(), "").unwrap();
        let FileSync::Raised(raised) = s.sync_with_file(at(21)) else {
            panic!("an empty file still halts")
        };
        assert!(
            raised.detail.contains("HALT"),
            "the reason should name the file, got {:?}",
            raised.detail
        );
    }

    #[test]
    fn syncing_twice_only_reports_the_trip_once() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        std::fs::write(s.halt_file(), "stop").unwrap();
        assert!(matches!(s.sync_with_file(at(21)), FileSync::Raised(_)));
        assert_eq!(
            s.sync_with_file(at(21)),
            FileSync::Unchanged,
            "every wake polls the file; only the first must cancel orders"
        );
    }

    #[test]
    fn removing_the_halt_file_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        std::fs::write(s.halt_file(), "stop").unwrap();
        s.sync_with_file(at(21));
        std::fs::remove_file(s.halt_file()).unwrap();
        s.sync_with_file(at(21));
        assert!(!s.is_tripped(), "deleting the file is a resume");
    }

    #[test]
    fn removing_the_halt_file_does_not_clear_a_breaker_halt() {
        // The file was never the reason. Deleting something that was not
        // there must not resume trading after a risk limit was breached.
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        s.trip(Halt::new(
            HaltSource::CircuitBreaker,
            HaltScope::UntilResume,
            "drawdown 20%",
            at(21),
        ));
        s.sync_with_file(at(21));
        assert!(s.is_tripped());
        assert_eq!(s.current().unwrap().source, HaltSource::CircuitBreaker);
    }

    #[test]
    fn a_rest_of_day_halt_lifts_when_the_day_rolls_over() {
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        s.trip(Halt::new(
            HaltSource::CircuitBreaker,
            HaltScope::RestOfDay,
            "daily loss",
            at(21),
        ));
        assert!(
            !s.expire_if_day_rolled(at(21).date_naive()),
            "same day: held"
        );
        assert!(s.is_tripped());
        assert!(
            s.expire_if_day_rolled(at(22).date_naive()),
            "next day: lifted"
        );
        assert!(!s.is_tripped());
    }

    #[test]
    fn an_until_resume_halt_survives_the_day_rolling_over() {
        // The whole distinction between the two scopes. If this passes with
        // the scope check removed, the drawdown breaker is decorative.
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        s.trip(Halt::new(
            HaltSource::CircuitBreaker,
            HaltScope::UntilResume,
            "drawdown",
            at(21),
        ));
        assert!(!s.expire_if_day_rolled(at(22).date_naive()));
        assert!(
            s.is_tripped(),
            "a drawdown halt must not clear itself by waiting"
        );
    }

    #[test]
    fn a_halt_file_halt_survives_the_day_rolling_over() {
        // It is raised UntilResume precisely so that sleeping on it does
        // nothing: the operator put the file there and only they take it away.
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        std::fs::write(s.halt_file(), "stop").unwrap();
        s.sync_with_file(at(21));
        assert!(!s.expire_if_day_rolled(at(22).date_naive()));
        assert!(s.is_tripped());
    }

    #[test]
    fn expiring_a_day_scoped_halt_leaves_the_file_halt_to_be_re_raised() {
        // Belt and braces: if both a file and a day-scoped halt somehow
        // coexist, the file must win the next poll.
        let dir = tempfile::tempdir().unwrap();
        let s = switch(&dir);
        s.trip(Halt::new(
            HaltSource::CircuitBreaker,
            HaltScope::RestOfDay,
            "daily loss",
            at(21),
        ));
        std::fs::write(s.halt_file(), "and also this").unwrap();
        assert!(s.expire_if_day_rolled(at(22).date_naive()));
        let FileSync::Raised(raised) = s.sync_with_file(at(22)) else {
            panic!("the file re-raises")
        };
        assert_eq!(raised.source, HaltSource::HaltFile);
        assert!(s.is_tripped());
    }
}
