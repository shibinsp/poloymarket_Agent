//! Venue-agnostic domain types.
//!
//! These replace the Polymarket-specific vocabulary (`condition_id`, YES/NO
//! token pairs, probability-as-price) with terms that also describe equities
//! and crypto spot. The key modelling decision is that a prediction market's
//! two outcomes are *separate instruments*: buying NO is `Side::Buy` on the NO
//! instrument, not a sell of YES. That removes the `1 - price` complement
//! arithmetic — and the inverted-order-side bug class — by construction.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::market::models::{OrderBookSnapshot, PriceLevel};

/// Identifies a venue instance (`polymarket`, `alpaca`, `binance_us`, …).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VenueId(String);

impl VenueId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VenueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What kind of thing is being traded. Drives sizing, exit and settlement
/// behaviour, which differ fundamentally between a contract that resolves to
/// $0/$1 and one that simply has a price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetClass {
    /// Binary outcome contract settling at 0 or 1 (Polymarket).
    PredictionBinary,
    /// Spot crypto pair, traded 24/7.
    CryptoSpot,
    /// Listed equity, traded in sessions.
    Equity,
}

impl AssetClass {
    /// Prediction markets settle; continuous assets are closed by trading out.
    pub fn settles(&self) -> bool {
        matches!(self, AssetClass::PredictionBinary)
    }
}

/// Direction of an order. Unlike the old `Side::{Yes, No}`, this says nothing
/// about *which* outcome — that is the instrument's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn opposite(&self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Side::Buy => f.write_str("BUY"),
            Side::Sell => f.write_str("SELL"),
        }
    }
}

/// Globally unique instrument reference: which venue, and its symbol there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstrumentId {
    pub venue: VenueId,
    /// Venue-native symbol. Prediction markets use `{condition_id}:{OUTCOME}`
    /// so the two outcomes of one market are distinct, self-describing ids.
    pub symbol: String,
}

impl InstrumentId {
    pub fn new(venue: VenueId, symbol: impl Into<String>) -> Self {
        Self {
            venue,
            symbol: symbol.into(),
        }
    }

    /// Stable key for caches and database rows — also prevents two venues'
    /// "BTC" from colliding.
    pub fn key(&self) -> String {
        format!("{}:{}", self.venue, self.symbol)
    }
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.venue, self.symbol)
    }
}

/// Venue-specific details that don't generalise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InstrumentMeta {
    Prediction {
        condition_id: String,
        /// "Yes" / "No" as the venue spells it.
        outcome: String,
        /// CLOB token id for this specific outcome.
        token_id: String,
        question: String,
        end_date: DateTime<Utc>,
    },
    Spot {
        base: String,
    },
    Equity {
        exchange: String,
    },
}

/// A tradeable instrument plus the constraints needed to size an order for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instrument {
    pub id: InstrumentId,
    pub asset_class: AssetClass,
    pub display_name: String,
    /// Currency the price is quoted in ("USD", "USDC").
    pub quote_ccy: String,
    /// Minimum price increment, if the venue enforces one.
    pub tick_size: Option<Decimal>,
    /// Minimum quantity increment, if the venue enforces one.
    pub lot_size: Option<Decimal>,
    /// Smallest order value the venue will accept. Binance.US rejects orders
    /// under roughly $10, which matters a great deal at a $100 bankroll.
    pub min_notional: Option<Decimal>,
    /// Whether fractional quantities are allowed (Alpaca equities, crypto).
    pub fractional: bool,
    pub meta: InstrumentMeta,
}

impl Instrument {
    pub fn venue(&self) -> &VenueId {
        &self.id.venue
    }

    pub fn symbol(&self) -> &str {
        &self.id.symbol
    }

    /// Round a quantity down to the venue's lot size, so an order is never
    /// rejected for an unrepresentable size.
    pub fn round_qty(&self, qty: Decimal) -> Decimal {
        match self.lot_size {
            Some(lot) if lot > Decimal::ZERO => (qty / lot).floor() * lot,
            _ => qty,
        }
    }

    /// Round a price to the venue's tick size, toward the side that is safe to
    /// pay: down when buying, up when selling.
    pub fn round_price(&self, price: Decimal, side: Side) -> Decimal {
        match self.tick_size {
            Some(tick) if tick > Decimal::ZERO => {
                let ticks = price / tick;
                let rounded = match side {
                    Side::Buy => ticks.floor(),
                    Side::Sell => ticks.ceil(),
                };
                rounded * tick
            }
            _ => price,
        }
    }

    /// Whether an order of this notional value clears the venue's minimum.
    pub fn meets_min_notional(&self, notional: Decimal) -> bool {
        match self.min_notional {
            Some(min) => notional >= min,
            None => true,
        }
    }
}

/// Top of book plus optional depth, at a point in time.
#[derive(Debug, Clone)]
pub struct Quote {
    pub instrument: InstrumentId,
    pub bid: Decimal,
    pub ask: Decimal,
    /// For `PredictionBinary` this *is* the implied probability.
    pub mid: Decimal,
    pub last: Option<Decimal>,
    pub ts: DateTime<Utc>,
    /// Full depth when the venue provides it; top-of-book-only venues omit it.
    pub book: Option<OrderBookSnapshot>,
}

