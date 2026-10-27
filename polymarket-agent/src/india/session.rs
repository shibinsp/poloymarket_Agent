//! NSE Indian equity exchange session detection.
//!
//! Accurately detects:
//! - Pre-Open Session: 09:00 - 09:15 IST (Order entry & price discovery)
//! - Regular Continuous Market: 09:15 - 15:30 IST (Active trading)
//! - Closing & Post-Market Session: 15:30 - 16:00 IST (Closing auction & post-close)
//! - Closed: Outside 09:00 - 16:00 IST, weekends, and exchange holidays.

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc, Weekday};
use chrono_tz::Asia::Kolkata;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NseSessionKind {
    PreOpen,
    Regular,
    ClosingAuction,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NseSessionInfo {
    pub session_kind: NseSessionKind,
    pub session_name: String,
    pub is_trading_active: bool,
    pub allows_order_entry: bool,
    pub local_time_ist: String,
    pub status_badge: String,
    pub next_session_event: String,
    pub current_date_ist: String,
}

/// Standard verified list of official NSE equity trading holidays for 2024-2026.
pub fn nse_holidays() -> Vec<NaiveDate> {
    vec![
        // 2024
        NaiveDate::from_ymd_opt(2024, 1, 22).unwrap(), // Special holiday
        NaiveDate::from_ymd_opt(2024, 1, 26).unwrap(), // Republic Day
        NaiveDate::from_ymd_opt(2024, 3, 8).unwrap(),  // Mahashivratri
        NaiveDate::from_ymd_opt(2024, 3, 25).unwrap(), // Holi
        NaiveDate::from_ymd_opt(2024, 3, 29).unwrap(), // Good Friday
        NaiveDate::from_ymd_opt(2024, 4, 11).unwrap(), // Id-Ul-Fitr
        NaiveDate::from_ymd_opt(2024, 4, 17).unwrap(), // Shri Ram Navami
        NaiveDate::from_ymd_opt(2024, 5, 1).unwrap(),  // Maharashtra Day
        NaiveDate::from_ymd_opt(2024, 6, 17).unwrap(), // Bakri Id
        NaiveDate::from_ymd_opt(2024, 7, 17).unwrap(), // Muharram
        NaiveDate::from_ymd_opt(2024, 8, 15).unwrap(), // Independence Day
        NaiveDate::from_ymd_opt(2024, 10, 2).unwrap(), // Mahatma Gandhi Jayanti
        NaiveDate::from_ymd_opt(2024, 11, 1).unwrap(), // Diwali Laxmi Pujan
        NaiveDate::from_ymd_opt(2024, 11, 15).unwrap(), // Gurunanak Jayanti
        NaiveDate::from_ymd_opt(2024, 12, 25).unwrap(), // Christmas
        // 2025
        NaiveDate::from_ymd_opt(2025, 1, 26).unwrap(),
        NaiveDate::from_ymd_opt(2025, 2, 26).unwrap(),
        NaiveDate::from_ymd_opt(2025, 3, 14).unwrap(),
        NaiveDate::from_ymd_opt(2025, 3, 31).unwrap(),
        NaiveDate::from_ymd_opt(2025, 4, 10).unwrap(),
        NaiveDate::from_ymd_opt(2025, 4, 14).unwrap(),
        NaiveDate::from_ymd_opt(2025, 4, 18).unwrap(),
        NaiveDate::from_ymd_opt(2025, 5, 1).unwrap(),
        NaiveDate::from_ymd_opt(2025, 8, 15).unwrap(),
        NaiveDate::from_ymd_opt(2025, 8, 27).unwrap(),
        NaiveDate::from_ymd_opt(2025, 10, 2).unwrap(),
        NaiveDate::from_ymd_opt(2025, 10, 21).unwrap(),
        NaiveDate::from_ymd_opt(2025, 10, 22).unwrap(),
        NaiveDate::from_ymd_opt(2025, 11, 5).unwrap(),
        NaiveDate::from_ymd_opt(2025, 12, 25).unwrap(),
        // 2026
        NaiveDate::from_ymd_opt(2026, 1, 26).unwrap(),
        NaiveDate::from_ymd_opt(2026, 3, 3).unwrap(),
        NaiveDate::from_ymd_opt(2026, 3, 20).unwrap(),
        NaiveDate::from_ymd_opt(2026, 4, 3).unwrap(),
        NaiveDate::from_ymd_opt(2026, 4, 14).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
        NaiveDate::from_ymd_opt(2026, 8, 15).unwrap(),
        NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
        NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
        NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
        NaiveDate::from_ymd_opt(2026, 12, 25).unwrap(),
    ]
}

/// Detect exact NSE market session at a given UTC timestamp.
pub fn detect_nse_session(at: DateTime<Utc>) -> NseSessionInfo {
    let ist_now = at.with_timezone(&Kolkata);
    let today = ist_now.date_naive();
    let time = ist_now.time();
    let weekday = ist_now.weekday();

    let local_time_ist = ist_now.format("%H:%M:%S IST").to_string();
    let current_date_ist = ist_now.format("%d %b %Y").to_string();

    // Check Weekend
    if weekday == Weekday::Sat || weekday == Weekday::Sun {
        let next_mon = if weekday == Weekday::Sat { "Monday 09:00 IST" } else { "Tomorrow 09:00 IST" };
        return NseSessionInfo {
            session_kind: NseSessionKind::Closed,
            session_name: "Market Closed (Weekend)".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_ist,
            status_badge: "CLOSED".to_string(),
            next_session_event: format!("Opens {next_mon}"),
            current_date_ist,
        };
    }

    // Check Holiday
    if nse_holidays().contains(&today) {
        return NseSessionInfo {
            session_kind: NseSessionKind::Closed,
            session_name: "Market Closed (Exchange Holiday)".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_ist,
            status_badge: "HOLIDAY".to_string(),
            next_session_event: "Opens next trading day at 09:00 IST".to_string(),
            current_date_ist,
        };
    }

    let pre_open_start = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
    let regular_start = NaiveTime::from_hms_opt(9, 15, 0).unwrap();
    let regular_end = NaiveTime::from_hms_opt(15, 30, 0).unwrap();
    let closing_end = NaiveTime::from_hms_opt(16, 0, 0).unwrap();

    if time < pre_open_start {
        // Morning before pre-open
        NseSessionInfo {
            session_kind: NseSessionKind::Closed,
            session_name: "Market Closed (Pre-Market)".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_ist,
            status_badge: "CLOSED".to_string(),
            next_session_event: "Pre-Open starts at 09:00 IST".to_string(),
            current_date_ist,
        }
    } else if time >= pre_open_start && time < regular_start {
        // Pre-Open Session
        NseSessionInfo {
            session_kind: NseSessionKind::PreOpen,
            session_name: "Pre-Open Session · Price Discovery".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_ist,
            status_badge: "PRE-OPEN".to_string(),
            next_session_event: "Regular trading opens at 09:15 IST".to_string(),
            current_date_ist,
        }
    } else if time >= regular_start && time < regular_end {
        // Regular continuous market
        NseSessionInfo {
            session_kind: NseSessionKind::Regular,
            session_name: "Regular Continuous Trading".to_string(),
            is_trading_active: true,
            allows_order_entry: true,
            local_time_ist,
            status_badge: "REGULAR LIVE".to_string(),
            next_session_event: "Market closes at 15:30 IST".to_string(),
            current_date_ist,
        }
    } else if time >= regular_end && time < closing_end {
        // Closing & Auction / Post-close session
        NseSessionInfo {
            session_kind: NseSessionKind::ClosingAuction,
            session_name: "Closing Auction & Post-Market Session".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_ist,
            status_badge: "CLOSING / AUCTION".to_string(),
            next_session_event: "Session ends at 16:00 IST".to_string(),
            current_date_ist,
        }
    } else {
        // Evening / Closed after 16:00 IST
        let next_day = if weekday == Weekday::Fri {
            "Opens Monday 09:00 IST"
        } else {
            "Opens Tomorrow 09:00 IST"
        };
        NseSessionInfo {
            session_kind: NseSessionKind::Closed,
            session_name: "Market Closed (Session Complete)".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_ist,
            status_badge: "CLOSED".to_string(),
            next_session_event: next_day.to_string(),
            current_date_ist,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_regular_hours_detected() {
        // 2026-09-17 10:30 IST
        let at = Kolkata.with_ymd_and_hms(2026, 9, 17, 10, 30, 0).unwrap().with_timezone(&Utc);
        let s = detect_nse_session(at);
        assert_eq!(s.session_kind, NseSessionKind::Regular);
        assert!(s.is_trading_active);
        assert!(s.allows_order_entry);
    }

    #[test]
    fn test_pre_open_detected() {
        // 2026-09-17 09:05 IST
        let at = Kolkata.with_ymd_and_hms(2026, 9, 17, 9, 5, 0).unwrap().with_timezone(&Utc);
        let s = detect_nse_session(at);
        assert_eq!(s.session_kind, NseSessionKind::PreOpen);
        assert!(!s.is_trading_active);
    }

    #[test]
    fn test_closed_detected_in_evening() {
        // 2026-09-17 18:30 IST
        let at = Kolkata.with_ymd_and_hms(2026, 9, 17, 18, 30, 0).unwrap().with_timezone(&Utc);
        let s = detect_nse_session(at);
        assert_eq!(s.session_kind, NseSessionKind::Closed);
        assert!(!s.is_trading_active);
        assert!(!s.allows_order_entry);
    }

    #[test]
    fn test_weekend_detected() {
        // 2026-09-19 Saturday
        let at = Kolkata.with_ymd_and_hms(2026, 9, 19, 11, 0, 0).unwrap().with_timezone(&Utc);
        let s = detect_nse_session(at);
        assert_eq!(s.session_kind, NseSessionKind::Closed);
    }
}
