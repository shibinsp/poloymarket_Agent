//! Venue-based trading cycle for continuous assets.
//!
//! This is the loop that trades through the `Venue` abstraction: discover
//! instruments on open venues, quote them, form a directional view, size by
//! volatility, and place an order — recording the order *before* it is
//! submitted so a timed-out request still leaves something to reconcile.
//!
//! Scope is deliberately continuous assets (crypto, equities). Prediction
//! markets still run through the legacy Polymarket loop in `lifecycle.rs`
//! until their valuation path is ported; routing both through one loop before
//! either is proven would make a failure impossible to attribute.

use std::collections::HashSet;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::AppConfig;
use crate::db::store::{OrderRecord, Store, VenueTradeRecord};
use crate::market::models::AgentState;
use crate::risk::sizing::{size_position, SizeInputs};
use crate::valuation::directional::{
    self, build_system_prompt, build_user_prompt, parse_directional_response,
};
use crate::valuation::llm::LlmClient;
use crate::venue::session::SessionKind;
use crate::venue::types::{
    AssetClass, CandleInterval, Instrument, OrderKind, OrderRequest, OrderState, ScanFilter, Side,
    TimeInForce,
};
use crate::venue::{Venue, VenueRegistry};

/// What one pass over the venues did.
#[derive(Debug, Default, PartialEq)]
pub struct CycleOutcome {
    pub instruments_scanned: usize,
    pub views_taken: usize,
    pub orders_placed: usize,
    pub api_cost: Decimal,
    /// Instruments skipped because an earlier order is still unresolved.
    pub skipped_unresolved: usize,
}

pub struct VenueCycle<'a> {
    pub registry: &'a VenueRegistry,
    pub llm: Option<&'a LlmClient>,
    pub store: &'a Store,
    pub config: &'a AppConfig,
}

