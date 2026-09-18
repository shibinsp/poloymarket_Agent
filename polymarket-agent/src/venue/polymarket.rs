//! Polymarket as a `Venue`.
//!
//! Wraps the existing `PolymarketClient` by composition rather than rewriting
//! it. The substantive change is the instrument model: each market becomes
//! *two* instruments (`{condition_id}:YES` and `{condition_id}:NO`), each with
//! its own CLOB token id and its own order book. Buying NO is therefore
//! `Side::Buy` on the NO instrument — no price complement, no side inversion.
//!
//! Some trait methods are genuinely unsupported by the underlying client
//! (order status lookup, open-order listing, venue-side positions). They
//! return errors saying so rather than a plausible-looking wrong answer,
//! because a reconciliation routine that silently sees "no open orders" is
//! worse than one that fails loudly. They land with the order-lifecycle work.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::warn;

use crate::execution::resolution::fetch_market_resolution;
use crate::market::models::Market;
use crate::market::polymarket::{MarketFilters, PolymarketClient};
use crate::venue::session::TradingSession;
use crate::venue::types::{
    AssetClass, Balance, Candle, CandleInterval, Instrument, InstrumentId, InstrumentMeta,
    OrderAck, OrderKind, OrderRef, OrderRequest, Position, Quote, ScanFilter, Settlement,
    VenueCapabilities, VenueId,
};
use crate::venue::Venue;

/// Outcome suffix used to build an instrument symbol from a condition id.
const YES: &str = "YES";
const NO: &str = "NO";

pub struct PolymarketVenue {
    id: VenueId,
    client: Arc<PolymarketClient>,
    capabilities: VenueCapabilities,
    session: TradingSession,
}

impl PolymarketVenue {
    pub fn new(client: Arc<PolymarketClient>) -> Self {
        Self {
            id: VenueId::new("polymarket"),
            client,
            capabilities: VenueCapabilities {
                asset_classes: vec![AssetClass::PredictionBinary],
                limit_only_outside_regular: false,
                // The CLOB has no caller-supplied order id, so idempotency has
                // to come from the caller checking before retrying.
                supports_client_order_id: false,
                supports_candles: false,
            },
            // Prediction markets never close; individual markets simply resolve.
            session: TradingSession::Always,
        }
    }

    /// Split one market into its tradeable outcome instruments.
    fn instruments_for(&self, market: &Market) -> Vec<Instrument> {
        instruments_for_market(&self.id, market)
    }
}

/// Split one market into its tradeable outcome instruments.
///
/// Free function so the mapping can be tested without constructing a live
/// client.
fn instruments_for_market(venue_id: &VenueId, market: &Market) -> Vec<Instrument> {
    market
        .tokens
        .iter()
        .filter_map(|token| {
            // A market with no condition id would produce colliding
            // instrument ids, so skip it rather than trade blind.
            if market.condition_id.is_empty() || token.token_id.is_empty() {
                warn!(
                    question = %market.question,
                    "Skipping market with empty condition or token id"
                );
                return None;
            }
            let outcome = normalise_outcome(&token.outcome);
            Some(Instrument {
                id: InstrumentId::new(
                    venue_id.clone(),
                    format!("{}:{}", market.condition_id, outcome),
                ),
                asset_class: AssetClass::PredictionBinary,
                display_name: format!("{} [{}]", market.question, outcome),
                quote_ccy: "USDC".to_string(),
                // The CLOB prices in cents and sizes in whole shares.
                tick_size: Some(dec!(0.01)),
                lot_size: None,
                min_notional: None,
                fractional: false,
                meta: InstrumentMeta::Prediction {
                    condition_id: market.condition_id.clone(),
                    outcome: token.outcome.clone(),
                    token_id: token.token_id.clone(),
                    question: market.question.clone(),
                    end_date: market.end_date,
                },
            })
        })
        .collect()
}

impl PolymarketVenue {
    fn token_id_of(instrument: &Instrument) -> Result<&str> {
        match &instrument.meta {
            InstrumentMeta::Prediction { token_id, .. } => Ok(token_id),
            other => bail!(
                "Instrument {} is not a Polymarket outcome: {other:?}",
                instrument.id
            ),
        }
    }

    fn condition_id_of(id: &InstrumentId) -> Result<&str> {
        id.symbol
            .split(':')
            .next()
            .filter(|c| !c.is_empty())
            .with_context(|| format!("Malformed Polymarket instrument symbol: {}", id.symbol))
    }

    fn outcome_of(id: &InstrumentId) -> Result<&str> {
        id.symbol
            .split(':')
            .nth(1)
            .filter(|o| !o.is_empty())
            .with_context(|| format!("Malformed Polymarket instrument symbol: {}", id.symbol))
    }

