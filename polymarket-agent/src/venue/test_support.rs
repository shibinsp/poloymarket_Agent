//! Test-only venue fixtures.
//!
//! `StubVenue` answers the two questions the registry and the scheduler ask —
//! what asset classes do you list, and is your session open — without a
//! network. It lives here rather than in either module's test block because
//! both need it, and two copies of a fixture drift until they disagree about
//! the thing under test.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::session::TradingSession;
use super::types::{
    AssetClass, Balance, Candle, CandleInterval, Instrument, InstrumentId, InstrumentMeta,
    OrderAck, OrderRef, OrderRequest, Position, Quote, ScanFilter, Settlement, VenueCapabilities,
    VenueId,
};
use super::Venue;

/// Minimal venue for exercising the registry.
pub struct StubVenue {
    id: VenueId,
    caps: VenueCapabilities,
    session: TradingSession,
    instruments: Vec<Instrument>,
    fail: bool,
    /// Mid price returned by `quote`, when one is configured.
    mark: Option<Decimal>,
    /// What `place_order` answers with. Lets a test drive the accepted,
    /// partially-filled and filled paths, which are handled very differently.
    ack: Option<OrderAck>,
    /// Every order this venue was asked to place, so a test can assert that a
    /// second one was never sent.
    pub placed: Mutex<Vec<OrderRequest>>,
    /// How many times `cancel_all` was called. Shutdown, death and halting
    /// all have to clear the book, and "was every venue asked" is the only
    /// thing worth asserting about that.
    ///
    /// An `Arc` so a test can keep a handle on the counter after the venue
    /// has been boxed into a `VenueRegistry` and is no longer reachable as a
    /// concrete type. The alternative — casting the trait object back — is a
    /// pointer cast that compiles happily after the registry's contents
    /// change and then reads whatever is there.
    cancel_all_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl StubVenue {
    pub fn new(id: &str, session: TradingSession, symbols: &[&str], fail: bool) -> Self {
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
    pub fn with_classes(
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
                min_qty: None,
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
            mark: None,
            ack: None,
            placed: Mutex::new(Vec::new()),
            cancel_all_calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Quote every instrument at this price.
    pub fn quoting(mut self, mark: Decimal) -> Self {
        self.mark = Some(mark);
        self
    }

    /// Answer `place_order` with this ack.
    pub fn acking(mut self, ack: OrderAck) -> Self {
        self.ack = Some(ack);
        self
    }

    pub fn orders_placed(&self) -> usize {
        self.placed.lock().expect("stub mutex").len()
    }
    /// A handle on the cancel counter that outlives boxing into a registry.
    pub fn cancel_all_counter(&self) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        self.cancel_all_calls.clone()
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
    async fn quote(&self, id: &InstrumentId) -> Result<Quote> {
        let Some(mark) = self.mark else {
            anyhow::bail!("not implemented")
        };
        Ok(Quote {
            instrument: id.clone(),
            bid: mark,
            ask: mark,
            mid: mark,
            last: None,
            ts: chrono::Utc::now(),
            book: None,
        })
    }
    async fn candles(
        &self,
        _id: &InstrumentId,
        _i: CandleInterval,
        _l: usize,
    ) -> Result<Vec<Candle>> {
        Ok(Vec::new())
    }
    async fn place_order(&self, r: &OrderRequest) -> Result<OrderAck> {
        self.placed.lock().expect("stub mutex").push(r.clone());
        match &self.ack {
            Some(a) => Ok(a.clone()),
            None => anyhow::bail!("not implemented"),
        }
    }
    async fn get_order(&self, _o: &OrderRef) -> Result<OrderAck> {
        anyhow::bail!("not implemented")
    }
    async fn cancel_order(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    async fn cancel_all(&self) -> Result<()> {
        self.cancel_all_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail {
            anyhow::bail!("{} cannot be reached", self.id);
        }
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
            total: Some(dec!(100)),
        })
    }
    async fn settlement(&self, _id: &InstrumentId) -> Result<Option<Settlement>> {
        Ok(None)
    }
}
