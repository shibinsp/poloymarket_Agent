//! Venue abstraction.
//!
//! One trait covering prediction markets, crypto spot and equities, so the
//! agent loop doesn't special-case exchanges. Modelled on the existing
//! `DataSource` trait (`crate::data`): an async trait, a registry holding
//! boxed implementations, and per-venue error isolation so one broken
//! exchange can't stop the cycle.

pub mod alpaca;
pub mod polymarket;
pub mod session;
pub mod types;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tracing::warn;

use self::session::{SessionState, TradingSession};
use self::types::{
    Balance, Candle, CandleInterval, Instrument, InstrumentId, OrderAck, OrderRef, OrderRequest,
    Position, Quote, ScanFilter, Settlement, VenueCapabilities, VenueId,
};

/// A trading venue.
///
/// Implementations wrap one exchange's API and translate it into the
/// venue-agnostic types. They are expected to be cheap to clone-by-reference
/// (hold an `Arc` internally if needed) and safe to call concurrently.
#[async_trait]
pub trait Venue: Send + Sync {
    fn id(&self) -> &VenueId;

    fn capabilities(&self) -> &VenueCapabilities;

    /// When this venue accepts orders. Crypto and prediction markets return
    /// `TradingSession::Always`.
    fn session(&self) -> &TradingSession;

    /// Discover tradeable instruments matching the filter.
    async fn list_instruments(&self, filter: &ScanFilter) -> Result<Vec<Instrument>>;

    /// Current top of book (plus depth where available).
    async fn quote(&self, id: &InstrumentId) -> Result<Quote>;

    /// Historical bars, for volatility-based sizing and prompt context.
    /// Venues without bar data return an empty vec rather than erroring.
    async fn candles(
        &self,
        id: &InstrumentId,
        interval: CandleInterval,
        limit: usize,
    ) -> Result<Vec<Candle>>;

    /// Submit an order. Implementations must send `client_order_id` where the
    /// venue supports it, so a retry after a timeout cannot double-place.
    async fn place_order(&self, request: &OrderRequest) -> Result<OrderAck>;

    /// Current state of a previously submitted order. This is how an
    /// `OrderState::Unknown` gets resolved before any retry.
    async fn get_order(&self, order: &OrderRef) -> Result<OrderAck>;

    async fn cancel_order(&self, venue_order_id: &str) -> Result<()>;

    /// Cancel everything outstanding — used by the kill switch.
    async fn cancel_all(&self) -> Result<()>;

    async fn open_orders(&self) -> Result<Vec<OrderAck>>;

    /// Positions as the venue sees them — the source of truth for
    /// reconciliation against local records.
    async fn positions(&self) -> Result<Vec<Position>>;

    async fn balance(&self) -> Result<Balance>;

    /// Settlement result for a resolved instrument. `None` while unresolved;
    /// venues whose assets never settle always return `None`.
    async fn settlement(&self, id: &InstrumentId) -> Result<Option<Settlement>>;

    /// Convenience: session state at an instant.
    fn session_state(&self, at: DateTime<Utc>) -> SessionState {
        self.session().state_at(at)
    }

    fn is_open_at(&self, at: DateTime<Utc>) -> bool {
        self.session().is_open_at(at)
    }
}

/// The set of venues the agent trades. Mirrors `DataAggregator`: iterate all,
/// log and skip failures rather than aborting the cycle.
pub struct VenueRegistry {
    venues: Vec<Box<dyn Venue>>,
}

impl VenueRegistry {
    pub fn new(venues: Vec<Box<dyn Venue>>) -> Self {
        Self { venues }
    }

    pub fn is_empty(&self) -> bool {
        self.venues.is_empty()
    }

    pub fn len(&self) -> usize {
        self.venues.len()
    }

    pub fn all(&self) -> impl Iterator<Item = &dyn Venue> {
        self.venues.iter().map(|v| v.as_ref())
    }

    pub fn get(&self, id: &VenueId) -> Option<&dyn Venue> {
        self.venues
            .iter()
            .find(|v| v.id() == id)
            .map(|v| v.as_ref())
    }

    /// Venues currently accepting orders — what the scanner should look at.
    pub fn open_at(&self, at: DateTime<Utc>) -> Vec<&dyn Venue> {
        self.all().filter(|v| v.is_open_at(at)).collect()
    }

    /// Earliest instant any closed venue reopens, for sleep-until-open.
    /// `None` when every venue is already open.
    pub fn next_open_after(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.all()
            .filter_map(|v| match v.session_state(at) {
                SessionState::Closed { next_open } => Some(next_open),
                SessionState::Open(_) => None,
            })
            .min()
    }