impl Quote {
    /// Absolute spread. Compare against `mid` for a relative figure.
    pub fn spread(&self) -> Decimal {
        self.ask - self.bid
    }

    /// Spread as a fraction of mid, or `None` at a degenerate mid of zero.
    pub fn spread_pct(&self) -> Option<Decimal> {
        if self.mid > Decimal::ZERO {
            Some(self.spread() / self.mid)
        } else {
            None
        }
    }

    /// Price a marketable order of this side would have to cross to.
    pub fn taker_price(&self, side: Side) -> Decimal {
        match side {
            Side::Buy => self.ask,
            Side::Sell => self.bid,
        }
    }

    /// Depth levels that a taker order of this side consumes.
    pub fn levels_for_side(&self, side: Side) -> &[PriceLevel] {
        match (&self.book, side) {
            (Some(book), Side::Buy) => &book.asks,
            (Some(book), Side::Sell) => &book.bids,
            (None, _) => &[],
        }
    }
}

/// One OHLCV bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candle {
    pub ts: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandleInterval {
    H1,
    H4,
    D1,
}

impl CandleInterval {
    pub fn hours(&self) -> i64 {
        match self {
            CandleInterval::H1 => 1,
            CandleInterval::H4 => 4,
            CandleInterval::D1 => 24,
        }
    }
}

/// How long an order stays live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeInForce {
    /// Good till cancelled.
    Gtc,
    /// Immediate or cancel.
    Ioc,
    /// Good for the trading day.
    Day,
    /// Good till an explicit expiry.
    Gtd(DateTime<Utc>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderKind {
    Limit { price: Decimal },
    Market,
}

impl OrderKind {
    pub fn limit_price(&self) -> Option<Decimal> {
        match self {
            OrderKind::Limit { price } => Some(*price),
            OrderKind::Market => None,
        }
    }
}

/// An order to send to a venue.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderRequest {
    pub instrument: Instrument,
    pub side: Side,
    pub kind: OrderKind,
    pub qty: Decimal,
    pub tif: TimeInForce,
    /// Required by equity venues outside regular hours.
    pub extended_hours: bool,
    /// Caller-generated id, echoed by the venue. Makes a retry after a network
    /// timeout idempotent instead of a possible double-placement.
    pub client_order_id: String,
}

impl OrderRequest {
    /// Notional value at the limit price, where there is one.
    pub fn notional(&self) -> Option<Decimal> {
        self.kind.limit_price().map(|p| p * self.qty)
    }
}

/// Lifecycle state of an order at the venue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderState {
    /// Accepted but nothing filled yet.
    Accepted,
    PartiallyFilled,
    Filled,
    Cancelled,
    Expired,
    Rejected(String),
    /// The venue's answer is unknown — a request timed out, so the order may
    /// or may not exist. Must be resolved by querying before any retry.
    Unknown,
}

impl OrderState {
    /// Whether the venue can still fill this order.
    pub fn is_open(&self) -> bool {
        matches!(self, OrderState::Accepted | OrderState::PartiallyFilled)
    }

    /// Whether this order will never do anything further.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OrderState::Filled
                | OrderState::Cancelled
                | OrderState::Expired
                | OrderState::Rejected(_)
        )
    }
}

/// What the venue said about an order.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderAck {
    pub venue_order_id: String,
    pub client_order_id: String,
    pub state: OrderState,
    pub filled_qty: Decimal,
    /// Average price actually paid, once anything has filled.
    pub avg_fill_price: Option<Decimal>,
    pub fees: Decimal,
}

impl OrderAck {
    /// Value actually transacted so far.
    pub fn filled_notional(&self) -> Decimal {
        self.avg_fill_price
            .map(|p| p * self.filled_qty)
            .unwrap_or(Decimal::ZERO)
    }
}

/// How to refer to an order when asking the venue about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderRef {
    Client(String),
    Venue(String),
}

/// A position as the *venue* reports it — the source of truth for
/// reconciliation against local records.
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub instrument: InstrumentId,
    pub qty: Decimal,
    pub avg_entry: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Balance {
    pub ccy: String,
    /// Free to deploy.
    pub available: Decimal,
    /// Including the value of open positions.
    pub total: Decimal,
}

/// Outcome of a settled prediction market.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settlement {
    pub won: bool,
    /// Payout per unit held — 1.0 for a winning binary contract.
    pub payout_per_unit: Decimal,
}

/// What a venue can and cannot do, so callers don't have to special-case by id.
#[derive(Debug, Clone, PartialEq)]
pub struct VenueCapabilities {
    pub asset_classes: Vec<AssetClass>,
    /// Venue rejects market orders outside regular hours (Alpaca equities).
    pub limit_only_outside_regular: bool,
    /// Venue echoes a caller-supplied client order id.
    pub supports_client_order_id: bool,
    /// Venue exposes historical bars.
    pub supports_candles: bool,
}

impl VenueCapabilities {
    pub fn supports(&self, class: AssetClass) -> bool {
        self.asset_classes.contains(&class)
    }
}

