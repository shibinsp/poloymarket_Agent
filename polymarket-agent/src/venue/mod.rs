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
    AssetClass, Balance, Candle, CandleInterval, Instrument, InstrumentId, OrderAck, OrderRef,
    OrderRequest, Position, Quote, ScanFilter, Settlement, VenueCapabilities, VenueId,
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
            let has_always_on = venue.capabilities().supports(AssetClass::CryptoSpot)
                || venue.capabilities().supports(AssetClass::PredictionBinary);
            if !session_open && !has_always_on {
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

/// Whether an instrument can be traded given its venue's session state.
/// Crypto and prediction markets never close; equities do.
fn instrument_tradeable(instrument: &Instrument, venue_session_open: bool) -> bool {
    match instrument.asset_class {
        AssetClass::CryptoSpot | AssetClass::PredictionBinary => true,
        AssetClass::Equity => venue_session_open,
    }
}

#[cfg(test)]
mod tests {
    use super::types::InstrumentMeta;
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
            Self::with_classes(
                id,
                session,
                &symbols
                    .iter()
                    .map(|s| (*s, AssetClass::CryptoSpot))
                    .collect::<Vec<_>>(),
                fail,
            )
        }

        /// A venue whose instruments span asset classes — Alpaca's real shape.
        fn with_classes(
            id: &str,
            session: TradingSession,
            symbols: &[(&str, AssetClass)],
            fail: bool,
        ) -> Self {
            let venue_id = VenueId::new(id);
            let mut classes: Vec<AssetClass> = Vec::new();
            for (_, class) in symbols {
                if !classes.contains(class) {
                    classes.push(*class);
                }
            }
            let instruments = symbols
                .iter()
                .map(|(s, class)| Instrument {
                    id: InstrumentId::new(venue_id.clone(), *s),
                    asset_class: *class,
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
                    asset_classes: classes,
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