    /// Collect instruments across every open venue, isolating failures the way
    /// `DataAggregator::fetch_all` does — one unreachable exchange degrades the
    /// cycle instead of ending it.
    pub async fn list_all_instruments(
        &self,
        at: DateTime<Utc>,
        filter: &ScanFilter,
    ) -> Vec<Instrument> {
        let mut out = Vec::new();
        for venue in self.open_at(at) {
            match venue.list_instruments(filter).await {
                Ok(instruments) => out.extend(instruments),
                Err(e) => warn!(
                    venue = %venue.id(),
                    error = %e,
                    "Instrument discovery failed — skipping this venue for this cycle"
                ),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::types::{AssetClass, InstrumentMeta};
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    /// Minimal venue for exercising the registry.
    struct StubVenue {
        id: VenueId,
        caps: VenueCapabilities,
        session: TradingSession,
        instruments: Vec<Instrument>,
        fail: bool,
    }

    impl StubVenue {
        fn new(id: &str, session: TradingSession, symbols: &[&str], fail: bool) -> Self {
            let venue_id = VenueId::new(id);
            let instruments = symbols
                .iter()
                .map(|s| Instrument {
                    id: InstrumentId::new(venue_id.clone(), *s),
                    asset_class: AssetClass::CryptoSpot,
                    display_name: s.to_string(),
                    quote_ccy: "USD".to_string(),
                    tick_size: None,
                    lot_size: None,
                    min_notional: None,
                    fractional: true,
                    meta: InstrumentMeta::Spot {
                        base: s.to_string(),
                    },
                })
                .collect();
            Self {
                id: venue_id,
                caps: VenueCapabilities {
                    asset_classes: vec![AssetClass::CryptoSpot],
                    limit_only_outside_regular: false,
                    supports_client_order_id: true,
                    supports_candles: false,
                },
                session,
                instruments,
                fail,
            }
        }
    }

    #[async_trait]
    impl Venue for StubVenue {
        fn id(&self) -> &VenueId {
            &self.id
        }
        fn capabilities(&self) -> &VenueCapabilities {
            &self.caps
        }
        fn session(&self) -> &TradingSession {
            &self.session
        }
        async fn list_instruments(&self, _f: &ScanFilter) -> Result<Vec<Instrument>> {
            if self.fail {
                anyhow::bail!("venue unreachable");
            }
            Ok(self.instruments.clone())
        }
        async fn quote(&self, _id: &InstrumentId) -> Result<Quote> {
            anyhow::bail!("not implemented")
        }
        async fn candles(
            &self,
            _id: &InstrumentId,
            _i: CandleInterval,
            _l: usize,
        ) -> Result<Vec<Candle>> {
            Ok(Vec::new())
        }
        async fn place_order(&self, _r: &OrderRequest) -> Result<OrderAck> {
            anyhow::bail!("not implemented")
        }
        async fn get_order(&self, _o: &OrderRef) -> Result<OrderAck> {
            anyhow::bail!("not implemented")
        }
        async fn cancel_order(&self, _id: &str) -> Result<()> {
            Ok(())
        }
        async fn cancel_all(&self) -> Result<()> {
            Ok(())
        }
        async fn open_orders(&self) -> Result<Vec<OrderAck>> {
            Ok(Vec::new())
        }
        async fn positions(&self) -> Result<Vec<Position>> {
            Ok(Vec::new())
        }
        async fn balance(&self) -> Result<Balance> {
            Ok(Balance {
                ccy: "USD".to_string(),
                available: dec!(100),
                total: dec!(100),
            })
        }
        async fn settlement(&self, _id: &InstrumentId) -> Result<Option<Settlement>> {
            Ok(None)
        }
    }

    fn et(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        chrono_tz::America::New_York
            .with_ymd_and_hms(y, m, d, h, 0, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn registry() -> VenueRegistry {
        VenueRegistry::new(vec![
            Box::new(StubVenue::new(
                "crypto",
                TradingSession::Always,
                &["BTC/USD"],
                false,
            )),
            Box::new(StubVenue::new(
                "equity",
                TradingSession::us_equity_regular(),
                &["AAPL"],
                false,
            )),
        ])
    }

    #[test]
    fn open_at_filters_by_session() {
        let reg = registry();
        // Saturday: crypto only.
        let open = reg.open_at(et(2026, 9, 19, 12));
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id().as_str(), "crypto");

        // Thursday midday: both.
        assert_eq!(reg.open_at(et(2026, 9, 17, 12)).len(), 2);
    }

    #[test]
    fn next_open_after_reports_the_closed_venue() {
        let reg = registry();
        let saturday = et(2026, 9, 19, 12);
        let next = reg.next_open_after(saturday).expect("equity venue is shut");
        assert_eq!(next, et(2026, 9, 21, 9) + chrono::Duration::minutes(30));

        // When everything is open there is nothing to wait for.
        assert_eq!(reg.next_open_after(et(2026, 9, 17, 12)), None);
    }

    #[tokio::test]
    async fn list_all_instruments_only_covers_open_venues() {
        let reg = registry();
        let filter = ScanFilter::default();
        let saturday = reg.list_all_instruments(et(2026, 9, 19, 12), &filter).await;
        assert_eq!(saturday.len(), 1, "equities are shut at the weekend");
        assert_eq!(saturday[0].symbol(), "BTC/USD");

        let weekday = reg.list_all_instruments(et(2026, 9, 17, 12), &filter).await;
        assert_eq!(weekday.len(), 2);
    }

    #[tokio::test]
    async fn one_failing_venue_does_not_stop_the_others() {
        let reg = VenueRegistry::new(vec![
            Box::new(StubVenue::new(
                "broken",
                TradingSession::Always,
                &["X"],
                true,
            )),
            Box::new(StubVenue::new(
                "working",
                TradingSession::Always,
                &["BTC/USD"],
                false,
            )),
        ]);
        let found = reg
            .list_all_instruments(Utc::now(), &ScanFilter::default())
            .await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].venue().as_str(), "working");
    }

    #[test]
    fn get_finds_venue_by_id() {
        let reg = registry();
        assert!(reg.get(&VenueId::new("crypto")).is_some());
        assert!(reg.get(&VenueId::new("nope")).is_none());
        assert_eq!(reg.len(), 2);
        assert!(!reg.is_empty());
    }
}