    /// Find an instrument by id, since quoting needs its CLOB token id and the
    /// id alone doesn't carry one.
    async fn resolve(&self, id: &InstrumentId) -> Result<Instrument> {
        let condition_id = Self::condition_id_of(id)?;
        let outcome = Self::outcome_of(id)?;
        let markets = self
            .client
            .get_markets(&MarketFilters {
                min_volume_24h: Decimal::ZERO,
                // Wide enough to cover anything currently held.
                max_resolution_days: 365,
                max_markets: 1000,
                max_spread_pct: Decimal::ONE,
            })
            .await
            .context("Failed to list markets while resolving instrument")?;

        markets
            .iter()
            .find(|m| m.condition_id == condition_id)
            .map(|m| self.instruments_for(m))
            .unwrap_or_default()
            .into_iter()
            .find(|i| {
                Self::outcome_of(&i.id)
                    .map(|o| o == outcome)
                    .unwrap_or(false)
            })
            .with_context(|| format!("Instrument {id} not found on Polymarket"))
    }
}

/// Map a venue outcome label onto the canonical suffix used in symbols.
fn normalise_outcome(outcome: &str) -> &'static str {
    if outcome.eq_ignore_ascii_case("yes") {
        YES
    } else {
        NO
    }
}

#[async_trait]
impl Venue for PolymarketVenue {
    fn id(&self) -> &VenueId {
        &self.id
    }

    fn capabilities(&self) -> &VenueCapabilities {
        &self.capabilities
    }

    fn session(&self) -> &TradingSession {
        &self.session
    }

    async fn list_instruments(&self, filter: &ScanFilter) -> Result<Vec<Instrument>> {
        let markets = self
            .client
            .get_markets(&MarketFilters {
                min_volume_24h: filter.min_volume_24h.unwrap_or(Decimal::ZERO),
                max_resolution_days: filter.max_days_to_resolution.unwrap_or(14),
                max_markets: filter.max_results.unwrap_or(1000),
                max_spread_pct: Decimal::ONE,
            })
            .await
            .context("Failed to fetch Polymarket markets")?;

        Ok(markets
            .iter()
            .flat_map(|m| self.instruments_for(m))
            .collect())
    }

    async fn quote(&self, id: &InstrumentId) -> Result<Quote> {
        let instrument = self.resolve(id).await?;
        let token_id = Self::token_id_of(&instrument)?;
        // Each outcome token has its own book, so the NO side is quoted
        // directly rather than inferred as 1 - YES.
        let book = self
            .client
            .get_order_book(token_id)
            .await
            .with_context(|| format!("Failed to fetch order book for {id}"))?;

        let bid = book.bids.first().map(|l| l.price).unwrap_or(Decimal::ZERO);
        let ask = book.asks.first().map(|l| l.price).unwrap_or(Decimal::ONE);

        Ok(Quote {
            instrument: id.clone(),
            bid,
            ask,
            mid: book.midpoint,
            last: None,
            ts: book.timestamp,
            book: Some(book),
        })
    }

    async fn candles(
        &self,
        _id: &InstrumentId,
        _interval: CandleInterval,
        _limit: usize,
    ) -> Result<Vec<Candle>> {
        // The CLOB exposes a price history, but as bare (timestamp, price)
        // points. Synthesising OHLCV from those would invent highs and lows
        // that never traded, so report no bars instead — `capabilities()`
        // advertises this.
        Ok(Vec::new())
    }

    async fn place_order(&self, request: &OrderRequest) -> Result<OrderAck> {
        let token_id = Self::token_id_of(&request.instrument)?;
        let price = match request.kind {
            OrderKind::Limit { price } => price,
            OrderKind::Market => bail!("Polymarket orders are limit-only"),
        };

        let outcome = self
            .client
            .place_token_order(token_id, request.side, price, request.qty)
            .await
            .with_context(|| format!("Failed to place order on {}", request.instrument.id))?;

        Ok(OrderAck {
            venue_order_id: outcome.order_id,
            client_order_id: request.client_order_id.clone(),
            state: outcome.state,
            filled_qty: outcome.filled_qty,
            avg_fill_price: outcome.avg_fill_price,
            fees: Decimal::ZERO,
        })
    }

    async fn get_order(&self, _order: &OrderRef) -> Result<OrderAck> {
        bail!(
            "Polymarket order status lookup is not implemented — an order's fate \
             cannot be confirmed yet, so callers must not assume a fill"
        )
    }

    async fn cancel_order(&self, venue_order_id: &str) -> Result<()> {
        self.client
            .cancel_order(venue_order_id)
            .await
            .with_context(|| format!("Failed to cancel order {venue_order_id}"))
    }

    async fn cancel_all(&self) -> Result<()> {
        bail!(
            "Polymarket bulk cancel is not implemented — cancel orders individually \
             by id, or cancel from the Polymarket web interface"
        )
    }

    async fn open_orders(&self) -> Result<Vec<OrderAck>> {
        bail!("Polymarket open-order listing is not implemented")
    }

