//! The U.S. trading desk: six agents, one job each, one decision card per
//! signal.
//!
//! - ATLAS scans: which stocks are in play this morning (relative volume).
//! - VEGA reads the chart: the opening-range breakout, its side and stop.
//! - ORION tracks the news: the stock's last day of headlines, tagged for
//!   events that make opening ranges unreliable (earnings, deals, halts...).
//! - LUNA measures sentiment: headline tone and the market's mood.
//! - TITAN controls risk: every limit, with the size it allows.
//! - NOVA makes the final call from the five reports.
//!
//! NOVA's rules: no VEGA signal, no trade; TITAN's block is final; a trading
//! halt in the last day is always a skip (a halted stock gaps through its
//! stop). ORION's other event vetoes and LUNA's half size are *advisory*: the
//! card records what they would have done, and only "enforce" mode acts on
//! them. A backtest over 2,490 ORB trades (Nov 2024 - Oct 2026) found the
//! earnings/merger/offering veto inconsistent - it cut the held-out year from
//! +21% to +4%, since news is often what puts a stock in play - while the 7
//! trades after a halt lost 160-200 bp in both years.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
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
    /// The advisory event veto, if any (enforced only in "enforce" mode).
    pub veto: Option<String>,
    /// A trading halt in the last 24h: always a skip.
    pub halted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Luna {
    /// Headline tone for the stock, -1 (all negative) .. +1 (all positive).
    pub stock_tone: Decimal,
    /// SPY's move since the open, percent.
    pub market_move_pct: Decimal,
    /// Share of the universe whose first bar closed up, percent.
    pub breadth_up_pct: Decimal,
    pub mood: String,
    /// Tone and mood both point the other way from the trade.
    pub against_trade: bool,
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
    TradeHalf,
    Skip,
}

#[derive(Debug, Clone, Serialize)]
pub struct Nova {
    pub action: Action,
    /// What ORION and LUNA alone would have done (equals `action` when enforced).
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
    pub luna: Luna,
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

/// Events whose breakouts the advisory veto would skip: the open is driven by
/// the news rather than by a range being tested. Not enforced by default.
const ADVISORY_VETO_EVENTS: &[&str] = &["earnings", "merger", "offering"];

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
    let veto = events
        .iter()
        .find(|e| ADVISORY_VETO_EVENTS.contains(&e.as_str()))
        .map(|e| format!("{e} news in the last 24h"));
    let halted = events.iter().any(|e| e == "halt");
    Orion {
        headlines_24h: recent.len(),
        latest: recent
            .iter()
            .max_by_key(|h| h.at)
            .map(|h| h.headline.clone()),
        events,
        veto,
        halted,
    }
}

// ---------------------------------------------------------------- LUNA

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

pub fn luna(
    headlines: &[Headline],
    market_move_pct: Decimal,
    breadth_up_pct: Decimal,
    long: bool,
) -> Luna {
    let stock_tone = tone(headlines);
    let market_score = if market_move_pct > dec!(0.3) && breadth_up_pct >= dec!(55) {
        1
    } else if market_move_pct < dec!(-0.3) && breadth_up_pct <= dec!(45) {
        -1
    } else {
        0
    };
    let mood = match market_score {
        1 => "bullish",
        -1 => "bearish",
        _ => "neutral",
    }
    .to_string();
    // Against the trade only when the stock's own news and the market agree
    // on the other direction: one weak reading alone is noise.
    let against_trade = if long {
        stock_tone <= dec!(-0.5) && market_score < 0
    } else {
        stock_tone >= dec!(0.5) && market_score > 0
    };
    Luna {
        stock_tone,
        market_move_pct: market_move_pct.round_dp(2),
        breadth_up_pct: breadth_up_pct.round_dp(1),
        mood,
        against_trade,
    }
}

// ---------------------------------------------------------------- NOVA

