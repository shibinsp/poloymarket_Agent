//! Polymarket "underdog a week out" paper book (built 9 Nov 2026 at the
//! user's request to make Polymarket earn).
//!
//! The rule: about a week before a market's scheduled end, buy the outcome
//! token priced 20-30c and hold it to settlement. No model call and no stop.
//!
//! Evidence (scratchpad `poly_underdog_score.py`, 3,987 resolved markets,
//! Oct 2025 - Oct 2026, one bet per event, 1c over mid): +48% a bet at 168 h,
//! both halves positive, 10 of 13 months positive, 85% of nearby settings
//! positive (15-25c / 20-30c, 120-216 h), walk-forward +29% a bet, +43% at 2c
//! cost. Caveat: the sample was the highest-volume closed markets, and volume
//! is known only afterwards, which may favour upsets. That is why this is a
//! small paper book judged on live settlements.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::market::models::{MarketCandidate, Side};

/// Strategy tag stored on each trade; exits skip it (it holds to settlement).
pub const STRATEGY: &str = "poly_underdog";
/// Entry window, hours before the scheduled end. The scan runs every few
/// minutes outside US hours, so a market is usually caught near 180 h, or
/// later if it entered the window during US hours.
pub const MIN_HOURS: i64 = 156;
pub const MAX_HOURS: i64 = 180;
/// Token price band, paying the ask.
pub const MIN_PRICE: Decimal = dec!(0.20);
pub const MAX_PRICE: Decimal = dec!(0.30);
/// Dollars per bet, at most this many new bets a scan, and at most this much
/// open in the book at once.
pub const STAKE_USD: Decimal = dec!(2);
pub const MAX_NEW_PER_SCAN: usize = 3;
pub const MAX_OPEN_USD: Decimal = dec!(30);

/// The side and price to buy, if this market fits the rule now.
/// YES is bought at the best ask; NO at 1 - best bid (the NO token's ask).
pub fn pick(c: &MarketCandidate, now: DateTime<Utc>) -> Option<(Side, Decimal)> {
    let hours = (c.market.end_date - now).num_hours();
    if !(MIN_HOURS..=MAX_HOURS).contains(&hours) || !c.market.active {
        return None;
    }
    let in_band = |p: Decimal| p >= MIN_PRICE && p < MAX_PRICE;
    if let Some(ask) = c.order_book.asks.first().map(|l| l.price) {
        if in_band(ask) {
            return Some((Side::Yes, ask));
        }
    }
    if let Some(bid) = c.order_book.bids.first().map(|l| l.price) {
        let no = Decimal::ONE - bid;
        if in_band(no) {
            return Some((Side::No, no));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::models::{Market, MarketCategory, OrderBookSnapshot, PriceLevel, TokenInfo};
    use chrono::Duration;

    fn cand(hours: i64, bid: Decimal, ask: Decimal) -> MarketCandidate {
        let now = Utc::now();
        MarketCandidate {
            market: Market {
                condition_id: "0xabc".into(),
                question: "Q?".into(),
                outcomes: vec!["Yes".into(), "No".into()],
                tokens: vec![
                    TokenInfo {
                        token_id: "y".into(),
                        outcome: "Yes".into(),
                        price: ask,
                    },
                    TokenInfo {
                        token_id: "n".into(),
                        outcome: "No".into(),
                        price: Decimal::ONE - bid,
                    },
                ],
                end_date: now + Duration::hours(hours) + Duration::minutes(30),
                category: MarketCategory::Politics,
                volume_24h: dec!(5000),
                active: true,
            },
            order_book: OrderBookSnapshot {
                token_id: "y".into(),
                bids: vec![PriceLevel {
                    price: bid,
                    size: dec!(500),
                }],
                asks: vec![PriceLevel {
                    price: ask,
                    size: dec!(500),
                }],
                spread: ask - bid,
                midpoint: (ask + bid) / dec!(2),
                implied_probability: (ask + bid) / dec!(2),
                timestamp: now,
            },
        }
    }

    #[test]
    fn buys_a_cheap_yes_a_week_out() {
        let p = pick(&cand(168, dec!(0.23), dec!(0.25)), Utc::now());
        assert_eq!(p, Some((Side::Yes, dec!(0.25))));
    }

    #[test]
    fn buys_a_cheap_no_a_week_out() {
        // YES 0.74/0.76 -> NO costs 1 - 0.74 = 0.26.
        let p = pick(&cand(170, dec!(0.74), dec!(0.76)), Utc::now());
        assert_eq!(p, Some((Side::No, dec!(0.26))));
    }

    #[test]
    fn skips_outside_the_window_or_band() {
        assert_eq!(pick(&cand(100, dec!(0.23), dec!(0.25)), Utc::now()), None);
        assert_eq!(pick(&cand(200, dec!(0.23), dec!(0.25)), Utc::now()), None);
        assert_eq!(pick(&cand(168, dec!(0.48), dec!(0.50)), Utc::now()), None);
        assert_eq!(pick(&cand(168, dec!(0.29), dec!(0.31)), Utc::now()), None);
    }
}
