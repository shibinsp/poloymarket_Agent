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
    Dead,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Alive => write!(f, "ALIVE"),
            Self::LowFuel => write!(f, "LOW_FUEL"),
            Self::CriticalSurvival => write!(f, "CRITICAL_SURVIVAL"),
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