pub fn nova(titan: &Titan, orion: &Orion, luna: &Luna, enforce: bool) -> Nova {
    let mut reasons = Vec::new();
    let advisory_action = if orion.halted {
        reasons.push("ORION: trading halt in the last 24h (always enforced)".to_string());
        Action::Skip
    } else if let Some(v) = &orion.veto {
        reasons.push(format!("ORION: {v}"));
        Action::Skip
    } else if luna.against_trade {
        reasons.push(format!(
            "LUNA: tone {} with a {} market is against the trade",
            luna.stock_tone, luna.mood
        ));
        Action::TradeHalf
    } else {
        Action::Trade
    };
    let action = if !titan.allowed {
        reasons.insert(0, format!("TITAN: {}", titan.reason));
        Action::Skip
    } else if enforce || orion.halted {
        advisory_action
    } else {
        if advisory_action != Action::Trade {
            reasons.push("advisory only: ORION/LUNA rules are not yet validated".to_string());
        }
        Action::Trade
    };
    if action == Action::Trade && reasons.is_empty() {
        reasons.push(
            "VEGA signal confirmed; TITAN passed; no news or sentiment objection".to_string(),
        );
    }
    Nova {
        action,
        advisory_action: if titan.allowed {
            advisory_action
        } else {
            Action::Skip
        },
        enforced: enforce,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn orion_tags_events_and_vetoes_on_earnings_but_not_analyst_notes() {
        let news = vec![
            h(30, "Tesla Q3 earnings beat estimates"),
            h(60, "Analyst raises price target"),
        ];
        let o = orion(&news, Utc::now());
        assert_eq!(o.headlines_24h, 2);
        assert!(o.events.contains(&"earnings".to_string()));
        assert!(o.events.contains(&"analyst".to_string()));
        assert!(o.veto.as_deref().unwrap().starts_with("earnings"));

        let quiet = orion(&[h(30, "Analyst raises price target")], Utc::now());
        assert_eq!(quiet.veto, None);
    }

    #[test]
    fn orion_ignores_news_older_than_a_day() {
        let o = orion(&[h(60 * 30, "Company announces merger")], Utc::now());
        assert_eq!(o.headlines_24h, 0);
        assert_eq!(o.veto, None);
    }

    #[test]
    fn tone_counts_positive_and_negative_words() {
        assert_eq!(tone(&[h(1, "Shares surge after record quarter")]), dec!(1));
        assert_eq!(tone(&[h(1, "Stock plunges on downgrade")]), dec!(-1));
        // jumps, upgrade (+2) against misses (-1): (2 - 1) / 3
        assert_eq!(
            tone(&[h(1, "Stock jumps on upgrade but misses revenue")]),
            dec!(0.33)
        );
        assert_eq!(tone(&[h(1, "Company holds annual meeting")]), dec!(0));
    }

    #[test]
    fn luna_objects_only_when_stock_and_market_agree_against_the_trade() {
        let bad = [h(1, "Shares sink after downgrade and weak outlook")];
        assert!(luna(&bad, dec!(-0.8), dec!(30), true).against_trade);
        assert!(
            !luna(&bad, dec!(0.8), dec!(70), true).against_trade,
            "market disagrees"
        );
        assert!(
            !luna(&bad, dec!(-0.8), dec!(30), false).against_trade,
            "a short wants this"
        );
    }

    #[test]
    fn nova_follows_titan_always_and_orion_luna_only_when_enforced() {
        let calm_orion = orion(&[], Utc::now());
        let calm_luna = luna(&[], dec!(0), dec!(50), true);
        assert_eq!(
            nova(&titan(true), &calm_orion, &calm_luna, false).action,
            Action::Trade
        );
        assert_eq!(
            nova(&titan(false), &calm_orion, &calm_luna, false).action,
            Action::Skip
        );

        let event = orion(&[h(10, "Company to acquire rival in merger")], Utc::now());
        let advisory = nova(&titan(true), &event, &calm_luna, false);
        assert_eq!(advisory.action, Action::Trade);
        assert_eq!(advisory.advisory_action, Action::Skip);
        assert_eq!(
            nova(&titan(true), &event, &calm_luna, true).action,
            Action::Skip
        );

        let gloomy = luna(
            &[h(1, "Shares sink after downgrade and weak outlook")],
            dec!(-0.8),
            dec!(30),
            true,
        );
        assert_eq!(
            nova(&titan(true), &calm_orion, &gloomy, true).action,
            Action::TradeHalf
        );
    }

    #[test]
    fn a_halt_is_always_a_skip_even_in_advisory_mode() {
        let calm_luna = luna(&[], dec!(0), dec!(50), true);
        let halted = orion(&[h(20, "Shares halted pending news")], Utc::now());
        assert!(halted.halted);
        assert_eq!(halted.veto, None, "a halt is not an advisory veto");
        let n = nova(&titan(true), &halted, &calm_luna, false);
        assert_eq!(n.action, Action::Skip);
        assert!(n.reasons[0].contains("halt"));

        // Earnings news stays advisory.
        let earnings = orion(
            &[h(20, "Company reports results, EPS above estimates")],
            Utc::now(),
        );
        assert!(!earnings.halted);
        assert_eq!(
            nova(&titan(true), &earnings, &calm_luna, false).action,
            Action::Trade
        );
    }
}
