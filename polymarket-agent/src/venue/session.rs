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
            // A wrapping window belongs to the day it *started* on, so an
            // instant at 01:00 is inside the previous day's overnight session.
            let (session_date, inside) = if window.wraps_midnight() {
                if time >= window.start {
                    (today, true)
                } else if time < window.end {
                    (today - Duration::days(1), true)
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
                let naive = date.and_time(window.start);
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
        // 01:00 Friday belongs to Thursday's overnight session, which is a
        // trading day, so it is open.
        assert_eq!(
            s.state_at(et(2026, 9, 18, 1, 0)).kind(),
            Some(SessionKind::Overnight)
        );
        // 01:00 Sunday belongs to Saturday's overnight — not a trading day.
        assert!(!s.is_open_at(et(2026, 9, 20, 1, 0)));
        // 01:00 Monday belongs to Sunday's overnight — also not a trading day.
        assert!(!s.is_open_at(et(2026, 9, 21, 1, 0)));
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
