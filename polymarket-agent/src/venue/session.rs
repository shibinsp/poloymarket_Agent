//! Trading sessions.
//!
//! Polymarket and crypto never close; equities do, and Alpaca's extended and
//! overnight windows accept limit orders only. The scheduler uses
//! `state_at` both to decide whether to scan a venue and to sleep until the
//! next open rather than waking every cycle to do nothing.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

/// Which kind of window is currently open. Venues restrict order types outside
/// `Regular`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Regular,
    /// Pre-market / after-hours.
    Extended,
    /// Overnight (Alpaca's 8pm–4am ET).
    Overnight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Open(SessionKind),
    Closed {
        /// When trading resumes, so the caller can sleep until then.
        next_open: DateTime<Utc>,
    },
}

impl SessionState {
    pub fn is_open(&self) -> bool {
        matches!(self, SessionState::Open(_))
    }

    pub fn kind(&self) -> Option<SessionKind> {
        match self {
            SessionState::Open(k) => Some(*k),
            SessionState::Closed { .. } => None,
        }
    }
}

/// One daily window in the venue's local time. `end` before `start` means the
/// window wraps past midnight (an overnight session).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionWindow {
    pub start: NaiveTime,
    pub end: NaiveTime,
    pub kind: SessionKind,
}

impl SessionWindow {
    pub fn new(start: (u32, u32), end: (u32, u32), kind: SessionKind) -> Self {
        Self {
            start: NaiveTime::from_hms_opt(start.0, start.1, 0).expect("valid time"),
            end: NaiveTime::from_hms_opt(end.0, end.1, 0).expect("valid time"),
            kind,
        }
    }