    async fn positions(&self) -> Result<Vec<Position>> {
        bail!("Polymarket position listing is not implemented")
    }

    async fn balance(&self) -> Result<Balance> {
        let available = self
            .client
            .get_balance()
            .await
            .context("Failed to fetch Polymarket balance")?;
        Ok(Balance {
            ccy: "USDC".to_string(),
            available,
            // Without venue-side position data, total equals free cash.
            total: available,
        })
    }

    async fn settlement(&self, id: &InstrumentId) -> Result<Option<Settlement>> {
        let condition_id = Self::condition_id_of(id)?;
        let outcome = Self::outcome_of(id)?;

        let resolution = fetch_market_resolution(
            self.client.http_client(),
            self.client.gamma_base_url(),
            condition_id,
        )
        .await
        .with_context(|| format!("Failed to fetch resolution for {id}"))?;

        Ok(resolution.map(|r| {
            // Each instrument is one outcome, so it won if its own outcome won.
            let won = if outcome == YES {
                r.yes_won
            } else {
                !r.yes_won
            };
            Settlement {
                won,
                payout_per_unit: if won { Decimal::ONE } else { Decimal::ZERO },
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::models::{MarketCategory, TokenInfo};
    use crate::venue::types::{Side, TimeInForce};
    use chrono::Utc;

    fn market() -> Market {
        Market {
            condition_id: "0xabc".to_string(),
            question: "Will it rain?".to_string(),
            outcomes: vec!["Yes".to_string(), "No".to_string()],
            tokens: vec![
                TokenInfo {
                    token_id: "tok_yes".to_string(),
                    outcome: "Yes".to_string(),
                    price: dec!(0.60),
                },
                TokenInfo {
                    token_id: "tok_no".to_string(),
                    outcome: "No".to_string(),
                    price: dec!(0.40),
                },
            ],
            end_date: Utc::now() + chrono::Duration::days(3),
            category: MarketCategory::Weather,
            volume_24h: dec!(50000),
            active: true,
        }
    }

    fn venue_id() -> VenueId {
        VenueId::new("polymarket")
    }

    #[test]
    fn a_market_becomes_two_instruments_with_distinct_tokens() {
        let instruments = instruments_for_market(&venue_id(), &market());

        assert_eq!(instruments.len(), 2);
        assert_eq!(instruments[0].id.symbol, "0xabc:YES");
        assert_eq!(instruments[1].id.symbol, "0xabc:NO");

        // Each carries its own CLOB token — the NO side is not derived from YES.
        let tokens: Vec<&str> = instruments
            .iter()
            .map(|i| PolymarketVenue::token_id_of(i).unwrap())
            .collect();
        assert_eq!(tokens, vec!["tok_yes", "tok_no"]);
    }

    #[test]
    fn markets_without_ids_are_skipped_not_traded_blind() {
        let mut m = market();
        m.condition_id = String::new();
        assert!(instruments_for_market(&venue_id(), &m).is_empty());
    }

    #[test]
    fn symbol_parsing_round_trips() {
        let id = InstrumentId::new(VenueId::new("polymarket"), "0xabc:NO");
        assert_eq!(PolymarketVenue::condition_id_of(&id).unwrap(), "0xabc");
        assert_eq!(PolymarketVenue::outcome_of(&id).unwrap(), "NO");

        let malformed = InstrumentId::new(VenueId::new("polymarket"), "0xabc");
        assert!(PolymarketVenue::outcome_of(&malformed).is_err());
    }

    #[test]
    fn outcome_labels_normalise_to_canonical_suffixes() {
        assert_eq!(normalise_outcome("Yes"), "YES");
        assert_eq!(normalise_outcome("YES"), "YES");
        assert_eq!(normalise_outcome("No"), "NO");
        // Anything that isn't a yes is treated as the complement.
        assert_eq!(normalise_outcome("Nope"), "NO");
    }

    #[test]
    fn buying_no_is_a_buy_of_the_no_instrument() {
        // The regression this whole model exists to prevent: the request for
        // "buy NO" carries Side::Buy and the NO token, never a Sell.
        let instruments = instruments_for_market(&venue_id(), &market());

        let no = instruments
            .into_iter()
            .find(|i| i.id.symbol.ends_with(":NO"))
            .unwrap();

        let request = OrderRequest {
            instrument: no,
            side: Side::Buy,
            kind: OrderKind::Limit { price: dec!(0.40) },
            qty: dec!(10),
            tif: TimeInForce::Gtc,
            extended_hours: false,
            client_order_id: "cid".to_string(),
        };

        assert_eq!(request.side, Side::Buy);
        assert_eq!(
            PolymarketVenue::token_id_of(&request.instrument).unwrap(),
            "tok_no"
        );
        assert_eq!(request.notional(), Some(dec!(4.0)));
    }
}