/// Filter for instrument discovery.
#[derive(Debug, Clone, Default)]
pub struct ScanFilter {
    pub asset_classes: Vec<AssetClass>,
    /// Explicit universe. Equity/crypto venues need one; prediction venues
    /// discover instruments dynamically.
    pub symbols: Vec<String>,
    pub min_volume_24h: Option<Decimal>,
    pub max_days_to_resolution: Option<u32>,
    pub max_results: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn instrument(tick: Option<Decimal>, lot: Option<Decimal>, min: Option<Decimal>) -> Instrument {
        Instrument {
            id: InstrumentId::new(VenueId::new("alpaca"), "AAPL"),
            asset_class: AssetClass::Equity,
            display_name: "Apple".to_string(),
            quote_ccy: "USD".to_string(),
            tick_size: tick,
            lot_size: lot,
            min_notional: min,
            fractional: true,
            meta: InstrumentMeta::Equity {
                exchange: "NASDAQ".to_string(),
            },
        }
    }

    #[test]
    fn instrument_id_key_disambiguates_venues() {
        let a = InstrumentId::new(VenueId::new("alpaca"), "BTC/USD");
        let b = InstrumentId::new(VenueId::new("binance_us"), "BTC/USD");
        assert_ne!(a.key(), b.key());
        assert_eq!(a.key(), "alpaca:BTC/USD");
    }

    #[test]
    fn prediction_outcomes_are_separate_instruments() {
        // The whole point of the model: YES and NO are distinct ids, so
        // "buy NO" needs no price complement or side inversion.
        let yes = InstrumentId::new(VenueId::new("polymarket"), "0xabc:YES");
        let no = InstrumentId::new(VenueId::new("polymarket"), "0xabc:NO");
        assert_ne!(yes, no);
    }

    #[test]
    fn round_qty_floors_to_lot_size() {
        let inst = instrument(None, Some(dec!(0.001)), None);
        assert_eq!(inst.round_qty(dec!(1.23456)), dec!(1.234));
        // No lot size configured leaves the quantity untouched.
        assert_eq!(
            instrument(None, None, None).round_qty(dec!(1.23456)),
            dec!(1.23456)
        );
    }

    #[test]
    fn round_price_rounds_toward_the_safe_side() {
        let inst = instrument(Some(dec!(0.01)), None, None);
        // Buying: never round the price up into paying more.
        assert_eq!(inst.round_price(dec!(1.2399), Side::Buy), dec!(1.23));
        // Selling: never round down into receiving less.
        assert_eq!(inst.round_price(dec!(1.2301), Side::Sell), dec!(1.24));
    }

    #[test]
    fn min_notional_gates_small_orders() {
        let inst = instrument(None, None, Some(dec!(10)));
        assert!(!inst.meets_min_notional(dec!(9.99)));
        assert!(inst.meets_min_notional(dec!(10)));
        // Unset means the venue imposes no minimum.
        assert!(instrument(None, None, None).meets_min_notional(dec!(0.01)));
    }

    #[test]
    fn quote_taker_price_and_spread() {
        let q = Quote {
            instrument: InstrumentId::new(VenueId::new("v"), "s"),
            bid: dec!(0.40),
            ask: dec!(0.60),
            mid: dec!(0.50),
            last: None,
            ts: Utc::now(),
            book: None,
        };
        assert_eq!(q.spread(), dec!(0.20));
        assert_eq!(q.spread_pct(), Some(dec!(0.4)));
        assert_eq!(q.taker_price(Side::Buy), dec!(0.60));
        assert_eq!(q.taker_price(Side::Sell), dec!(0.40));
    }

    #[test]
    fn quote_spread_pct_is_none_at_zero_mid() {
        let q = Quote {
            instrument: InstrumentId::new(VenueId::new("v"), "s"),
            bid: Decimal::ZERO,
            ask: Decimal::ZERO,
            mid: Decimal::ZERO,
            last: None,
            ts: Utc::now(),
            book: None,
        };
        assert_eq!(q.spread_pct(), None);
    }

    #[test]
    fn order_state_open_and_terminal_are_exclusive() {
        for state in [
            OrderState::Accepted,
            OrderState::PartiallyFilled,
            OrderState::Filled,
            OrderState::Cancelled,
            OrderState::Expired,
            OrderState::Rejected("x".into()),
            OrderState::Unknown,
        ] {
            assert!(
                !(state.is_open() && state.is_terminal()),
                "{state:?} cannot be both open and terminal"
            );
        }
        // Unknown is deliberately neither: it must be resolved by querying.
        assert!(!OrderState::Unknown.is_open());
        assert!(!OrderState::Unknown.is_terminal());
    }

    #[test]
    fn side_opposite_round_trips() {
        assert_eq!(Side::Buy.opposite(), Side::Sell);
        assert_eq!(Side::Buy.opposite().opposite(), Side::Buy);
    }

    #[test]
    fn only_prediction_markets_settle() {
        assert!(AssetClass::PredictionBinary.settles());
        assert!(!AssetClass::CryptoSpot.settles());
        assert!(!AssetClass::Equity.settles());
    }
}