    fn wraps_midnight(&self) -> bool {
        self.end <= self.start
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TradingSession {
    /// Never closes — crypto spot, prediction markets.
    Always,
    Windows {
        tz: Tz,
        /// Days the venue trades at all.
        weekdays: Vec<Weekday>,
        windows: Vec<SessionWindow>,
        /// Full-day closures (holidays), in venue-local dates.
        holidays: Vec<NaiveDate>,
    },
}

impl TradingSession {
    /// US equities including Alpaca's extended and overnight sessions:
    /// overnight 20:00–04:00, pre-market 04:00–09:30, regular 09:30–16:00,
    /// after-hours 16:00–20:00 ET.
    pub fn us_equity_extended() -> Self {
        TradingSession::Windows {
            tz: chrono_tz::America::New_York,
            weekdays: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
            windows: vec![
                SessionWindow::new((4, 0), (9, 30), SessionKind::Extended),
                SessionWindow::new((9, 30), (16, 0), SessionKind::Regular),
                SessionWindow::new((16, 0), (20, 0), SessionKind::Extended),
                // Wraps past midnight into the next calendar day.
                SessionWindow::new((20, 0), (4, 0), SessionKind::Overnight),
            ],
            holidays: Vec::new(),
        }
    }

    /// US equities, regular hours only — the conservative default for live
    /// trading, since extended sessions are thinner and limit-only.
    pub fn us_equity_regular() -> Self {
        TradingSession::Windows {
            tz: chrono_tz::America::New_York,
            weekdays: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
            windows: vec![SessionWindow::new((9, 30), (16, 0), SessionKind::Regular)],
            holidays: Vec::new(),
        }
    }

    /// NSE/BSE: 09:15–15:30 IST, Mon–Fri.
    pub fn nse() -> Self {
        TradingSession::Windows {
            tz: chrono_tz::Asia::Kolkata,
            weekdays: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
            windows: vec![SessionWindow::new((9, 15), (15, 30), SessionKind::Regular)],
            holidays: Vec::new(),
        }
    }

    pub fn with_holidays(mut self, dates: Vec<NaiveDate>) -> Self {
        if let TradingSession::Windows { holidays, .. } = &mut self {
            *holidays = dates;
        }
        self
    }

    pub fn is_open_at(&self, at: DateTime<Utc>) -> bool {
        self.state_at(at).is_open()
    }

    /// Resolve the session state at an instant, and when it next opens if shut.
    pub fn state_at(&self, at: DateTime<Utc>) -> SessionState {
        let TradingSession::Windows {
            tz,
            weekdays,
            windows,
            holidays,
        } = self
        else {
            return SessionState::Open(SessionKind::Regular);
        };

        let local = at.with_timezone(tz);
        let today = local.date_naive();
        let time = local.time();

        for window in windows {
            // A wrapping window belongs to the trading day it *ends* on, not
            // the one it starts on. Alpaca's overnight session runs Sunday
            // 20:00 ET into Monday and Thursday 20:00 into Friday; there is no
            // Friday-night session, because Saturday is not a trading day.
            // Attaching it to the start date inverts exactly that: it opens
            // Friday night into a shut venue and sits out Sunday night.
            let (session_date, inside) = if window.wraps_midnight() {
                if time >= window.start {
                    (today + Duration::days(1), true)
                } else if time < window.end {
                    (today, true)
                } else {
                    (today, false)
                }
            } else {
                (today, time >= window.start && time < window.end)
            };

            if inside && self.trades_on(session_date, weekdays, holidays) {
                return SessionState::Open(window.kind);
            }
        }

        SessionState::Closed {
            next_open: self.next_open_after(at),
        }
    }

    fn trades_on(&self, date: NaiveDate, weekdays: &[Weekday], holidays: &[NaiveDate]) -> bool {
        weekdays.contains(&date.weekday()) && !holidays.contains(&date)
    }

    /// Next instant the venue is open. Scans forward day by day; a venue that
    /// never opens within two weeks is treated as indefinitely closed.
    fn next_open_after(&self, at: DateTime<Utc>) -> DateTime<Utc> {
        let TradingSession::Windows {
            tz,
            weekdays,
            windows,
            holidays,
        } = self
        else {
            return at;
        };

        let local = at.with_timezone(tz);
        let mut candidates: Vec<DateTime<Utc>> = Vec::new();

        for day_offset in 0..14 {
            let date = local.date_naive() + Duration::days(day_offset);
            if !self.trades_on(date, weekdays, holidays) {
                continue;
            }
            for window in windows {
                // `date` is the trading day. A wrapping window that belongs to
                // it began the evening before, so the same shift applied in
                // `state_at` has to be applied here or the two disagree about
                // when the session starts.
                let start_date = if window.wraps_midnight() {
                    date - Duration::days(1)
                } else {
                    date
                };
                let naive = start_date.and_time(window.start);
                // Ambiguous or skipped local times (DST transitions) are
                // simply not offered as candidates.
                if let Some(start) = tz.from_local_datetime(&naive).earliest() {
                    let start_utc = start.with_timezone(&Utc);
                    if start_utc > at {
                        candidates.push(start_utc);
                    }
                }
            }
        }

        candidates
            .into_iter()
            .min()
            .unwrap_or(at + Duration::days(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn et(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        chrono_tz::America::New_York
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn always_open_session_never_closes() {
        let s = TradingSession::Always;
        // A Sunday at 3am is still open for crypto and prediction markets.
        assert!(s.is_open_at(et(2026, 9, 20, 3, 0)));
        assert_eq!(s.state_at(Utc::now()).kind(), Some(SessionKind::Regular));
    }

    #[test]
    fn regular_hours_boundaries_are_half_open() {
        let s = TradingSession::us_equity_regular();
        // 2026-09-17 is a Thursday.
        assert!(!s.is_open_at(et(2026, 9, 17, 9, 29)));
        assert!(s.is_open_at(et(2026, 9, 17, 9, 30)));
        assert!(s.is_open_at(et(2026, 9, 17, 15, 59)));
        // Close is exclusive — 16:00 is shut.
        assert!(!s.is_open_at(et(2026, 9, 17, 16, 0)));
    }

    #[test]
    fn equities_are_closed_at_the_weekend_but_crypto_is_not() {
        let equity = TradingSession::us_equity_extended();
        // Saturday.
        assert!(!equity.is_open_at(et(2026, 9, 19, 12, 0)));
        assert!(TradingSession::Always.is_open_at(et(2026, 9, 19, 12, 0)));
    }

    #[test]
    fn extended_and_overnight_windows_are_distinguished() {
        let s = TradingSession::us_equity_extended();
        // Thursday pre-market.
        assert_eq!(
            s.state_at(et(2026, 9, 17, 5, 0)).kind(),
            Some(SessionKind::Extended)
        );
        // Thursday regular.
        assert_eq!(
            s.state_at(et(2026, 9, 17, 11, 0)).kind(),
            Some(SessionKind::Regular)
        );
        // Thursday after-hours.
        assert_eq!(
            s.state_at(et(2026, 9, 17, 17, 0)).kind(),
            Some(SessionKind::Extended)
        );
        // Thursday late evening is the overnight session.
        assert_eq!(
            s.state_at(et(2026, 9, 17, 22, 0)).kind(),
            Some(SessionKind::Overnight)
        );
    }

    #[test]
    fn overnight_window_wraps_past_midnight() {
        let s = TradingSession::us_equity_extended();
        // Mid-week the overnight session spans both sides of midnight:
        // Thursday 21:00 and the Friday 01:00 that continues it are one
        // session, and Friday is a trading day.
        assert_eq!(
            s.state_at(et(2026, 9, 17, 21, 0)).kind(),
            Some(SessionKind::Overnight)
        );
        assert_eq!(
            s.state_at(et(2026, 9, 18, 1, 0)).kind(),
            Some(SessionKind::Overnight)
        );
    }

    /// An overnight session belongs to the trading day it *ends* on, which is
    /// what decides both ends of the week. Alpaca runs Sunday 20:00 ET into
    /// Monday and stops at Friday 20:00; attaching the window to the day it
    /// starts on inverts exactly that — opening Friday night into a venue that
    /// is shut and sitting out the Sunday night session that does run.
    ///
    /// Both ends are asserted here on purpose: the earlier test checked only
    /// the Sunday/Monday side, so it passed while the Friday/Saturday side was
    /// wrong.
    #[test]
    fn the_trading_week_opens_sunday_night_and_closes_friday_evening() {
        let s = TradingSession::us_equity_extended();

        // Friday 20:00 ET ends the week — there is no Friday-night session,
        // because it would settle into a Saturday.
        assert!(!s.is_open_at(et(2026, 9, 18, 21, 0)), "Friday 21:00");
        assert!(!s.is_open_at(et(2026, 9, 19, 1, 0)), "Saturday 01:00");
        assert!(!s.is_open_at(et(2026, 9, 19, 12, 0)), "Saturday midday");
        assert!(!s.is_open_at(et(2026, 9, 20, 12, 0)), "Sunday midday");

        // Sunday 20:00 ET opens the week, and it runs through into Monday.
        assert_eq!(
            s.state_at(et(2026, 9, 20, 20, 0)).kind(),
            Some(SessionKind::Overnight),
            "Sunday 20:00 is the weekly open"
        );
        assert_eq!(
            s.state_at(et(2026, 9, 21, 1, 0)).kind(),
            Some(SessionKind::Overnight),
            "Monday 01:00 continues Sunday's session"
        );
    }

    /// `state_at` and `next_open_after` have to agree about when a wrapping
    /// session starts, or the scheduler sleeps to an instant that still
    /// reports closed and spins.
    #[test]
    fn next_open_from_the_weekend_lands_on_the_sunday_night_open() {
        let s = TradingSession::us_equity_extended();
        for from in [
            et(2026, 9, 18, 21, 0), // Friday night, just after the close
            et(2026, 9, 19, 12, 0), // Saturday midday
            et(2026, 9, 20, 12, 0), // Sunday midday
        ] {
            let SessionState::Closed { next_open } = s.state_at(from) else {
                panic!("expected closed at {from}");
            };
            assert_eq!(next_open, et(2026, 9, 20, 20, 0), "from {from}");
            assert!(
                s.is_open_at(next_open),
                "woke to a closed venue from {from}"
            );
        }
    }

    /// A holiday removes the session that settles into it, including the
    /// overnight leg that starts the evening before.
    #[test]
    fn a_holiday_also_cancels_the_overnight_session_that_runs_into_it() {
        let monday = NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        let s = TradingSession::us_equity_extended().with_holidays(vec![monday]);

        assert!(!s.is_open_at(et(2026, 9, 20, 21, 0)), "Sunday night leg");
        assert!(!s.is_open_at(et(2026, 9, 21, 1, 0)), "Monday small hours");
        assert!(
            !s.is_open_at(et(2026, 9, 21, 10, 0)),
            "Monday regular hours"
        );
        // Tuesday's session is untouched, and its overnight leg starts Monday
        // evening even though Monday itself is a holiday.
        assert!(s.is_open_at(et(2026, 9, 21, 21, 0)), "Monday night leg");
        assert!(
            s.is_open_at(et(2026, 9, 22, 10, 0)),
            "Tuesday regular hours"
        );
    }

    #[test]
    fn closed_state_reports_a_future_next_open() {
        let s = TradingSession::us_equity_regular();
        let saturday = et(2026, 9, 19, 12, 0);
        let SessionState::Closed { next_open } = s.state_at(saturday) else {
            panic!("expected closed on a Saturday");
        };
        assert!(next_open > saturday);
        // Next open is Monday's 09:30 ET.
        assert!(s.is_open_at(next_open));
        assert_eq!(next_open, et(2026, 9, 21, 9, 30));
    }

    #[test]
    fn holidays_close_the_venue() {
        let holiday = NaiveDate::from_ymd_opt(2026, 9, 17).unwrap();
        let s = TradingSession::us_equity_regular().with_holidays(vec![holiday]);
        // Thursday 11am would normally be open.
        assert!(!s.is_open_at(et(2026, 9, 17, 11, 0)));
        // The next day still trades.
        assert!(s.is_open_at(et(2026, 9, 18, 11, 0)));
    }

    #[test]
    fn session_follows_local_time_across_dst() {
        let s = TradingSession::us_equity_regular();
        // US DST ends 2026-11-01. 09:30 ET is open on both sides of the
        // change even though the UTC offset differs.
        assert!(s.is_open_at(et(2026, 10, 30, 9, 30)));
        assert!(s.is_open_at(et(2026, 11, 2, 9, 30)));
        // The same UTC instant that is 09:30 EDT is 08:30 EST after the
        // switch, i.e. closed.
        let pre_dst_utc = et(2026, 10, 30, 9, 30);
        let same_clock_utc_in_november = pre_dst_utc + Duration::days(3);
        assert!(!s.is_open_at(same_clock_utc_in_november));
    }

    #[test]
    fn nse_session_is_ist() {
        let s = TradingSession::nse();
        let ist = chrono_tz::Asia::Kolkata;
        let open = ist
            .with_ymd_and_hms(2026, 9, 17, 10, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let shut = ist
            .with_ymd_and_hms(2026, 9, 17, 16, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert!(s.is_open_at(open));
        assert!(!s.is_open_at(shut));
    }
}
