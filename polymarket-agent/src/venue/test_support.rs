//! Test-only venue fixtures.
//!
//! `StubVenue` answers the two questions the registry and the scheduler ask —
//! what asset classes do you list, and is your session open — without a
//! network. It lives here rather than in either module's test block because
//! both need it, and two copies of a fixture drift until they disagree about
//! the thing under test.

use anyhow::Result;
use async_trait::async_trait;
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
            total: Some(dec!(100)),
        })
    }
    async fn settlement(&self, _id: &InstrumentId) -> Result<Option<Settlement>> {
        Ok(None)
    }
}
