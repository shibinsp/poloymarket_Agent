//! Venue abstraction.
//!
//! One trait covering prediction markets, crypto spot and equities, so the
//! agent loop doesn't special-case exchanges. Modelled on the existing
//! `DataSource` trait (`crate::data`): an async trait, a registry holding
//! boxed implementations, and per-venue error isolation so one broken
//! exchange can't stop the cycle.

pub mod alpaca;
pub mod binance_us;
pub mod coinbase;
pub mod equity;
pub mod factory;
pub mod polymarket;
pub mod preflight;
pub mod session;
#[cfg(test)]
pub mod test_support;
pub mod types;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
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

    /// Spendable cash. **Cheap**: no position pricing, no per-holding quotes.
    ///
    /// Split from equity deliberately. Marking a book costs a quote per
    /// holding at a spot venue, and every caller of this method wants only the
    /// cash figure: `venue_cycle::venue_bankroll`, `lifecycle::current_balance`
    /// — which the survival ladder and `shutdown` both go through — and
    /// `preflight::check`. Folding the two together made `shutdown` block on a
    /// network round trip per holding per venue while trying to exit.
    async fn balance(&self) -> Result<Balance>;

    /// Account value including the marked value of open positions.
    ///
    /// `None` when it cannot be established — which the caller must not
    /// confuse with zero, and must not paper over with cash: entering a
    /// position moves cash out and the position's value is invisible, so an
    /// equity curve built on cash reads every entry as an instant loss of the
    /// full notional.
    ///
    /// Expensive at a venue whose account endpoint reports quantities without
    /// prices, so it is asked for explicitly and once a cycle rather than
    /// falling out of every balance check.
    async fn equity(&self) -> Result<Option<Decimal>>;

    /// Whether the account is *permitted* to trade.
    ///
    /// Distinct from `balance`, which a blocked account answers perfectly
    /// well: Alpaca returns cash and equity on a `trading_blocked` or
    /// `account_blocked` account and then rejects every order. A PDT
    /// violation or a compliance hold would otherwise pass every startup
    /// check and be discovered one rejected order at a time.
    ///
    /// Defaults to permitted, so a venue with no such concept need not
    /// implement it.
    /// Whether this venue can report account equity at all.
    ///
    /// Not "did it answer this time" — whether it ever can. Every loss limit
    /// is measured against equity summed across the registry, so a venue that
    /// structurally cannot report one makes that sum unknowable for as long as
    /// it is enabled. The registry refuses to build such a venue rather than
    /// let it halt the agent on every cycle.
    fn reports_equity(&self) -> bool {
        true
    }

    async fn trading_readiness(&self) -> Result<()> {
        Ok(())
    }

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

    /// Pull every resting order off every venue.
    ///
    /// Used on three paths that share one requirement: after this returns,
    /// nothing the agent placed may still be working at a venue nobody is
    /// watching. Halting, dying, and being stopped all qualify — an order
    /// left resting through a `systemctl stop` can fill during a deploy, and
    /// the position it opens belongs to nobody until the process comes back.
    ///
    /// Best-effort by necessity: a venue that will not answer cannot be made
    /// to cancel. Failures are logged rather than propagated, and every venue
    /// is asked even after one fails — otherwise a single unreachable venue
    /// would leave every other venue's book untouched, which is the opposite
    /// of what this is for.
    pub async fn cancel_all_resting(&self, why: &str) {
        for venue in self.all() {
            match venue.cancel_all().await {
                Ok(()) => tracing::info!(venue = %venue.id(), why, "Cancelled resting orders"),
                Err(e) => tracing::warn!(
                    venue = %venue.id(),
                    why,
                    error = %e,
                    "Could not cancel resting orders — they may still fill"
                ),
            }
        }
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
///
/// Public because a closed equity session and a 24/7 crypto venue that is
/// somehow reporting closed look identical from `is_open_at` alone, and the
/// dry run has to be able to tell an operator which they are looking at.
pub fn venue_has_work_at(venue: &dyn Venue, at: DateTime<Utc>) -> bool {
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
            min_qty: None,
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

    /// Gate item 1: no orphaned live orders on stop or crash. The shutdown
    /// path, the death path and the halt path all come through here.
    #[tokio::test]
    async fn every_venue_is_asked_to_clear_its_book() {
        use std::sync::atomic::Ordering;

        let a = StubVenue::new("a", TradingSession::Always, &["BTC/USD"], false);
        let b = StubVenue::new("b", TradingSession::Always, &["ETH/USD"], false);
        let (ca, cb) = (a.cancel_all_counter(), b.cancel_all_counter());
        let reg = VenueRegistry::new(vec![Box::new(a), Box::new(b)]);

        reg.cancel_all_resting("shutdown").await;

        assert_eq!(ca.load(Ordering::SeqCst), 1, "venue a was not asked");
        assert_eq!(cb.load(Ordering::SeqCst), 1, "venue b was not asked");
    }

    /// The ordering that matters: a venue that cannot be reached must not
    /// stop the others being cleared. Otherwise one unreachable venue leaves
    /// every other venue's book working through a deploy.
    #[tokio::test]
    async fn a_venue_that_cannot_be_reached_does_not_strand_the_others() {
        use std::sync::atomic::Ordering;

        let broken = StubVenue::new("broken", TradingSession::Always, &["X"], true);
        let working = StubVenue::new("working", TradingSession::Always, &["BTC/USD"], false);
        let (broken_calls, working_calls) =
            (broken.cancel_all_counter(), working.cancel_all_counter());
        let reg = VenueRegistry::new(vec![Box::new(broken), Box::new(working)]);

        reg.cancel_all_resting("shutdown").await;

        assert_eq!(
            broken_calls.load(Ordering::SeqCst),
            1,
            "the unreachable venue is still asked"
        );
        assert_eq!(
            working_calls.load(Ordering::SeqCst),
            1,
            "and the reachable one is still cleared despite the failure"
        );
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

/// What the dashboard needs to answer "which platforms does this agent trade,
/// and is each one actually working".
///
/// Every field here was already computed at startup and then thrown away:
/// `build_registry_reporting` returns why a venue was skipped, and nothing
/// kept it. An operator who enabled Coinbase in paper mode saw it vanish with
/// no trace anywhere in the UI — the reason was one line in the startup log,
/// which is not where anyone looks when a venue is simply absent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VenueStatus {
    pub id: String,
    /// `alpaca`, `coinbase`, `binance_us`, `polymarket`.
    pub kind: String,
    /// What the config asked for.
    pub enabled: bool,
    /// Whether it is actually in the registry and will be traded.
    pub active: bool,
    /// Why not, when `enabled` and `active` disagree.
    pub reason: Option<String>,
    pub symbols: Vec<String>,
    /// Serialized as a string: `rust_decimal`'s `serde-str` is how every
    /// money value crosses this API, and the UI parses them as such.
    pub fee_pct: Decimal,
    pub asset_classes: Vec<String>,
    /// `always` for 24/7 crypto, otherwise the market's session.
    pub session: String,
    /// False for a venue that cannot value its own book — every loss limit is
    /// measured against equity summed across the registry, so this is the
    /// difference between the breakers running and the agent halting.
    pub reports_equity: bool,
    /// Whether the platform has a paper endpoint at all.
    ///
    /// The single most load-bearing fact about a venue here: Alpaca has a
    /// separate paper host, and Coinbase and Binance.US have nothing — so
    /// enabling either means real money, which is why the agent refuses to
    /// build them outside live mode.
    pub paper_trading: bool,
}

/// Whether this venue kind has a paper endpoint.
///
/// Stated once, here, rather than inferred at each call site: Coinbase's
/// sandbox serves auth and serialization with no matching engine, and
/// `testnet.binance.vision` belongs to global Binance, a different exchange
/// that blocks US persons.
pub fn kind_supports_paper(kind: &str) -> bool {
    match kind.to_ascii_lowercase().as_str() {
        "alpaca" => true,
        // In-process paper simulation rather than a venue endpoint.
        "polymarket" => true,
        _ => false,
    }
}

/// Fold the config, the registry and the skip reasons into one answer.
pub fn venue_status(
    configured: &[crate::config::VenueConfig],
    registry: &VenueRegistry,
    skipped: &[crate::venue::factory::SkippedVenue],
) -> Vec<VenueStatus> {
    configured
        .iter()
        .map(|cfg| {
            let built = registry
                .all()
                .find(|v| v.id().as_str().eq_ignore_ascii_case(&cfg.id));
            VenueStatus {
                id: cfg.id.clone(),
                kind: cfg.kind.clone(),
                enabled: cfg.enabled,
                active: built.is_some(),
                reason: skipped
                    .iter()
                    .find(|s| s.id == cfg.id)
                    .map(|s| s.reason.clone())
                    .or_else(|| (!cfg.enabled).then(|| "disabled in the config".to_string())),
                symbols: cfg.symbols.clone(),
                fee_pct: cfg.fee_pct,
                asset_classes: built
                    .map(|v| {
                        v.capabilities()
                            .asset_classes
                            .iter()
                            .map(|c| format!("{c:?}"))
                            .collect()
                    })
                    .unwrap_or_default(),
                session: built
                    .map(|v| format!("{:?}", v.session()))
                    .unwrap_or_else(|| "unknown".to_string()),
                reports_equity: built.map(|v| v.reports_equity()).unwrap_or(false),
                paper_trading: kind_supports_paper(&cfg.kind),
            }
        })
        .collect()
}

#[cfg(test)]
mod venue_status_tests {
    use super::*;
    use crate::config::VenueConfig;
    use crate::venue::factory::SkippedVenue;
    use crate::venue::session::TradingSession;
    use crate::venue::test_support::StubVenue;
    use rust_decimal_macros::dec;

    fn cfg(id: &str, kind: &str, enabled: bool) -> VenueConfig {
        VenueConfig {
            id: id.to_string(),
            kind: kind.to_string(),
            enabled,
            base_url: None,
            data_url: None,
            symbols: vec!["BTC/USD".to_string()],
            fee_pct: dec!(0.0025),
        }
    }

    /// The case the page exists for: enabled, not in the registry, and the
    /// reason is the only record of why. Before this it was one startup log
    /// line and the venue simply did not appear in the UI at all.
    #[test]
    fn an_enabled_but_skipped_venue_carries_its_reason() {
        let registry = VenueRegistry::new(Vec::new());
        let skipped = vec![SkippedVenue {
            id: "coinbase".to_string(),
            reason: "Coinbase has no paper endpoint".to_string(),
        }];

        let status = venue_status(&[cfg("coinbase", "coinbase", true)], &registry, &skipped);

        assert_eq!(status.len(), 1);
        assert!(status[0].enabled, "the config asked for it");
        assert!(!status[0].active, "and it is not trading");
        assert_eq!(
            status[0].reason.as_deref(),
            Some("Coinbase has no paper endpoint")
        );
    }

    #[test]
    fn a_venue_that_built_is_active_and_has_no_reason() {
        let registry = VenueRegistry::new(vec![Box::new(StubVenue::new(
            "alpaca",
            TradingSession::Always,
            &["BTC/USD"],
            false,
        ))]);

        let status = venue_status(&[cfg("alpaca", "alpaca", true)], &registry, &[]);

        assert!(status[0].active);
        assert_eq!(status[0].reason, None);
        assert_eq!(status[0].session, "Always");
        assert!(status[0].reports_equity);
    }

    /// Disabled is not the same as broken, and saying "skipped" for it would
    /// send an operator looking for a fault they chose.
    #[test]
    fn a_disabled_venue_says_so_rather_than_reporting_a_failure() {
        let registry = VenueRegistry::new(Vec::new());
        let status = venue_status(&[cfg("polymarket", "polymarket", false)], &registry, &[]);

        assert!(!status[0].enabled);
        assert_eq!(status[0].reason.as_deref(), Some("disabled in the config"));
    }

    /// The single most load-bearing fact on the page: a platform with no paper
    /// endpoint trades real money the moment it is enabled.
    #[test]
    fn only_the_platforms_with_a_paper_path_report_one() {
        assert!(kind_supports_paper("alpaca"));
        assert!(kind_supports_paper("Alpaca"), "matched case-insensitively");
        assert!(kind_supports_paper("polymarket"), "simulated in process");
        assert!(!kind_supports_paper("coinbase"));
        assert!(!kind_supports_paper("binance_us"));
        assert!(
            !kind_supports_paper("something_new"),
            "absence is not a paper path"
        );
    }
}