impl VenueCycle<'_> {
    /// Run one pass. Failures on a single instrument are logged and skipped
    /// rather than aborting the cycle — one bad symbol must not stop the rest.
    pub async fn run(
        &self,
        now: DateTime<Utc>,
        state: AgentState,
        bankroll: Decimal,
        cycle: i64,
    ) -> Result<CycleOutcome> {
        let mut outcome = CycleOutcome::default();

        let Some(llm) = self.llm else {
            warn!("No valuation model configured — venue cycle cannot form views");
            return Ok(outcome);
        };

        // Never place a second order for a symbol whose previous order hasn't
        // been resolved: that is how duplicate positions appear.
        let busy: HashSet<String> = self
            .store
            .get_unresolved_orders()
            .await?
            .into_iter()
            .map(|o| format!("{}:{}", o.venue_id, o.symbol))
            .collect();

        let filter = ScanFilter {
            asset_classes: vec![AssetClass::CryptoSpot, AssetClass::Equity],
            symbols: self.config.venue_symbols(),
            min_volume_24h: None,
            max_days_to_resolution: None,
            max_results: Some(self.config.scanning.max_markets),
        };

        let ctx = CycleContext {
            now,
            state,
            bankroll,
            cycle,
        };
        let instruments = self.registry.list_tradeable_instruments(now, &filter).await;
        outcome.instruments_scanned = instruments.len();

        for instrument in instruments {
            // Prediction markets are handled by the legacy loop.
            if instrument.asset_class == AssetClass::PredictionBinary {
                continue;
            }
            if busy.contains(&instrument.id.key()) {
                outcome.skipped_unresolved += 1;
                continue;
            }

            let Some(venue) = self.registry.get(instrument.venue()) else {
                continue;
            };

            match self
                .evaluate_instrument(venue, llm, &instrument, &ctx)
                .await
            {
                Ok(EvaluationResult { cost, placed, .. }) => {
                    outcome.api_cost += cost;
                    if cost > Decimal::ZERO {
                        outcome.views_taken += 1;
                    }
                    if placed {
                        outcome.orders_placed += 1;
                    }
                }
                Err(e) => warn!(
                    instrument = %instrument.id,
                    error = %e,
                    "Evaluation failed — skipping this instrument"
                ),
            }

            // Respect the per-cycle order cap so one cycle can't open the
            // whole book.
            if outcome.orders_placed >= self.config.max_orders_per_cycle() {
                info!(
                    orders = outcome.orders_placed,
                    "Reached the per-cycle order limit"
                );
                break;
            }
        }

        Ok(outcome)
    }

    async fn evaluate_instrument(
        &self,
        venue: &dyn Venue,
        llm: &LlmClient,
        instrument: &Instrument,
        ctx: &CycleContext,
    ) -> Result<EvaluationResult> {
        let quote = venue.quote(&instrument.id).await?;

        // Enough bars for the ATR window plus its seed bar.
        let wanted = self.config.sizing_continuous.atr_period + 1;
        let candles = venue
            .candles(&instrument.id, CandleInterval::H1, wanted.max(50))
            .await
            .unwrap_or_default();

        let system = build_system_prompt();
        let user = build_user_prompt(instrument, &quote, &candles, &[]);
        let response = llm.complete(&system, &user, Some(ctx.cycle)).await?;
        let cost = response.cost;

        let view = parse_directional_response(&response.text)?;

        let fee_pct = self.config.venue_fee_pct();
        let cost_to_trade =
            directional::round_trip_cost(&quote, fee_pct, self.config.execution.max_slippage_pct);

        if !directional::should_trade(
            &view,
            cost_to_trade,
            self.config.min_p_up(),
            self.config.valuation.min_edge_threshold,
        ) {
            info!(
                instrument = %instrument.id,
                direction = ?view.direction,
                p_up = %view.p_up,
                expected_return = %view.expected_return_pct,
                round_trip_cost = %cost_to_trade,
                "No tradeable edge"
            );
            return Ok(EvaluationResult {
                cost,
                placed: false,
            });
        }

        let price = quote.taker_price(Side::Buy);
        let sizing = size_position(
            &SizeInputs {
                instrument,
                probability: view.p_up,
                price,
                confidence: view.confidence,
                bankroll: ctx.bankroll,
                state: ctx.state,
                candles: &candles,
            },
            &self.config.risk,
            &self.config.sizing_continuous,
        );

        if !sizing.should_trade() {
            info!(
                instrument = %instrument.id,
                reason = sizing.rejection.unwrap_or("unsized"),
                "Position not sized"
            );
            return Ok(EvaluationResult {
                cost,
                placed: false,
            });
        }

        let limit_price = instrument.round_price(price, Side::Buy);
        if limit_price <= Decimal::ZERO {
            warn!(instrument = %instrument.id, "Non-positive limit price — skipping");
            return Ok(EvaluationResult {
                cost,
                placed: false,
            });
        }
        let qty = instrument.round_qty(sizing.position_usd / limit_price);
        if qty <= Decimal::ZERO {
            info!(
                instrument = %instrument.id,
                "Quantity rounds to zero at this price and lot size"
            );
            return Ok(EvaluationResult {
                cost,
                placed: false,
            });
        }

        let extended_hours =
            needs_extended_hours_flag(instrument.asset_class, venue.session_state(ctx.now).kind());

        let client_order_id = Uuid::new_v4().to_string();
        let request = OrderRequest {
            instrument: instrument.clone(),
            side: Side::Buy,
            kind: OrderKind::Limit { price: limit_price },
            qty,
            // Day orders for equities so nothing survives the session
            // unnoticed; crypto has no session to expire against.
            tif: match instrument.asset_class {
                AssetClass::Equity => TimeInForce::Day,
                _ => TimeInForce::Gtc,
            },
            extended_hours,
            client_order_id: client_order_id.clone(),
        };

        // Persist before submitting: if the request times out, this row is the
        // only evidence an order may exist at the venue.
        self.store
            .insert_order(&OrderRecord {
                id: None,
                client_order_id: client_order_id.clone(),
                venue_order_id: None,
                venue_id: instrument.venue().to_string(),
                symbol: instrument.symbol().to_string(),
                side: Side::Buy.to_string(),
                intent: "ENTRY".to_string(),
                trade_id: None,
                limit_price: Some(limit_price.to_string()),
                qty: qty.to_string(),
                filled_qty: "0".to_string(),
                avg_fill_price: None,
                state: "PENDING".to_string(),
                reject_reason: None,
                cycle: Some(ctx.cycle),
                submitted_at: None,
                updated_at: None,
                expires_at: Some(
                    (ctx.now + Duration::seconds(self.config.execution.order_ttl_seconds as i64))
                        .to_rfc3339(),
                ),
            })
            .await?;

        // A failed submit is not a rejection: the venue may have accepted the
        // order before the response was lost. Record UNKNOWN and let
        // reconciliation ask, rather than assuming either way.
        let outcome = match venue.place_order(&request).await {
            Ok(ack) => Ok(ack),
            Err(e) => {
                self.store
                    .update_order_state(
                        &client_order_id,
                        "UNKNOWN",
                        None,
                        "0",
                        None,
                        Some(&e.to_string()),
                    )
                    .await?;
                Err(e)
            }
        };

        let ack = outcome.as_ref().ok();

        if let Some(ack) = ack {
            self.store
                .update_order_state(
                    &client_order_id,
                    order_state_str(&ack.state),
                    Some(&ack.venue_order_id),
                    &ack.filled_qty.to_string(),
                    ack.avg_fill_price.map(|p| p.to_string()).as_deref(),
                    match &ack.state {
                        OrderState::Rejected(reason) => Some(reason.as_str()),
                        _ => None,
                    },
                )
                .await?;
        }

        // Record the thesis now, whatever the order did.
        //
        // An order that fills on a later cycle still needs the stop, target and
        // horizon that justified it, and none of that can be reconstructed
        // after the fact. The row is PENDING until a fill is confirmed, and
        // `get_open_venue_trades` ignores PENDING — so this is a record of
        // intent, not a claim that a position exists.
        let filled_qty = ack.map(|a| a.filled_qty).unwrap_or(Decimal::ZERO);
        let fill_price = ack.and_then(|a| a.avg_fill_price).unwrap_or(limit_price);
        let status = trade_status_for(ack.map(|a| &a.state), filled_qty, qty);
        let trade_id = self
            .store
            .insert_venue_trade(&VenueTradeRecord {
                cycle: ctx.cycle,
                venue_id: instrument.venue().to_string(),
                symbol: instrument.symbol().to_string(),
                asset_class: asset_class_str(instrument.asset_class).to_string(),
                display_name: Some(instrument.display_name.clone()),
                side: Side::Buy.to_string(),
                entry_price: fill_price.to_string(),
                quantity: if filled_qty > Decimal::ZERO {
                    filled_qty.to_string()
                } else {
                    qty.to_string()
                },
                // Cash value, for the legacy `size` column, so that column
                // means dollars for every kind of trade.
                notional: (if filled_qty > Decimal::ZERO {
                    filled_qty * fill_price
                } else {
                    qty * limit_price
                })
                .to_string(),
                avg_fill_price: (filled_qty > Decimal::ZERO).then(|| fill_price.to_string()),
                edge_at_entry: directional::net_edge(&view, cost_to_trade).to_string(),
                fair_value: view.p_up.to_string(),
                confidence: view.confidence.to_string(),
                risk_pct: sizing.risk_pct.unwrap_or_default().to_string(),
                stop_pct: sizing.stop_pct.unwrap_or_default().to_string(),
                status: status.to_string(),
                stop_price: sizing
                    .stop_pct
                    .map(|s| (fill_price * (Decimal::ONE - s)).to_string()),
                target_price: view.target_price.map(|t| t.to_string()),
                horizon_hours: Some(view.horizon_hours),
                client_order_id: Some(client_order_id.clone()),
                venue_order_id: ack.map(|a| a.venue_order_id.clone()),
            })
            .await?;

        self.store
            .link_order_to_trade(&client_order_id, trade_id)
            .await?;

        // Surface the submit failure now that the thesis is safely recorded.
        let ack = outcome?;

        info!(
            instrument = %instrument.id,
            venue_order_id = %ack.venue_order_id,
            state = order_state_str(&ack.state),
            trade_id,
            qty = %qty,
            limit_price = %limit_price,
            filled = %ack.filled_qty,
            status,
            p_up = %view.p_up,
            horizon_hours = view.horizon_hours,
            at = %ctx.now,
            "Order submitted"
        );

        Ok(EvaluationResult { cost, placed: true })
    }
}

