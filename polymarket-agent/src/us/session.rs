//! US equity exchange session detection (NYSE / NASDAQ).
//!
//! Handles:
//! - Pre-Market: 04:00 - 09:30 ET
//! - Regular Trading Hours (RTH): 09:30 - 16:00 ET (Strategy execution & fills active)
//! - After-Hours: 16:00 - 20:00 ET
//! - Closed: Outside 04:00 - 20:00 ET, weekends, and official US market holidays.
//!
//! Automatically handles Eastern Time Daylight Saving Time (EDT/EST) via `chrono-tz::America::New_York`.

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc, Weekday};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UsSessionKind {
    PreMarket,
    Regular,
    AfterHours,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsSessionInfo {
    pub session_kind: UsSessionKind,
    pub session_name: String,
    pub is_trading_active: bool,
    pub allows_order_entry: bool,
    pub local_time_et: String,
    pub status_badge: String,
    pub next_session_event: String,
    pub current_date_et: String,
    pub is_market_closed: bool,
    pub strategy_paused_reason: Option<String>,
}

/// Verified list of official NYSE/NASDAQ equity market holidays for 2024-2026.
pub fn us_market_holidays() -> Vec<NaiveDate> {
    vec![
        // 2024
        NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),   // New Year's Day
        NaiveDate::from_ymd_opt(2024, 1, 15).unwrap(),  // Martin Luther King Jr. Day
        NaiveDate::from_ymd_opt(2024, 2, 19).unwrap(),  // Washington's Birthday / Presidents' Day
        NaiveDate::from_ymd_opt(2024, 3, 29).unwrap(),  // Good Friday
        NaiveDate::from_ymd_opt(2024, 5, 27).unwrap(),  // Memorial Day
        NaiveDate::from_ymd_opt(2024, 6, 19).unwrap(),  // Juneteenth National Independence Day
        NaiveDate::from_ymd_opt(2024, 7, 4).unwrap(),   // Independence Day
        NaiveDate::from_ymd_opt(2024, 9, 2).unwrap(),   // Labor Day
        NaiveDate::from_ymd_opt(2024, 11, 28).unwrap(), // Thanksgiving Day
        NaiveDate::from_ymd_opt(2024, 12, 25).unwrap(), // Christmas Day
        // 2025
        NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
        NaiveDate::from_ymd_opt(2025, 1, 20).unwrap(),
        NaiveDate::from_ymd_opt(2025, 2, 17).unwrap(),
        NaiveDate::from_ymd_opt(2025, 4, 18).unwrap(),
        NaiveDate::from_ymd_opt(2025, 5, 26).unwrap(),
        NaiveDate::from_ymd_opt(2025, 6, 19).unwrap(),
        NaiveDate::from_ymd_opt(2025, 7, 4).unwrap(),
        NaiveDate::from_ymd_opt(2025, 9, 1).unwrap(),
        NaiveDate::from_ymd_opt(2025, 11, 27).unwrap(),
        NaiveDate::from_ymd_opt(2025, 12, 25).unwrap(),
        // 2026
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        NaiveDate::from_ymd_opt(2026, 1, 19).unwrap(),
        NaiveDate::from_ymd_opt(2026, 2, 16).unwrap(),
        NaiveDate::from_ymd_opt(2026, 4, 3).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 25).unwrap(),
        NaiveDate::from_ymd_opt(2026, 6, 19).unwrap(),
        NaiveDate::from_ymd_opt(2026, 7, 3).unwrap(), // Observed
        NaiveDate::from_ymd_opt(2026, 9, 7).unwrap(),
        NaiveDate::from_ymd_opt(2026, 11, 26).unwrap(),
        NaiveDate::from_ymd_opt(2026, 12, 25).unwrap(),
    ]
}

