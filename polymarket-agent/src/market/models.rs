use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Our domain representation of a Polymarket market.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub condition_id: String,
    pub question: String,
    pub outcomes: Vec<String>,
    pub tokens: Vec<TokenInfo>,
    pub end_date: DateTime<Utc>,
    pub category: MarketCategory,
    pub volume_24h: Decimal,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    pub token_id: String,
    pub outcome: String,
    pub price: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MarketCategory {
    Weather,
    Sports,
    Crypto,
    Politics,
    #[serde(untagged)]
    Other(String),
}

/// Snapshot of an order book at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    pub token_id: String,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    pub spread: Decimal,
    pub midpoint: Decimal,
    pub implied_probability: Decimal,
    pub timestamp: DateTime<Utc>,
}

impl OrderBookSnapshot {
    /// The price levels that will actually be consumed when trading `side`.
    ///
    /// Buying YES lifts the ask side of the book; buying NO is priced off the
    /// complement of the YES bid side (see `execution::order::prepare_order`,
    /// which derives the NO execution price as `1 - best_bid`). Callers that
    /// need depth/liquidity for a specific side must use this instead of
    /// reaching for `asks` unconditionally, or they'll pair one side's
    /// liquidity with the other side's reference price.
    pub fn levels_for_side(&self, side: Side) -> &[PriceLevel] {
        match side {
            Side::Yes => &self.asks,
            Side::No => &self.bids,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: Decimal,
    pub size: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceHistoryPoint {
    pub timestamp: DateTime<Utc>,
    pub price: Decimal,
}

/// A market that passed initial scanning filters and is a candidate for valuation.
#[derive(Debug, Clone)]
pub struct MarketCandidate {
    pub market: Market,
    pub order_book: OrderBookSnapshot,
}

/// A fully evaluated trading opportunity.
#[derive(Debug, Clone)]
pub struct Opportunity {
    pub market: Market,
    pub order_book: OrderBookSnapshot,
    pub fair_value: Decimal,
    pub confidence: Decimal,
    pub edge: Decimal,
    pub recommended_side: Side,
    pub kelly_size: Decimal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Side {
    Yes,
    No,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    Alive,
    LowFuel,
    CriticalSurvival,
    /// Entries are stopped. Exits, order polling, reconciliation and
    /// settlement all continue.
    ///
    /// Deliberately not the same axis as the survival ladder above, which is
    /// derived from the balance every cycle and would overwrite this on the
    /// next pass. The precedence is fixed in `apply_halt`: `Dead` outranks a
    /// halt (an account at zero is not something a human resume can fix), and
    /// a halt outranks everything else.
    ///
    /// Refusing to *close* a position because the day went badly is how a
    /// bounded loss becomes an unbounded one, so nothing about this state
    /// touches the exit path.
    Halted,
    Dead,
}

/// Fold a halt into the state the survival check computed.
///
/// Kept as a free function rather than buried in the lifecycle so the
/// precedence is stated in one place and can be tested without an agent.
pub fn apply_halt(survival: AgentState, halted: bool) -> AgentState {
    match (survival, halted) {
        // Death wins. A halted agent whose balance has gone to zero still
        // needs to shut down and settle; waiting for someone to resume it
        // would leave it cycling forever against an empty account.
        (AgentState::Dead, _) => AgentState::Dead,
        (_, true) => AgentState::Halted,
        (other, false) => other,
    }
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Alive => write!(f, "ALIVE"),
            Self::LowFuel => write!(f, "LOW_FUEL"),
            Self::CriticalSurvival => write!(f, "CRITICAL_SURVIVAL"),
            Self::Halted => write!(f, "HALTED"),
            Self::Dead => write!(f, "DEAD"),
        }
    }
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Yes => write!(f, "YES"),
            Self::No => write!(f, "NO"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn a_halt_overrides_the_survival_ladder() {
        for survival in [
            AgentState::Alive,
            AgentState::LowFuel,
            AgentState::CriticalSurvival,
        ] {
            assert_eq!(
                apply_halt(survival, true),
                AgentState::Halted,
                "{survival} must yield to a halt"
            );
        }
    }

    #[test]
    fn death_outranks_a_halt() {
        // An account at zero is not something a human resume can fix, and a
        // halted agent that never reaches Dead never shuts down or settles —
        // it cycles forever against an empty account waiting for someone.
        assert_eq!(apply_halt(AgentState::Dead, true), AgentState::Dead);
        assert_eq!(apply_halt(AgentState::Dead, false), AgentState::Dead);
    }

    #[test]
    fn without_a_halt_the_survival_state_passes_through_unchanged() {
        for survival in [
            AgentState::Alive,
            AgentState::LowFuel,
            AgentState::CriticalSurvival,
            AgentState::Dead,
        ] {
            assert_eq!(apply_halt(survival, false), survival);
        }
    }

    #[test]
    fn halted_renders_as_a_stable_identifier() {
        // Written to `cycles.agent_state` and served on /api/health; the
        // dashboard keys off it.
        assert_eq!(AgentState::Halted.to_string(), "HALTED");
    }

    fn book_with(bid_size: Decimal, ask_size: Decimal) -> OrderBookSnapshot {
        OrderBookSnapshot {
            token_id: "tok".to_string(),
            bids: vec![PriceLevel {
                price: dec!(0.40),
                size: bid_size,
            }],
            asks: vec![PriceLevel {
                price: dec!(0.60),
                size: ask_size,
            }],
            spread: dec!(0.20),
            midpoint: dec!(0.50),
            implied_probability: dec!(0.50),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn levels_for_side_yes_uses_asks() {
        // Asymmetric depth: if Side::Yes ever picked bids instead, this
        // would return 300 instead of 1000.
        let book = book_with(dec!(300), dec!(1000));
        let levels = book.levels_for_side(Side::Yes);
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].price, dec!(0.60));
        assert_eq!(levels[0].size, dec!(1000));
    }

    #[test]
    fn levels_for_side_no_uses_bids() {
        // Same asymmetric book: Side::No must resolve to the bid side, not
        // the (much deeper, wrong-side) ask depth.
        let book = book_with(dec!(300), dec!(1000));
        let levels = book.levels_for_side(Side::No);
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].price, dec!(0.40));
        assert_eq!(levels[0].size, dec!(300));
    }
}
