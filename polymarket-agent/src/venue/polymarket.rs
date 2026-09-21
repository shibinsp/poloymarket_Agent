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
use crate::market::models::{Market, OrderBookSnapshot};
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
    let instruments = build_instruments(venue_id, market);

    // A symbol has to identify exactly one CLOB token, so anything that does
    // not come out as one YES and one NO is not traded at all. Emitting a
    // partial or duplicated set would put two different tokens behind the same
    // symbol, and `resolve` would pick whichever happened to come first.
    let yes = instruments.iter().filter(|i| has_suffix(i, YES)).count();
    let no = instruments.iter().filter(|i| has_suffix(i, NO)).count();
    if yes != 1 || no != 1 {
        warn!(
            question = %market.question,
            yes,
            no,
            "Skipping market that does not resolve to exactly one YES and one NO instrument"
        );
        return Vec::new();
    }

    instruments
}

fn has_suffix(instrument: &Instrument, suffix: &str) -> bool {
    instrument
        .id
        .symbol
        .rsplit(':')
        .next()
        .is_some_and(|s| s == suffix)
}

fn build_instruments(venue_id: &VenueId, market: &Market) -> Vec<Instrument> {
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
            let Some(outcome) = normalise_outcome(&token.outcome) else {
                warn!(
                    question = %market.question,
                    outcome = %token.outcome,
                    "Skipping market whose outcomes are not binary Yes/No"
                );
                return None;
            };
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
                min_qty: None,
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

        // One market, fetched by id. The previous implementation paged the
        // whole discovery universe on every quote — up to ten round trips per
        // instrument per cycle — and then could not find the one market that
        // mattered, because discovery filters out anything closed, past its
        // end date, or below the top-N-by-volume cut. A held position hits all
        // three of those exactly when it needs exiting.
        let market = self
            .client
            .get_market_by_condition_id(condition_id)
            .await
            .with_context(|| format!("Failed to resolve instrument {id}"))?
            .with_context(|| format!("Market {condition_id} not found on Polymarket"))?;

        self.instruments_for(&market)
            .into_iter()
            .find(|i| {
                Self::outcome_of(&i.id)
                    .map(|o| o == outcome)
                    .unwrap_or(false)
            })
            .with_context(|| format!("Instrument {id} not found on Polymarket"))
    }
}

/// Best bid and ask, refusing a book that is missing a side.
///
/// The previous code defaulted the missing side to 0 or 1, which made a
/// one-sided book look like a well-formed quote: the taker price for a buy came
/// out at $1.00 — the most a binary contract can possibly cost, i.e. a
/// guaranteed total loss — and the spread read as a plausible-looking 200%.
/// `OrderBookSnapshot::midpoint` is computed from the same two fallbacks, so it
/// was wrong in exactly the same way; callers derive mid from these checked
/// prices instead. The Alpaca adapter already refuses this state.
fn two_sided_prices(book: &OrderBookSnapshot, id: &InstrumentId) -> Result<(Decimal, Decimal)> {
    let bid = book
        .bids
        .first()
        .map(|l| l.price)
        .with_context(|| format!("One-sided book for {id}: no bids"))?;
    let ask = book
        .asks
        .first()
        .map(|l| l.price)
        .with_context(|| format!("One-sided book for {id}: no asks"))?;
    Ok((bid, ask))
}