/// Detect exact US equity market session at a given UTC timestamp.
pub fn detect_us_session(at: DateTime<Utc>) -> UsSessionInfo {
    let et_now = at.with_timezone(&New_York);
    let today = et_now.date_naive();
    let time = et_now.time();
    let weekday = et_now.weekday();

    let local_time_et = et_now.format("%H:%M:%S ET").to_string();
    let current_date_et = et_now.format("%d %b %Y").to_string();

    // Check Weekend
    if weekday == Weekday::Sat || weekday == Weekday::Sun {
        let next_mon = if weekday == Weekday::Sat { "Monday 09:30 ET" } else { "Tomorrow 09:30 ET" };
        return UsSessionInfo {
            session_kind: UsSessionKind::Closed,
            session_name: "Market Closed (Weekend)".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_et,
            status_badge: "⚪ CLOSED (WEEKEND)".to_string(),
            next_session_event: format!("Opens {}", next_mon),
            current_date_et,
            is_market_closed: true,
            strategy_paused_reason: Some("Market closed (Weekend) — strategy paused".to_string()),
        };
    }

    // Check Official Exchange Holidays
    if us_market_holidays().contains(&today) {
        return UsSessionInfo {
            session_kind: UsSessionKind::Closed,
            session_name: "Market Closed (Holiday)".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_et,
            status_badge: "⚪ CLOSED (HOLIDAY)".to_string(),
            next_session_event: "Next business day 09:30 ET".to_string(),
            current_date_et,
            is_market_closed: true,
            strategy_paused_reason: Some("Market closed (Holiday) — strategy paused".to_string()),
        };
    }

    // Session times in Eastern Time
    let t_premarket_start = NaiveTime::from_hms_opt(4, 0, 0).unwrap();
    let t_rth_open = NaiveTime::from_hms_opt(9, 30, 0).unwrap();
    let t_rth_close = NaiveTime::from_hms_opt(16, 0, 0).unwrap();
    let t_afterhours_close = NaiveTime::from_hms_opt(20, 0, 0).unwrap();

    if time < t_premarket_start {
        // Overnight Closed (Before 04:00 ET)
        UsSessionInfo {
            session_kind: UsSessionKind::Closed,
            session_name: "Market Closed (Overnight)".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_et,
            status_badge: "⚪ CLOSED".to_string(),
            next_session_event: "Premarket begins 04:00 ET".to_string(),
            current_date_et,
            is_market_closed: true,
            strategy_paused_reason: Some("Market closed — strategy paused".to_string()),
        }
    } else if time >= t_premarket_start && time < t_rth_open {
        // Pre-market Session: Strategy paused by default (regular hours only)
        UsSessionInfo {
            session_kind: UsSessionKind::PreMarket,
            session_name: "Pre-Market Session (04:00–09:30 ET)".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_et,
            status_badge: "🟡 PRE-MARKET".to_string(),
            next_session_event: "Regular Trading Hours open 09:30 ET".to_string(),
            current_date_et,
            is_market_closed: true,
            strategy_paused_reason: Some("Pre-market session active — strategy paused until regular session open (09:30 ET)".to_string()),
        }
    } else if time >= t_rth_open && time < t_rth_close {
        // Regular Trading Hours: Strategy active & fills permitted
        UsSessionInfo {
            session_kind: UsSessionKind::Regular,
            session_name: "Regular Trading Hours (09:30–16:00 ET)".to_string(),
            is_trading_active: true,
            allows_order_entry: true,
            local_time_et,
            status_badge: "🟢 RTH OPEN".to_string(),
            next_session_event: "Market closes 16:00 ET".to_string(),
            current_date_et,
            is_market_closed: false,
            strategy_paused_reason: None,
        }
    } else if time >= t_rth_close && time < t_afterhours_close {
        // After-Hours Session: Strategy paused
        UsSessionInfo {
            session_kind: UsSessionKind::AfterHours,
            session_name: "After-Hours Session (16:00–20:00 ET)".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_et,
            status_badge: "🟡 AFTER-HOURS".to_string(),
            next_session_event: "After-Hours closes 20:00 ET".to_string(),
            current_date_et,
            is_market_closed: true,
            strategy_paused_reason: Some("After-hours session active — strategy paused (RTH-only)".to_string()),
        }
    } else {
        // Post-market Closed (After 20:00 ET)
        let next_day = if weekday == Weekday::Fri { "Monday 09:30 ET" } else { "Tomorrow 09:30 ET" };
        UsSessionInfo {
            session_kind: UsSessionKind::Closed,
            session_name: "Market Closed".to_string(),
            is_trading_active: false,
            allows_order_entry: false,
            local_time_et,
            status_badge: "⚪ CLOSED".to_string(),
            next_session_event: format!("Opens {}", next_day),
            current_date_et,
            is_market_closed: true,
            strategy_paused_reason: Some("Market closed — strategy paused".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_us_session_rth() {
        // 2026-10-27 10:30:00 ET is regular trading hours
        let dt = New_York.with_ymd_and_hms(2026, 10, 27, 10, 30, 0).unwrap().with_timezone(&Utc);
        let session = detect_us_session(dt);
        assert_eq!(session.session_kind, UsSessionKind::Regular);
        assert!(session.is_trading_active);
        assert!(session.allows_order_entry);
        assert!(!session.is_market_closed);
    }

    #[test]
    fn test_us_session_premarket() {
        // 2026-10-27 07:30:00 ET is pre-market
        let dt = New_York.with_ymd_and_hms(2026, 10, 27, 7, 30, 0).unwrap().with_timezone(&Utc);
        let session = detect_us_session(dt);
        assert_eq!(session.session_kind, UsSessionKind::PreMarket);
        assert!(!session.is_trading_active);
        assert!(session.is_market_closed);
    }

    #[test]
    fn test_us_session_closed_weekend() {
        // 2026-10-25 is Sunday
        let dt = New_York.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap().with_timezone(&Utc);
        let session = detect_us_session(dt);
        assert_eq!(session.session_kind, UsSessionKind::Closed);
        assert!(session.is_market_closed);
    }
}