/// Per-cycle inputs, grouped so the evaluation signature stays readable.
struct CycleContext {
    now: DateTime<Utc>,
    state: AgentState,
    bankroll: Decimal,
    cycle: i64,
}

struct EvaluationResult {
    cost: Decimal,
    placed: bool,
}

/// Whether an order must carry the extended-hours flag.
///
/// A venue configured with an extended session reports Open outside regular
/// hours, and an equity order placed then is rejected unless it says so.
/// Crypto has no sessions and rejects the flag outright.
fn needs_extended_hours_flag(asset_class: AssetClass, session: Option<SessionKind>) -> bool {
    asset_class == AssetClass::Equity
        && matches!(
            session,
            Some(SessionKind::Extended) | Some(SessionKind::Overnight)
        )
}

/// The status a trade row starts life in, given what the order did.
///
/// `PENDING` means "an order is out there for this thesis" — it is deliberately
/// not counted as an open position anywhere.
fn trade_status_for(
    state: Option<&OrderState>,
    filled_qty: Decimal,
    requested_qty: Decimal,
) -> &'static str {
    if filled_qty > Decimal::ZERO {
        if filled_qty >= requested_qty {
            "OPEN"
        } else {
            "PARTIAL"
        }
    } else if state.is_some_and(crate::agent::reconcile::is_terminal) {
        // Ended without filling — it never became a position.
        "CANCELLED"
    } else {
        "PENDING"
    }
}