/// Map a venue outcome label onto the canonical suffix used in symbols.
///
/// `None` for anything that is not a recognisable binary outcome. Folding
/// every non-"yes" label to NO gave several tokens the same symbol with
/// different CLOB token ids, so `resolve` returned whichever came first and
/// the agent quoted, traded and settled against an outcome it never chose.
fn normalise_outcome(outcome: &str) -> Option<&'static str> {
    let outcome = outcome.trim();
    if outcome.eq_ignore_ascii_case("yes") {
        Some(YES)
    } else if outcome.eq_ignore_ascii_case("no") {
        Some(NO)
    } else {
        None
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

        let (bid, ask) = two_sided_prices(&book, id)?;

        Ok(Quote {
            instrument: id.clone(),
            bid,
            ask,
            mid: (bid + ask) / dec!(2),
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

    /// `positions` is unimplemented here, so there is no way to value what is
    /// held and `equity()` is always `None`. Saying so lets the
    /// registry refuse this venue instead of halting every cycle over an
    /// equity figure it was never going to get.
    fn reports_equity(&self) -> bool {
        false
    }

    async fn positions(&self) -> Result<Vec<Position>> {
        bail!("Polymarket position listing is not implemented")
    }

    /// Always `None`: `positions` is unimplemented here, so there is no way to
    /// value what is held. `reports_equity` says as much, and the registry
    /// refuses to build this venue rather than let it halt every cycle.
    async fn equity(&self) -> Result<Option<Decimal>> {
        Ok(None)
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

    fn book(bids: &[(Decimal, Decimal)], asks: &[(Decimal, Decimal)]) -> OrderBookSnapshot {
        let level = |(price, size): &(Decimal, Decimal)| crate::market::models::PriceLevel {
            price: *price,
            size: *size,
        };
        OrderBookSnapshot {
            token_id: "tok".to_string(),
            bids: bids.iter().map(level).collect(),
            asks: asks.iter().map(level).collect(),
            // Deliberately the values convert_order_book would fabricate for a
            // one-sided book, to prove nothing downstream reads them.
            spread: Decimal::ONE,
            midpoint: dec!(0.5),
            implied_probability: dec!(0.5),
            timestamp: Utc::now(),
        }
    }

    fn instrument_id() -> InstrumentId {
        InstrumentId::new(venue_id(), "0xabc:YES")
    }

    #[test]
    fn a_two_sided_book_quotes_its_own_prices() {
        let b = book(&[(dec!(0.58), dec!(100))], &[(dec!(0.62), dec!(100))]);
        let (bid, ask) = two_sided_prices(&b, &instrument_id()).unwrap();
        assert_eq!(bid, dec!(0.58));
        assert_eq!(ask, dec!(0.62));
        // Mid comes from the checked prices, not the snapshot's own field.
        assert_eq!((bid + ask) / dec!(2), dec!(0.60));
    }

    /// A missing ask used to default to $1.00 — the most a binary contract can
    /// cost, so a buy at the taker price was a guaranteed total loss that the
    /// quote presented as ordinary.
    #[test]
    fn a_book_with_no_asks_is_refused_rather_than_priced_at_one() {
        let b = book(&[(dec!(0.58), dec!(100))], &[]);
        let err = two_sided_prices(&b, &instrument_id()).expect_err("no asks");
        assert!(err.to_string().contains("no asks"), "{err}");
    }

    #[test]
    fn a_book_with_no_bids_is_refused_rather_than_priced_at_zero() {
        let b = book(&[], &[(dec!(0.62), dec!(100))]);
        let err = two_sided_prices(&b, &instrument_id()).expect_err("no bids");
        assert!(err.to_string().contains("no bids"), "{err}");
    }

    #[test]
    fn an_empty_book_is_refused() {
        let b = book(&[], &[]);
        assert!(two_sided_prices(&b, &instrument_id()).is_err());
    }

    #[test]
    fn outcome_labels_normalise_to_canonical_suffixes() {
        assert_eq!(normalise_outcome("Yes"), Some("YES"));
        assert_eq!(normalise_outcome("YES"), Some("YES"));
        assert_eq!(normalise_outcome("No"), Some("NO"));
        assert_eq!(normalise_outcome(" no "), Some("NO"));
    }

    /// Anything that is not a binary Yes/No has no canonical suffix. Folding
    /// it to NO gave two tokens the same symbol and different CLOB token ids,
    /// so a lookup returned whichever came first.
    #[test]
    fn a_non_binary_outcome_has_no_canonical_suffix() {
        for label in ["Nope", "Up", "Down", "Maybe", ""] {
            assert_eq!(normalise_outcome(label), None, "label {label:?}");
        }
    }

    /// The consequence that made it critical: symbols must uniquely identify a
    /// token, so a market that cannot produce distinct Yes/No symbols is not
    /// traded at all rather than traded against the wrong outcome.
    #[test]
    fn a_market_with_non_binary_outcomes_yields_no_instruments() {
        let mut m = market();
        m.tokens[0].outcome = "Up".to_string();
        m.tokens[1].outcome = "Down".to_string();

        assert!(instruments_for_market(&venue_id(), &m).is_empty());
    }

    /// A market that lists the same outcome twice would also collide.
    #[test]
    fn a_market_that_cannot_produce_both_sides_yields_no_instruments() {
        let mut m = market();
        m.tokens[1].outcome = "Yes".to_string();

        assert!(instruments_for_market(&venue_id(), &m).is_empty());
    }

    #[test]
    fn a_binary_market_yields_exactly_one_instrument_per_outcome() {
        let instruments = instruments_for_market(&venue_id(), &market());
        let mut symbols: Vec<&str> = instruments.iter().map(|i| i.id.symbol.as_str()).collect();
        symbols.sort_unstable();
        let distinct = symbols.len();
        symbols.dedup();

        assert_eq!(instruments.len(), 2);
        assert_eq!(symbols.len(), distinct, "instrument symbols must be unique");
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
