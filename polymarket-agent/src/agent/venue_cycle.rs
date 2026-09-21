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

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use tracing::Instrument as _;
use tracing::{info, instrument, warn};
use uuid::Uuid;

use crate::config::{AgentMode, AppConfig};
use crate::db::store::{OrderRecord, Store, VenueTradeRecord};
use crate::market::models::AgentState;
use crate::risk::circuit_breaker;
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
    /// Cash currently committed to open continuous positions.
    async fn open_venue_notional(&self) -> Result<Decimal> {
        Ok(self
            .store
            .get_open_venue_trades()
            .await?
            .iter()
            .map(|t| t.quantity * t.avg_fill_price.unwrap_or(t.entry_price))
            .sum())
    }

    /// Free cash at the venue that will hold the position, cached per cycle.
    ///
    /// `None` means the venue would not say, in which case the caller skips
    /// it — sizing from a number belonging to a different account is how a
    /// $6 order gets placed against a $30k balance, or vice versa.
    async fn venue_bankroll(
        &self,
        venue: &dyn Venue,
        cache: &mut HashMap<String, Option<Decimal>>,
    ) -> Option<Decimal> {
        let key = venue.id().to_string();
        if let Some(cached) = cache.get(&key) {
            return *cached;
        }
        let resolved = match venue.balance().await {
            Ok(b) => Some(b.available),
            Err(e) => {
                warn!(
                    venue = %venue.id(),
                    error = %e,
                    "Venue would not report a balance — skipping its instruments this cycle"
                );
                None
            }
        };
        cache.insert(key, resolved);
        resolved
    }

    /// Run one pass. Failures on a single instrument are logged and skipped
    /// rather than aborting the cycle — one bad symbol must not stop the rest.
    /// No bankroll parameter: each venue is sized against its own balance,
    /// fetched below. Passing one in is how every venue ended up sized from
    /// the Polymarket wallet.
    #[instrument(
        skip(self),
        fields(otel.name = "venue.cycle", cycle = cycle, state = %state),
        err
    )]
    pub async fn run(
        &self,
        now: DateTime<Utc>,
        state: AgentState,
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
            // The union across venues: discovery is one call for the whole
            // registry, and each venue returns only the symbols it actually
            // lists. Anything venue-specific is read per venue below.
            symbols: self.config.venue_symbols(),
            min_volume_24h: None,
            max_days_to_resolution: None,
            max_results: Some(self.config.scanning.max_markets),
        };

        // Exposure already committed to continuous positions, and the ceiling
        // it may not cross. `size_position` caps a *single* position at
        // `max_position_pct`; nothing capped the total, so a ten-symbol
        // universe at two orders a cycle could reach near-full deployment in
        // eight cycles with no layer objecting. The legacy path has had this
        // via PortfolioManager since the beginning.
        let mut balances: HashMap<String, Option<Decimal>> = HashMap::new();
        let mut open_notional = self.open_venue_notional().await?;
        let mut open_positions = self.store.get_open_venue_trades().await?.len();

        // Once per cycle, not per instrument: the answer is identical for every
        // instrument in the pass, and it is a query over the whole resolved
        // history.
        let calibration = match crate::valuation::calibration::size_multiplier(
            self.store.pool(),
            self.config.risk.calibration_min_samples,
            self.config.risk.calibration_floor,
        )
        .await
        {
            Ok(m) => m,
            // Not fatal, and deliberately not a shrink either: failing to read
            // the record is not evidence against the agent, and inventing a
            // penalty from a database error would be a number with no meaning.
            Err(e) => {
                warn!(error = %format!("{e:#}"), "Could not read the forecast record — sizing as configured");
                None
            }
        };
        if let Some(multiplier) = calibration {
            info!(multiplier = %multiplier, "Sizing scaled by the agent's own forecast record");
        }

        let ctx = CycleContext {
            now,
            state,
            cycle,
            calibration,
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

            // Size against the account that will actually hold the position.
            // This used to be handed the Polymarket wallet balance for every
            // venue, so an Alpaca order was sized from USDC on Polygon: $6
            // orders against a $30k account, or orders the venue rejects for
            // insufficient buying power. A venue that will not report its
            // balance is skipped rather than sized from someone else's.
            let bankroll = match self.venue_bankroll(venue, &mut balances).await {
                Some(b) => b,
                None => continue,
            };
            if bankroll <= Decimal::ZERO {
                continue;
            }

            if let Some(reason) =
                portfolio_block(open_notional, open_positions, bankroll, &self.config.risk)
            {
                info!(
                    venue = %venue.id(),
                    open_notional = %open_notional,
                    open_positions,
                    reason,
                    "No further entries this cycle"
                );
                break;
            }

            match self
                .evaluate_instrument(venue, llm, &instrument, &ctx, bankroll, open_notional)
                .await
            {
                Ok(EvaluationResult {
                    cost,
                    placed,
                    notional,
                    ..
                }) => {
                    outcome.api_cost += cost;
                    if cost > Decimal::ZERO {
                        outcome.views_taken += 1;
                    }
                    if placed {
                        outcome.orders_placed += 1;
                        // Count it immediately: the cap has to hold within a
                        // cycle, not just between them.
                        open_notional += notional;
                        open_positions += 1;
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
        bankroll: Decimal,
        // Cash already committed to open positions, including orders placed
        // earlier in this same cycle. The absolute total cap has to hold
        // *within* a cycle, not just between them.
        open_notional: Decimal,
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

        // The venue's own fee. Charging every venue the highest fee among
        // them made a cheap venue's edge look unprofitable and stopped it
        // trading at all.
        let fee_pct = self.config.venue_fee_pct(venue.id().as_str());
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
                notional: Decimal::ZERO,
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
                bankroll,
                state: ctx.state,
                candles: &candles,
                calibration: ctx.calibration,
                // Absolute cash caps, live only, accounting for what is
                // already committed against the total.
                live_ceiling: circuit_breaker::live_notional_ceiling(
                    self.config.agent.mode == AgentMode::Live,
                    open_notional,
                    &self.config.risk,
                ),
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
                notional: Decimal::ZERO,
                placed: false,
            });
        }

        let limit_price = instrument.round_price(price, Side::Buy);
        if limit_price <= Decimal::ZERO {
            warn!(instrument = %instrument.id, "Non-positive limit price — skipping");
            return Ok(EvaluationResult {
                cost,
                notional: Decimal::ZERO,
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
                notional: Decimal::ZERO,
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
                // The market as it stood when this was decided. Only
                // knowable now — by fill time it has moved, and slippage is
                // the difference between the two.
                mid_at_submit: Some(quote.mid.to_string()),
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
        // The order submission gets its own span: it is the call most worth
        // being able to find a timing or a failure for after the fact.
        let place_span = tracing::info_span!(
            "venue.place_order",
            otel.name = "venue.place_order",
            venue = %venue.id(),
            symbol = %instrument.symbol(),
            qty = %qty,
            limit_price = %limit_price,
            otel.status_code = tracing::field::Empty,
            otel.status_message = tracing::field::Empty,
        );
        // Kept alive past the await so the failure can still be recorded on
        // it. The span is exited when the future returns but not closed until
        // the last handle drops, and a submission that failed must not export
        // as a successful span — this is the one call where "did it work"
        // cannot be inferred from anything else in the trace.
        let status_handle = place_span.clone();
        let outcome = match venue.place_order(&request).instrument(place_span).await {
            Ok(ack) => Ok(ack),
            Err(e) => {
                status_handle.record("otel.status_code", "ERROR");
                status_handle.record("otel.status_message", tracing::field::display(&e));
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

            // A marketable order can come back already filled. Terminal
            // states are excluded from `get_unresolved_orders`, so the
            // reconciler never sees it and would never record the execution —
            // leaving slippage measured over only the *slow* fills, which is
            // the biased tail of exactly the distribution being measured.
            if let (Some(order_id), Some(avg)) = (
                self.store.order_id(&client_order_id).await.ok().flatten(),
                ack.avg_fill_price,
            ) {
                if let Err(e) = self
                    .store
                    .record_fill_increment(
                        order_id,
                        Some(&ack.venue_order_id),
                        ack.filled_qty,
                        avg,
                        Decimal::ZERO,
                        None,
                        Some(ack.fees),
                        Some(quote.mid),
                        &Side::Buy.to_string(),
                        None,
                        ctx.now,
                    )
                    .await
                {
                    warn!(
                        instrument = %instrument.id,
                        error = %e,
                        "Could not record an execution filled at submission"
                    );
                }
            }
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
                    .map(|s| stop_level(fill_price, s, view.invalidation_price).to_string()),
                // A take-profit has to be above what we paid. The model can
                // return a mis-scaled or hallucinated level —
                // `parse_directional_response` range-checks p_up, confidence,
                // expected return and horizon, but cannot know the price — and
                // a target below entry fires `TakeProfit` on the first mark,
                // closing at a loss that the ledger and the alert both label a
                // profit-take. An implausible target is dropped, not obeyed:
                // the position still has its stop and its max-hold.
                target_price: plausible_target(view.target_price, fill_price).map(|t| {
                    if Some(t) != view.target_price {
                        warn!(
                            instrument = %instrument.id,
                            target = ?view.target_price,
                            entry = %fill_price,
                            "Model target is not above the entry — ignoring it"
                        );
                    }
                    t.to_string()
                }),
                horizon_hours: Some(view.horizon_hours),
                client_order_id: Some(client_order_id.clone()),
                venue_order_id: ack.map(|a| a.venue_order_id.clone()),
            })
            .await?;

        // Write the forecast down so it can be scored when the position
        // closes. Nothing on the venue path did this, which left the paper
        // window's Brier criterion unevaluable — the only path that will
        // actually be running during that window.
        //
        // Best-effort: losing a calibration row must never cost a trade.
        //
        // Only for an order the venue actually took. A rejected or timed-out
        // submission has no position behind it, and a forecast with no trade
        // can never be resolved — it would sit in the table forever, and
        // under the old symbol keying it would also have intercepted the
        // outcome meant for a real one.
        let accepted = ack.is_some_and(|a| !matches!(a.state, OrderState::Rejected(_)));
        if accepted {
            if let Err(e) = crate::valuation::calibration::record_directional_prediction(
                self.store.pool(),
                trade_id,
                view.confidence,
                view.p_up,
                fill_price,
            )
            .await
            {
                warn!(instrument = %instrument.id, error = %e, "Could not record the forecast for calibration");
            }
        }

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

        Ok(EvaluationResult {
            cost,
            notional: qty * limit_price,
            placed: true,
        })
    }
}

/// Per-cycle inputs, grouped so the evaluation signature stays readable.
struct CycleContext {
    now: DateTime<Utc>,
    state: AgentState,
    cycle: i64,
    /// How much of the configured size the agent's own forecast record
    /// justifies. Read once per cycle — it is one query over resolved
    /// history, and it cannot change between instruments in the same pass.
    calibration: Option<Decimal>,
}

struct EvaluationResult {
    cost: Decimal,
    /// Cash committed by an order placed in this evaluation, for the
    /// running exposure total.
    notional: Decimal,
    placed: bool,
}

/// Where to stop out of a long.
///
/// The ATR stop sizes the position so a loss at that level equals the risk
/// budget. The model separately names an `invalidation_price` — the level at
/// which its own thesis is wrong — and that was parsed, range-checked, stored
/// on the view and then read by nobody, so the agent would hold straight
/// through a level it had itself identified as disproving the trade.
///
/// The tighter of the two wins, which for a long is the higher. That can only
/// reduce the loss at the stop, so the risk arithmetic stays valid; taking the
/// looser one would quietly widen the risk beyond what was sized for. An
/// invalidation at or above the entry is nonsense for a long — it would exit
/// on the first mark — so it is ignored.
fn stop_level(entry: Decimal, stop_pct: Decimal, invalidation: Option<Decimal>) -> Decimal {
    let atr_stop = entry * (Decimal::ONE - stop_pct);
    match invalidation {
        Some(level) if level > Decimal::ZERO && level < entry => atr_stop.max(level),
        _ => atr_stop,
    }
}

/// A take-profit level, if the model gave a believable one.
///
/// It has to be above what we paid. `parse_directional_response` range-checks
/// p_up, confidence, expected return and horizon, but cannot know the price,
/// so a mis-scaled or hallucinated target reaches here intact — and a target
/// below entry fires `TakeProfit` on the very first mark, closing at a loss
/// that the ledger and the alert both label a profit-take. An implausible one
/// is dropped rather than obeyed: the position keeps its stop and its
/// max-hold, so it is still bounded.
fn plausible_target(target: Option<Decimal>, entry: Decimal) -> Option<Decimal> {
    target.filter(|t| *t > entry)
}

/// Whether portfolio limits forbid another entry, and why.
///
/// `size_position` caps one position at `max_position_pct`; nothing capped the
/// *total*, so a ten-symbol universe at two orders a cycle could reach
/// near-full deployment in eight cycles with no layer objecting. The legacy
/// path has had this since the beginning via `PortfolioManager`; the venue path
/// had nothing.
///
/// A free function so the arithmetic can be tested on its own — driving a whole
/// cycle to reach it needs a live model, which is how the first version of
/// these tests ended up passing whether the check was there or not.
fn portfolio_block(
    open_notional: Decimal,
    open_positions: usize,
    bankroll: Decimal,
    risk: &crate::config::RiskConfig,
) -> Option<&'static str> {
    if open_notional >= bankroll * risk.max_total_exposure_pct {
        return Some("total exposure is at its cap");
    }
    if open_positions >= risk.max_positions_per_category as usize {
        return Some("at the open-position limit");
    }
    None
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
        /// Make `balance()` fail, as an unreachable or unauthorised venue would.
        no_balance: bool,
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
                no_balance: false,
            }
        }

        fn without_balance(mut self) -> Self {
            self.no_balance = true;
            self
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
            if self.no_balance {
                anyhow::bail!("venue will not report a balance");
            }
            Ok(Balance {
                ccy: "USD".to_string(),
                available: dec!(1000),
            })
        }
        async fn equity(&self) -> Result<Option<Decimal>> {
            Ok(Some(dec!(1000)))
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

        let outcome = cycle.run(Utc::now(), AgentState::Alive, 1).await.unwrap();
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
                mid_at_submit: None,
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

        let outcome = cycle.run(Utc::now(), AgentState::Alive, 1).await.unwrap();
        assert_eq!(placed.load(Ordering::SeqCst), 0, "must not double-place");
        assert_eq!(outcome.orders_placed, 0);
    }

    /// A venue that will not say what it holds must not be sized from some
    /// other account's balance — that is how an Alpaca order ends up sized
    /// from USDC on Polygon. `None` means the caller skips the venue.
    #[tokio::test]
    async fn a_venue_that_will_not_report_a_balance_yields_no_bankroll() {
        let store = Store::new(":memory:").await.unwrap();
        let config = app_config();
        let registry = VenueRegistry::new(vec![]);
        let cycle = VenueCycle {
            registry: &registry,
            llm: None,
            store: &store,
            config: &config,
        };

        let mut cache = HashMap::new();
        let ok = StubVenue::new(true);
        assert_eq!(
            cycle.venue_bankroll(&ok, &mut cache).await,
            Some(dec!(1000)),
            "a venue that reports is sized from its own free cash"
        );

        let mut cache = HashMap::new();
        let silent = StubVenue::new(true).without_balance();
        assert_eq!(
            cycle.venue_bankroll(&silent, &mut cache).await,
            None,
            "a venue that will not report must not fall back to another's balance"
        );
    }

    /// The cache exists so a ten-symbol universe does not make ten balance
    /// calls per cycle.
    #[tokio::test]
    async fn a_venue_balance_is_fetched_once_per_cycle() {
        let store = Store::new(":memory:").await.unwrap();
        let config = app_config();
        let registry = VenueRegistry::new(vec![]);
        let cycle = VenueCycle {
            registry: &registry,
            llm: None,
            store: &store,
            config: &config,
        };

        let venue = StubVenue::new(true);
        let mut cache = HashMap::new();
        cycle.venue_bankroll(&venue, &mut cache).await;
        cycle.venue_bankroll(&venue, &mut cache).await;
        assert_eq!(cache.len(), 1);
    }

    /// `size_position` caps one position; nothing capped the total, so a
    /// ten-symbol universe at two orders a cycle could reach near-full
    /// deployment in eight cycles with no layer objecting.
    #[test]
    fn total_exposure_is_capped_across_positions() {
        let risk = app_config().risk;
        let bankroll = dec!(1000);
        let ceiling = bankroll * risk.max_total_exposure_pct;

        assert_eq!(
            portfolio_block(Decimal::ZERO, 0, bankroll, &risk),
            None,
            "an empty book allows an entry"
        );
        assert_eq!(
            portfolio_block(ceiling - dec!(1), 0, bankroll, &risk),
            None,
            "just under the ceiling still allows one"
        );
        assert_eq!(
            portfolio_block(ceiling, 0, bankroll, &risk),
            Some("total exposure is at its cap"),
            "at the ceiling, no more"
        );
        assert_eq!(
            portfolio_block(ceiling + dec!(1), 0, bankroll, &risk),
            Some("total exposure is at its cap")
        );
    }

    /// The model names the level at which its own thesis is wrong. That was
    /// parsed, validated, stored and then read by nobody, so the agent would
    /// hold straight through it.
    #[test]
    fn the_model_invalidation_tightens_the_stop_but_never_loosens_it() {
        let entry = dec!(100);
        let atr = dec!(0.03); // ATR stop at 97

        assert_eq!(stop_level(entry, atr, None), dec!(97));

        // Thesis breaks above the ATR stop: exit sooner.
        assert_eq!(stop_level(entry, atr, Some(dec!(98))), dec!(98));

        // Thesis breaks below it: keep the ATR stop, because widening would
        // risk more than the position was sized for.
        assert_eq!(stop_level(entry, atr, Some(dec!(90))), dec!(97));

        // Nonsense levels for a long are ignored rather than obeyed.
        assert_eq!(stop_level(entry, atr, Some(dec!(105))), dec!(97));
        assert_eq!(stop_level(entry, atr, Some(entry)), dec!(97));
        assert_eq!(stop_level(entry, atr, Some(Decimal::ZERO)), dec!(97));
    }

    /// A target below the entry books a loss as a take-profit: `evaluate_exit`
    /// fires TakeProfit on the first mark above it, and the trade closes with
    /// `close_reason = TAKE_PROFIT` at a loss.
    #[test]
    fn a_target_at_or_below_the_entry_is_discarded() {
        let entry = dec!(100);
        assert_eq!(plausible_target(Some(dec!(110)), entry), Some(dec!(110)));
        assert_eq!(
            plausible_target(Some(dec!(92)), entry),
            None,
            "a target below entry would close at a loss labelled a profit"
        );
        assert_eq!(
            plausible_target(Some(entry), entry),
            None,
            "a target at entry is not a profit either"
        );
        assert_eq!(plausible_target(None, entry), None);
    }

    #[test]
    fn the_open_position_count_is_capped_too() {
        let risk = app_config().risk;
        let bankroll = dec!(1_000_000); // exposure is nowhere near the cap
        let limit = risk.max_positions_per_category as usize;

        assert_eq!(
            portfolio_block(Decimal::ZERO, limit - 1, bankroll, &risk),
            None
        );
        assert_eq!(
            portfolio_block(Decimal::ZERO, limit, bankroll, &risk),
            Some("at the open-position limit")
        );
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
                    mid_at_submit: None,
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
