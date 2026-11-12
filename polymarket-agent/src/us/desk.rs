//! The U.S. trading desk: six agents, one job each, one decision card per
//! signal.
//!
//! - ATLAS scans: which stocks are in play this morning (relative volume).
//! - VEGA reads the chart: the opening-range breakout, its side and stop.
//! - ORION reads the news for one thing only: a trading halt in the last day
//!   (a halted stock gaps through its stop; the 7 such trades in a 2,490-trade
//!   backtest lost 160-200 bp in both years).
//! - TITAN controls risk: every limit, with the size it allows.
//! - NOVA makes the final call: no VEGA signal, no trade; TITAN's block and a
//!   halt are final; otherwise trade.
//!
//! Removed 10 Nov 2026 at the user's request after an audit: LUNA (headline
//! tone and market mood; it never once objected in 24 live decisions, and mood
//! filters did not improve 210 backtest trades) and ORION's earnings / merger /
//! offering veto (advisory only; the backtest found it inconsistent, cutting
//! the held-out year from +21% to +4%, and it flagged 11 of 14 live trades).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;

/// One headline from the news feed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Headline {
    pub at: DateTime<Utc>,
    pub headline: String,
    pub summary: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Atlas {
    pub relative_volume: Decimal,
    pub range_high: Decimal,
    pub range_low: Decimal,
    /// How many of the universe were in play today.
    pub in_play_today: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Vega {
    pub side: String,
    pub setup: String,
    /// New York time the trigger bar closed.
    pub signal_at: String,
    pub trigger_close: Decimal,
    pub stop: Decimal,
    /// How far past the range the trigger closed, as % of the range.
    pub breakout_pct_of_range: Decimal,
}

#[derive(Debug, Clone, Serialize)]
pub struct Orion {
    pub headlines_24h: usize,
    pub events: Vec<String>,
    pub latest: Option<String>,
    /// A trading halt in the last 24h: always a skip.
    pub halted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Titan {
    pub allowed: bool,
    pub reason: String,
    pub qty: Decimal,
    pub notional_usd: Decimal,
    /// Loss if the stop fills at its price.
    pub risk_usd: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Action {
    Trade,
    Skip,
}

#[derive(Debug, Clone, Serialize)]
pub struct Nova {
    pub action: Action,
    /// Kept for the stored cards' column; now always equals `action`.
    pub advisory_action: Action,
    pub enforced: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionCard {
    pub at: DateTime<Utc>,
    pub market: String,
    pub strategy: String,
    pub symbol: String,
    pub atlas: Atlas,
    pub vega: Vega,
    pub orion: Orion,
    pub titan: Titan,
    pub nova: Nova,
}

// ---------------------------------------------------------------- ORION

/// Event categories that change how a stock trades on the day. Each is a
/// list of phrases matched case-insensitively in the headline and summary.
const EVENTS: &[(&str, &[&str])] = &[
    (
        "earnings",
        &[
            "earnings",
            "quarterly results",
            " q1 ",
            " q2 ",
            " q3 ",
            " q4 ",
            "eps ",
            "revenue beat",
            "revenue miss",
            "reports results",
        ],
    ),
    (
        "guidance",
        &[
            "guidance",
            "outlook",
            "forecast cut",
            "raises forecast",
            "lowers forecast",
        ],
    ),
    (
        "merger",
        &[
            "merger",
            "acquisition",
            "to acquire",
            "acquires",
            "takeover",
            "buyout",
            "deal to buy",
        ],
    ),
    (
        "offering",
        &[
            "offering",
            "share sale",
            "secondary",
            "dilution",
            "convertible notes",
        ],
    ),
    ("halt", &["halted", "trading halt", "halt "]),
    (
        "regulatory",
        &[
            "sec probe",
            "investigation",
            "lawsuit",
            "subpoena",
            "doj",
            "ftc",
            "antitrust",
            "recall",
            "fda",
        ],
    ),
    (
        "analyst",
        &["upgrade", "downgrade", "price target", "initiates coverage"],
    ),
];

pub fn tag_events(text: &str) -> Vec<&'static str> {
    let t = format!(" {} ", text.to_ascii_lowercase());
    EVENTS
        .iter()
        .filter(|(_, phrases)| phrases.iter().any(|p| t.contains(p)))
        .map(|(name, _)| *name)
        .collect()
}

pub fn orion(headlines: &[Headline], now: DateTime<Utc>) -> Orion {
    let recent: Vec<&Headline> = headlines
        .iter()
        .filter(|h| now.signed_duration_since(h.at) <= chrono::Duration::hours(24) && h.at <= now)
        .collect();
    let mut events: Vec<String> = Vec::new();
    for h in &recent {
        for tag in tag_events(&format!("{} {}", h.headline, h.summary)) {
            if !events.iter().any(|e| e == tag) {
                events.push(tag.to_string());
            }
        }
    }
    let halted = events.iter().any(|e| e == "halt");
    Orion {
        headlines_24h: recent.len(),
        latest: recent
            .iter()
            .max_by_key(|h| h.at)
            .map(|h| h.headline.clone()),
        events,
        halted,
    }
}

// ---------------------------------------------------------------- tone
// (used by the live news stream's display; no trading decision reads it)

/// Word stems: a headline word counts when it starts with one ("plunges").
const POSITIVE: &[&str] = &[
    "beat",
    "surge",
    "soar",
    "jump",
    "rally",
    "record",
    "upgrade",
    "raise",
    "strong",
    "growth",
    "gain",
    "bullish",
    "outperform",
    "approv",
    "win",
    "partnership",
];
const NEGATIVE: &[&str] = &[
    "miss",
    "plunge",
    "sink",
    "drop",
    "fall",
    "slump",
    "downgrade",
    "cut",
    "weak",
    "loss",
    "bearish",
    "underperform",
    "lawsuit",
    "probe",
    "recall",
    "layoff",
    "warn",
    "decline",
];

/// Headline tone in -1..+1: (positive words - negative words) / matches.
pub fn tone(headlines: &[Headline]) -> Decimal {
    let (mut pos, mut neg) = (0i64, 0i64);
    for h in headlines {
        let cleaned: String = h
            .headline
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
            .collect();
        for word in cleaned.split_whitespace() {
            if POSITIVE.iter().any(|s| word.starts_with(s)) {
                pos += 1;
            } else if NEGATIVE.iter().any(|s| word.starts_with(s)) {
                neg += 1;
            }
        }
    }
    if pos + neg == 0 {
        return Decimal::ZERO;
    }
    (Decimal::from(pos - neg) / Decimal::from(pos + neg)).round_dp(2)
}

// ---------------------------------------------------------------- NOVA

pub fn nova(titan: &Titan, orion: &Orion) -> Nova {
    let mut reasons = Vec::new();
    let action = if !titan.allowed {
        reasons.push(format!("TITAN: {}", titan.reason));
        Action::Skip
    } else if orion.halted {
        reasons.push("ORION: trading halt in the last 24h".to_string());
        Action::Skip
    } else {
        reasons.push("VEGA signal confirmed; TITAN passed; no halt".to_string());
        Action::Trade
    };
    Nova {
        action,
        advisory_action: action,
        enforced: true,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn h(minutes_ago: i64, text: &str) -> Headline {
        Headline {
            at: Utc::now() - chrono::Duration::minutes(minutes_ago),
            headline: text.to_string(),
            summary: String::new(),
            source: "test".to_string(),
        }
    }

    fn titan(allowed: bool) -> Titan {
        Titan {
            allowed,
            reason: if allowed {
                "within limits".into()
            } else {
                "daily trade limit reached".into()
            },
            qty: dec!(1),
            notional_usd: dec!(200),
            risk_usd: dec!(4),
        }
    }

    #[test]
    fn orion_tags_events_but_only_a_halt_matters() {
        let news = vec![
            h(30, "Tesla Q3 earnings beat estimates"),
            h(60, "Analyst raises price target"),
        ];
        let o = orion(&news, Utc::now());
        assert_eq!(o.headlines_24h, 2);
        assert!(o.events.contains(&"earnings".to_string()));
        assert!(!o.halted);
        assert_eq!(nova(&titan(true), &o).action, Action::Trade);
    }

    #[test]
    fn orion_ignores_news_older_than_a_day() {
        let o = orion(&[h(60 * 30, "Shares halted pending news")], Utc::now());
        assert_eq!(o.headlines_24h, 0);
        assert!(!o.halted);
    }

    #[test]
    fn tone_counts_positive_and_negative_words() {
        assert_eq!(tone(&[h(1, "Shares surge after record quarter")]), dec!(1));
        assert_eq!(tone(&[h(1, "Stock plunges on downgrade")]), dec!(-1));
        assert_eq!(
            tone(&[h(1, "Stock jumps on upgrade but misses revenue")]),
            dec!(0.33)
        );
        assert_eq!(tone(&[h(1, "Company holds annual meeting")]), dec!(0));
    }

    #[test]
    fn titan_and_a_halt_are_the_only_skips() {
        let calm = orion(&[], Utc::now());
        assert_eq!(nova(&titan(true), &calm).action, Action::Trade);
        assert_eq!(nova(&titan(false), &calm).action, Action::Skip);
        let halted = orion(&[h(20, "Shares halted pending news")], Utc::now());
        let n = nova(&titan(true), &halted);
        assert_eq!(n.action, Action::Skip);
        assert!(n.reasons[0].contains("halt"));
    }
}