fn order_state_str(state: &OrderState) -> &'static str {
    match state {
        OrderState::Accepted => "ACCEPTED",
        OrderState::PartiallyFilled => "PARTIALLY_FILLED",
        OrderState::Filled => "FILLED",
        OrderState::Cancelled => "CANCELLED",
        OrderState::Expired => "EXPIRED",
        OrderState::Rejected(_) => "REJECTED",
        OrderState::Unknown => "UNKNOWN",
    }
}

fn asset_class_str(class: AssetClass) -> &'static str {
    match class {
        AssetClass::PredictionBinary => "prediction_binary",
        AssetClass::CryptoSpot => "crypto_spot",
        AssetClass::Equity => "equity",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::session::TradingSession;
    use crate::venue::types::{
        Balance, Candle, InstrumentId, InstrumentMeta, OrderAck, OrderRef, Position, Quote,
        Settlement, VenueCapabilities, VenueId,
    };
    use async_trait::async_trait;
    use chrono::Duration;
    use rust_decimal_macros::dec;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Venue that serves one crypto instrument and records placed orders.
    struct StubVenue {
        id: VenueId,
        caps: VenueCapabilities,
        session: TradingSession,
        placed: Arc<AtomicUsize>,
        fill: bool,
    }

    impl StubVenue {
        fn new(fill: bool) -> Self {
            Self {
                id: VenueId::new("stub"),
                caps: VenueCapabilities {
                    asset_classes: vec![AssetClass::CryptoSpot],
                    limit_only_outside_regular: false,
                    supports_client_order_id: true,
                    supports_candles: true,
                },
                session: TradingSession::Always,
                placed: Arc::new(AtomicUsize::new(0)),
                fill,
            }
        }

        fn instrument(&self) -> Instrument {
            Instrument {
                id: InstrumentId::new(self.id.clone(), "BTC/USD"),
                asset_class: AssetClass::CryptoSpot,
                display_name: "Bitcoin".to_string(),
                quote_ccy: "USD".to_string(),
                tick_size: Some(dec!(0.01)),
                lot_size: None,
                min_notional: None,
                min_qty: None,
                fractional: true,
                meta: InstrumentMeta::Spot {
                    base: "BTC".to_string(),
                },
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
            Ok(vec![self.instrument()])
        }
        async fn quote(&self, id: &InstrumentId) -> Result<Quote> {
            Ok(Quote {
                instrument: id.clone(),
                bid: dec!(99.5),
                ask: dec!(100.5),
                mid: dec!(100),
                last: None,
                ts: Utc::now(),
                book: None,
            })
        }
        async fn candles(
            &self,
            _id: &InstrumentId,
            _i: CandleInterval,
            _l: usize,
        ) -> Result<Vec<Candle>> {
            Ok((0..20)
                .map(|i| Candle {
                    ts: Utc::now() + Duration::hours(i),
                    open: dec!(100),
                    high: dec!(100.5),
                    low: dec!(99.5),
                    close: dec!(100),
                    volume: dec!(1000),
                })
                .collect())
        }
        async fn place_order(&self, request: &OrderRequest) -> Result<OrderAck> {
            self.placed.fetch_add(1, Ordering::SeqCst);
            Ok(OrderAck {
                venue_order_id: "venue-1".to_string(),
                client_order_id: request.client_order_id.clone(),
                state: if self.fill {
                    OrderState::Filled
                } else {
                    OrderState::Accepted
                },
                filled_qty: if self.fill {
                    request.qty
                } else {
                    Decimal::ZERO
                },
                avg_fill_price: if self.fill {
                    request.kind.limit_price()
                } else {
                    None
                },
                fees: Decimal::ZERO,
            })
        }
        async fn get_order(&self, _o: &OrderRef) -> Result<OrderAck> {
            anyhow::bail!("not needed")
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
                available: dec!(1000),
                total: Some(dec!(1000)),
            })
        }
        async fn settlement(&self, _id: &InstrumentId) -> Result<Option<Settlement>> {
            Ok(None)
        }
    }

    fn app_config() -> AppConfig {
        let toml = std::fs::read_to_string("config/default.toml").unwrap();
        toml::from_str(&toml).unwrap()
    }

    #[tokio::test]
    async fn without_a_model_no_views_are_taken() {
        let store = Store::new(":memory:").await.unwrap();
        let registry = VenueRegistry::new(vec![Box::new(StubVenue::new(true))]);
        let config = app_config();
        let cycle = VenueCycle {
            registry: &registry,
            llm: None,
            store: &store,
            config: &config,
        };

        let outcome = cycle
            .run(Utc::now(), AgentState::Alive, dec!(1000), 1)
            .await
            .unwrap();
        assert_eq!(outcome.orders_placed, 0);
        assert_eq!(outcome.views_taken, 0);
    }

    #[tokio::test]
    async fn an_unresolved_order_blocks_a_second_order_for_the_same_symbol() {
        let store = Store::new(":memory:").await.unwrap();
        // An order left in UNKNOWN from a previous cycle.
        store
            .insert_order(&OrderRecord {
                id: None,
                client_order_id: "stale".to_string(),
                venue_order_id: None,
                venue_id: "stub".to_string(),
                symbol: "BTC/USD".to_string(),
                side: "BUY".to_string(),
                intent: "ENTRY".to_string(),
                trade_id: None,
                limit_price: Some("100".to_string()),
                qty: "1".to_string(),
                filled_qty: "0".to_string(),
                avg_fill_price: None,
                state: "UNKNOWN".to_string(),
                reject_reason: None,
                cycle: Some(0),
                submitted_at: None,
                updated_at: None,
                expires_at: None,
            })
            .await
            .unwrap();

        let venue = StubVenue::new(true);
        let placed = venue.placed.clone();
        let registry = VenueRegistry::new(vec![Box::new(venue)]);
        let config = app_config();
        let cycle = VenueCycle {
            registry: &registry,
            // No model, but the skip happens before any model call anyway.
            llm: None,
            store: &store,
            config: &config,
        };

        let outcome = cycle
            .run(Utc::now(), AgentState::Alive, dec!(1000), 1)
            .await
            .unwrap();
        assert_eq!(placed.load(Ordering::SeqCst), 0, "must not double-place");
        assert_eq!(outcome.orders_placed, 0);
    }

    #[tokio::test]
    async fn unresolved_orders_are_detected_across_states() {
        let store = Store::new(":memory:").await.unwrap();
        for (i, state) in ["PENDING", "ACCEPTED", "PARTIALLY_FILLED", "UNKNOWN"]
            .iter()
            .enumerate()
        {
            store
                .insert_order(&OrderRecord {
                    id: None,
                    client_order_id: format!("o{i}"),
                    venue_order_id: None,
                    venue_id: "stub".to_string(),
                    symbol: format!("SYM{i}"),
                    side: "BUY".to_string(),
                    intent: "ENTRY".to_string(),
                    trade_id: None,
                    limit_price: None,
                    qty: "1".to_string(),
                    filled_qty: "0".to_string(),
                    avg_fill_price: None,
                    state: state.to_string(),
                    reject_reason: None,
                    cycle: Some(0),
                    submitted_at: None,
                    updated_at: None,
                    expires_at: None,
                })
                .await
                .unwrap();
        }
        // A terminal order must not block anything.
        store
            .update_order_state("o0", "FILLED", Some("v0"), "1", Some("100"), None)
            .await
            .unwrap();

        let unresolved = store.get_unresolved_orders().await.unwrap();
        assert_eq!(unresolved.len(), 3);
        assert!(!unresolved.iter().any(|o| o.state == "FILLED"));
    }

    #[test]
    fn extended_hours_flag_follows_the_session_not_an_assumption() {
        use SessionKind::{Extended, Overnight, Regular};

        // Equities outside regular hours must declare it, or Alpaca rejects
        // the order — the venue still reports Open in those windows.
        assert!(needs_extended_hours_flag(
            AssetClass::Equity,
            Some(Extended)
        ));
        assert!(needs_extended_hours_flag(
            AssetClass::Equity,
            Some(Overnight)
        ));
        assert!(!needs_extended_hours_flag(
            AssetClass::Equity,
            Some(Regular)
        ));

        // Crypto trades 24/7 and rejects the flag.
        for session in [Some(Regular), Some(Extended), Some(Overnight), None] {
            assert!(!needs_extended_hours_flag(AssetClass::CryptoSpot, session));
        }
    }

    #[test]
    fn order_states_map_to_their_stored_names() {
        assert_eq!(order_state_str(&OrderState::Filled), "FILLED");
        assert_eq!(order_state_str(&OrderState::Unknown), "UNKNOWN");
        assert_eq!(
            order_state_str(&OrderState::Rejected("x".into())),
            "REJECTED"
        );
    }
}
