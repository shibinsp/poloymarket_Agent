//! Venue abstraction.
//!
//! One trait covering prediction markets, crypto spot and equities, so the
//! agent loop doesn't special-case exchanges. Modelled on the existing
//! `DataSource` trait (`crate::data`): an async trait, a registry holding
//! boxed implementations, and per-venue error isolation so one broken
//! exchange can't stop the cycle.

pub mod alpaca;
pub mod factory;
pub mod polymarket;
pub mod session;
#[cfg(test)]
pub mod test_support;
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

    /// Earliest instant any *session-closed* venue reopens.
    ///
    /// Not a sleep-until time on its own, despite the obvious reading. It mins
    /// over the closed venues only, so a registry holding a 24/7 crypto venue
    /// beside a shut equity venue still returns Monday's open — sleeping
    /// straight to that would sit out two days of tradeable crypto. Ask
    /// `trades_at` first, which is what the scheduler does.
    ///
    /// `None` when every venue's session is open.
    pub fn next_open_after(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.all()
            .filter_map(|v| match v.session_state(at) {
                SessionState::Closed { next_open } => Some(next_open),
                SessionState::Open(_) => None,
            })
            .min()
    }

    /// Whether any venue could produce a tradeable instrument right now.
    ///
    /// This is deliberately *not* `!open_at(at).is_empty()`: a venue whose
    /// session is shut still trades its always-on classes, so asking about
    /// sessions alone would report "nothing to do" every weekend while the
    /// crypto book is live. The scheduler sleeps on this answer, which is the
    /// most expensive place to get it wrong.
    pub fn trades_at(&self, at: DateTime<Utc>) -> bool {
        self.all().any(|v| venue_has_work_at(v, at))
    }

    /// Instruments that can be traded right now, across every venue.
    ///
    /// Filtering is per *instrument*, not per venue. A venue serving both
    /// equities and crypto reports the restrictive equity session, so
    /// filtering by venue would stop crypto trading every weekend — the exact
    /// opposite of what a 24/7 asset needs. Only session-bound instruments are
    /// gated by the venue's session.
    ///
    /// Failures are isolated the way `DataAggregator::fetch_all` does: one
    /// unreachable exchange degrades the cycle instead of ending it.
    pub async fn list_tradeable_instruments(
        &self,
        at: DateTime<Utc>,
        filter: &ScanFilter,
    ) -> Vec<Instrument> {
        let mut out = Vec::new();
        for venue in self.all() {
            let session_open = venue.is_open_at(at);
            // Skip the call entirely only if nothing this venue lists could be
            // tradeable — i.e. it is closed and serves session-bound assets only.
            if !venue_has_work_at(venue, at) {
                continue;
            }

            match venue.list_instruments(filter).await {
                Ok(instruments) => out.extend(
                    instruments
                        .into_iter()
                        .filter(|i| instrument_tradeable(i, session_open)),
                ),
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

/// Whether a venue can serve a tradeable instrument at `at` — either its
/// session is open, or it lists a class that ignores sessions entirely.
fn venue_has_work_at(venue: &dyn Venue, at: DateTime<Utc>) -> bool {
    venue.is_open_at(at) || venue.capabilities().has_always_on()
}

/// Whether an instrument can be traded given its venue's session state.
/// Crypto and prediction markets never close; equities do.
fn instrument_tradeable(instrument: &Instrument, venue_session_open: bool) -> bool {
    instrument.asset_class.never_closes() || venue_session_open
}

#[cfg(test)]
mod tests {
    use super::types::{AssetClass, InstrumentMeta};
    use super::*;
    use chrono::TimeZone;

    use super::test_support::StubVenue;

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
            Box::new(StubVenue::with_classes(
                "equity",
                TradingSession::us_equity_regular(),
                &[("AAPL", AssetClass::Equity)],
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
    async fn equities_are_gated_by_session_but_crypto_is_not() {
        let reg = registry();
        let filter = ScanFilter::default();
        let saturday = reg
            .list_tradeable_instruments(et(2026, 9, 19, 12), &filter)
            .await;
        assert_eq!(saturday.len(), 1, "equities are shut at the weekend");
        assert_eq!(saturday[0].symbol(), "BTC/USD");

        let weekday = reg
            .list_tradeable_instruments(et(2026, 9, 17, 12), &filter)
            .await;
        assert_eq!(weekday.len(), 2);
    }

    #[tokio::test]
    async fn a_mixed_venue_still_trades_crypto_while_its_equity_session_is_shut() {
        // Alpaca's real shape: one venue, both asset classes, reporting the
        // restrictive equity session. Filtering by venue would silently stop
        // crypto trading every weekend — the opposite of a 24/7 asset's needs.
        let reg = VenueRegistry::new(vec![Box::new(StubVenue::with_classes(
            "alpaca",
            TradingSession::us_equity_regular(),
            &[
                ("AAPL", AssetClass::Equity),
                ("BTC/USD", AssetClass::CryptoSpot),
            ],
            false,
        ))]);

        // Saturday: the venue reports closed, but crypto must still come back.
        let saturday = reg
            .list_tradeable_instruments(et(2026, 9, 19, 12), &ScanFilter::default())
            .await;
        assert_eq!(saturday.len(), 1);
        assert_eq!(saturday[0].symbol(), "BTC/USD");

        // Thursday midday: both are tradeable.
        let weekday = reg
            .list_tradeable_instruments(et(2026, 9, 17, 12), &ScanFilter::default())
            .await;
        assert_eq!(weekday.len(), 2);
    }

    #[test]
    fn instrument_tradeability_follows_asset_class() {
        let equity = Instrument {
            id: InstrumentId::new(VenueId::new("v"), "AAPL"),
            asset_class: AssetClass::Equity,
            display_name: "AAPL".to_string(),
            quote_ccy: "USD".to_string(),
            tick_size: None,
            lot_size: None,
            min_notional: None,
            fractional: true,
            meta: InstrumentMeta::Equity {
                exchange: "NASDAQ".to_string(),
            },
        };
        let crypto = Instrument {
            asset_class: AssetClass::CryptoSpot,
            ..equity.clone()
        };

        assert!(instrument_tradeable(&equity, true));
        assert!(!instrument_tradeable(&equity, false));
        // Crypto ignores the venue's session entirely.
        assert!(instrument_tradeable(&crypto, true));
        assert!(instrument_tradeable(&crypto, false));
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
            .list_tradeable_instruments(Utc::now(), &ScanFilter::default())
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
