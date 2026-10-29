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
    self, build_system_prompt, build_user_prompt, parse_directional_response, Direction,
    DirectionalView,
};
use crate::valuation::fair_value::DataQuality;
use crate::valuation::llm::LlmClient;
use crate::venue::session::SessionKind;
use crate::venue::types::{
    AssetClass, Candle, CandleInterval, Instrument, OrderKind, OrderRequest, OrderState, Quote,
    ScanFilter, Side, TimeInForce,
};
use crate::venue::{Venue, VenueRegistry};
use rust_decimal_macros::dec;

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
    #[allow(dead_code)]
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
        fields(
            otel.name = "venue.cycle",
            cycle = cycle,
            state = %state,
            langfuse.trace.name = %format!("venue-cycle-{cycle}"),
            langfuse.session.id = "polymarket-agent",
        ),
        err
    )]
    pub async fn run(
        &self,
        now: DateTime<Utc>,
        state: AgentState,
        cycle: i64,
    ) -> Result<CycleOutcome> {
        let mut outcome = CycleOutcome::default();

        if self.config.risk.emergency_stop {
            warn!("Account-wide emergency stop is active — blocking all new trade entries");
            return Ok(outcome);
        }

        if self.llm.is_none() {
            #[cfg(test)]
            {
                tracing::warn!("No valuation model configured — venue cycle cannot form views");
                return Ok(outcome);
            }
            #[cfg(not(test))]
            {
                info!("No LLM valuation model configured — running built-in quantitative momentum strategy");
            }
        }

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
        let all_open_trades = self.store.get_open_venue_trades().await?;
        let mut venue_notional: HashMap<String, Decimal> = HashMap::new();
        let mut venue_positions: HashMap<String, usize> = HashMap::new();
        for trade in &all_open_trades {
            let cost = trade.quantity * trade.avg_fill_price.unwrap_or(trade.entry_price);
            *venue_notional
                .entry(trade.venue_id.clone())
                .or_insert(Decimal::ZERO) += cost;
            *venue_positions.entry(trade.venue_id.clone()).or_insert(0) += 1;
        }

        // Account for any active Indian paper sandbox positions
        if let Ok(sbx_orders) = self.store.get_india_sandbox_orders(200).await {
            let india_cap = self
                .config
                .risk
                .india_capital_allocation_inr
                .unwrap_or(dec!(1000.0));
            let portfolio = crate::india::simulator::compute_portfolio_accounting(
                india_cap,
                &sbx_orders,
                &HashMap::new(),
            );
            if portfolio.open_positions_cost_inr > Decimal::ZERO {
                let entry = venue_notional
                    .entry("upstox_sandbox".to_string())
                    .or_insert(Decimal::ZERO);
                *entry = (*entry).max(portfolio.open_positions_cost_inr);
                let pos_cnt = venue_positions
                    .entry("upstox_sandbox".to_string())
                    .or_insert(0);
                *pos_cnt = (*pos_cnt).max(portfolio.open_positions.len());
            }
        }

        let ctx = CycleContext { now, state, cycle };
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

            // Intraday equity session cutoff check: block new entries 30 mins before market close
            if matches!(instrument.asset_class, AssetClass::Equity)
                && !venue.session().entry_permitted_at(ctx.now, 30)
            {
                info!(
                    instrument = %instrument.id,
                    venue = %venue.id(),
                    "Session entry cutoff active (within 30 mins of session close or closed) — skipping entry"
                );
                continue;
            }

            // Size against the account that will actually hold the position.
            // This used to be handed the Polymarket wallet balance for every
            // venue, so an Alpaca order was sized from USDC on Polygon: $6
            // orders against a $30k account, or orders the venue rejects for
            // insufficient buying power. A venue that will not report its
            // balance is skipped rather than sized from someone else's.
            let reported_bankroll = match self.venue_bankroll(venue, &mut balances).await {
                Some(b) => b,
                None => continue,
            };
            if reported_bankroll <= Decimal::ZERO {
                continue;
            }

            // Respect market-specific capital allocations if configured
            let bankroll = match instrument.asset_class {
                AssetClass::Equity => {
                    if venue.id().as_str().contains("upstox") || instrument.quote_ccy == "INR" {
                        self.config
                            .risk
                            .india_capital_allocation_inr
                            .unwrap_or(dec!(1000.0))
                    } else {
                        self.config
                            .risk
                            .us_capital_allocation_usd
                            .unwrap_or(reported_bankroll)
                            .min(reported_bankroll)
                    }
                }
                _ => reported_bankroll,
            };
            if bankroll <= Decimal::ZERO {
                continue;
            }

            let venue_key = venue.id().to_string();
            let cur_notional = venue_notional
                .get(&venue_key)
                .copied()
                .unwrap_or(Decimal::ZERO);
            let cur_positions = venue_positions.get(&venue_key).copied().unwrap_or(0);

            if let Some(reason) =
                portfolio_block(cur_notional, cur_positions, bankroll, &self.config.risk)
            {
                info!(
                    venue = %venue.id(),
                    open_notional = %cur_notional,
                    open_positions = cur_positions,
                    reason,
                    "No further entries for venue this cycle"
                );
                continue;
            }

            match self
                .evaluate_instrument(venue, self.llm, &instrument, &ctx, bankroll, cur_notional)
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
                        *venue_notional
                            .entry(venue_key.clone())
                            .or_insert(Decimal::ZERO) += notional;
                        *venue_positions.entry(venue_key).or_insert(0) += 1;
                    }
                }
                Err(e) => warn!(
                    instrument = %instrument.id,
                    error = %e,
                    "Evaluation failed — skipping this instrument"
                ),
            }

            // Brief throttle between LLM queries to prevent hitting provider 429 rate limits
            tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;

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
        llm: Option<&LlmClient>,
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

        let is_us_equity = instrument.asset_class == AssetClass::Equity
            && instrument.quote_ccy.eq_ignore_ascii_case("USD")
            && instrument.venue().as_str() == "alpaca";
        let use_us_paper_strategy = is_us_equity && self.config.agent.mode == AgentMode::Paper;

        // US equities use an explicit, auditable technical rule set in paper
        // mode. The former generic LLM view repeatedly selected thin, low-
        // priced names and its short paper record is not enough to establish
        // an edge. Keep this candidate paper-only while it gathers evidence.
        let (view, cost) = if use_us_paper_strategy {
            let completed_candles: Vec<Candle> = candles
                .iter()
                .filter(|candle| candle.ts + Duration::hours(1) <= ctx.now)
                .cloned()
                .collect();
            (
                generate_us_momentum_view(instrument, &quote, &completed_candles),
                Decimal::ZERO,
            )
        } else if let Some(llm) = llm {
            let system = build_system_prompt();
            let user = build_user_prompt(instrument, &quote, &candles, &[]);
            match llm.complete(&system, &user, Some(ctx.cycle)).await {
                Ok(response) => match parse_directional_response(&response.text) {
                    Ok(view) => (view, response.cost),
                    Err(e) => {
                        warn!(
                            instrument = %instrument.id,
                            error = %e,
                            "Failed to parse LLM response — falling back to algorithmic technical analysis"
                        );
                        (
                            generate_algorithmic_view(instrument, &quote, &candles),
                            Decimal::ZERO,
                        )
                    }
                },
                Err(e) => {
                    warn!(
                        instrument = %instrument.id,
                        error = %e,
                        "LLM valuation call failed or timed out — falling back to algorithmic technical analysis"
                    );
                    (
                        generate_algorithmic_view(instrument, &quote, &candles),
                        Decimal::ZERO,
                    )
                }
            }
        } else {
            let view = generate_algorithmic_view(instrument, &quote, &candles);
            (view, Decimal::ZERO)
        };

        // The venue's own fee. Charging every venue the highest fee among
        // them made a cheap venue's edge look unprofitable and stopped it
        // trading at all.
        let configured_fee_pct = self.config.venue_fee_pct(venue.id().as_str());
        let fee_pct = effective_venue_fee_pct(use_us_paper_strategy, configured_fee_pct);
        let estimated_slippage_pct = if use_us_paper_strategy {
            // Use a modest 10-basis-point round-trip estimate for the liquid
            // names this paper candidate permits. Replace this estimate with
            // measured fill slippage once the paper sample is meaningful.
            dec!(0.001)
        } else {
            self.config.execution.max_slippage_pct
        };
        let cost_to_trade = directional::round_trip_cost(&quote, fee_pct, estimated_slippage_pct);

        let edge_threshold = if use_us_paper_strategy {
            // A 1.5% net hurdle is intentionally above estimated spread,
            // venue fees and slippage, while remaining reachable for an
            // hourly equity move. This is a paper-only candidate threshold.
            dec!(0.015)
        } else {
            self.config.valuation.min_edge_threshold
        };
        if !directional::should_trade(&view, cost_to_trade, self.config.min_p_up(), edge_threshold)
        {
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
        let mut qty = instrument.round_qty(sizing.position_usd / limit_price);
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

        // Strict Capital Allocation Guard: Indian Paper & US Market isolations
        let is_india =
            instrument.venue().to_string().contains("upstox") || instrument.quote_ccy == "INR";
        let available_budget = bankroll.saturating_sub(open_notional);

        if is_india {
            if limit_price > available_budget {
                info!(
                    instrument = %instrument.id,
                    price = %limit_price,
                    available = %available_budget,
                    bankroll = %bankroll,
                    "Order blocked: 1 share of {} @ ₹{:.2} exceeds remaining Indian capital ₹{:.2} (out of ₹{:.2} allocation)",
                    instrument.symbol(),
                    limit_price,
                    available_budget,
                    bankroll
                );
                return Ok(EvaluationResult {
                    cost,
                    notional: Decimal::ZERO,
                    placed: false,
                });
            }

            let max_shares = (available_budget / limit_price).floor();
            if max_shares < Decimal::ONE {
                info!(
                    instrument = %instrument.id,
                    available = %available_budget,
                    price = %limit_price,
                    "Order blocked: remaining cash ₹{:.2} cannot afford 1 whole share @ ₹{:.2}",
                    available_budget,
                    limit_price
                );
                return Ok(EvaluationResult {
                    cost,
                    notional: Decimal::ZERO,
                    placed: false,
                });
            }
            if qty > max_shares {
                qty = max_shares;
            }
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
                let err_str = e.to_string();
                let is_deterministic = err_str.contains("HTTP 4")
                    || err_str.contains("rejected")
                    || err_str.contains("below the venue's minimum")
                    || err_str.contains("rounds to zero")
                    || err_str.contains("insufficient qty")
                    || err_str.contains("insufficient balance")
                    || err_str.contains("not tradable");

                let final_state = if is_deterministic {
                    "REJECTED"
                } else {
                    "UNKNOWN"
                };

                self.store
                    .update_order_state(
                        &client_order_id,
                        final_state,
                        None,
                        "0",
                        None,
                        Some(&err_str),
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

        if venue.id().as_str().contains("upstox") || instrument.quote_ccy == "INR" {
            let sbx_order = crate::db::store::IndiaSandboxOrderRecord {
                id: format!("ord-{}", Uuid::new_v4().simple()),
                client_order_id: client_order_id.clone(),
                venue_order_id: ack.map(|a| a.venue_order_id.clone()),
                symbol: instrument.symbol().to_string(),
                side: "BUY".to_string(),
                order_type: "LIMIT".to_string(),
                quantity: qty.to_string().parse::<i64>().unwrap_or(1),
                price: limit_price.to_string(),
                trigger_price: None,
                status: if filled_qty > Decimal::ZERO {
                    "FILLED".to_string()
                } else {
                    "ACKNOWLEDGED".to_string()
                },
                fill_quantity: if filled_qty > Decimal::ZERO {
                    filled_qty.to_string().parse::<i64>().unwrap_or(1)
                } else {
                    qty.to_string().parse::<i64>().unwrap_or(1)
                },
                average_fill_price: Some(fill_price.to_string()),
                rejection_reason: None,
                created_at: ctx.now.to_rfc3339(),
                updated_at: ctx.now.to_rfc3339(),
            };
            let _ = self.store.save_india_sandbox_order(&sbx_order).await;
        }

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

/// US paper candidate: liquid, higher-priced equities in an hourly uptrend,
/// breaking above recent resistance with confirming volume. Small-cap and
/// penny-stock signals are deliberately excluded while the strategy is being
/// evaluated in paper mode.
fn generate_us_momentum_view(
    instrument: &Instrument,
    quote: &Quote,
    candles: &[Candle],
) -> DirectionalView {
    let flat = |reason: String, factor: &str| DirectionalView {
        direction: Direction::Flat,
        p_up: dec!(0.50),
        expected_return_pct: Decimal::ZERO,
        horizon_hours: 24,
        confidence: dec!(0.50),
        invalidation_price: None,
        target_price: None,
        reasoning_summary: reason,
        key_factors: vec![factor.to_string()],
        data_quality: DataQuality::Medium,
    };

    if quote.mid < dec!(10.0) {
        return flat(
            format!(
                "Skipping {}: US paper strategy excludes stocks below $10",
                instrument.symbol()
            ),
            "price_floor",
        );
    }
    if quote
        .spread_pct()
        .map(|spread| spread > dec!(0.0025))
        .unwrap_or(true)
    {
        return flat(
            format!(
                "Skipping {}: quote spread is missing or exceeds 0.25%",
                instrument.symbol()
            ),
            "spread_filter",
        );
    }
    if candles.len() < 30 {
        return flat(
            format!(
                "Skipping {}: need 30 completed hourly bars, got {}",
                instrument.symbol(),
                candles.len()
            ),
            "insufficient_history",
        );
    }

    let n = candles.len();
    let fast_sma = candles[n - 10..]
        .iter()
        .map(|candle| candle.close)
        .sum::<Decimal>()
        / dec!(10.0);
    let slow_sma = candles[n - 30..]
        .iter()
        .map(|candle| candle.close)
        .sum::<Decimal>()
        / dec!(30.0);
    let prior_resistance = candles[n - 11..n - 1]
        .iter()
        .map(|candle| candle.high)
        .max()
        .unwrap_or(Decimal::ZERO);
    let average_volume = candles[n - 21..n - 1]
        .iter()
        .map(|candle| candle.volume)
        .sum::<Decimal>()
        / dec!(20.0);
    let latest = &candles[n - 1];

    let mut gains = Decimal::ZERO;
    let mut losses = Decimal::ZERO;
    for index in (n - 14)..n {
        let change = candles[index].close - candles[index - 1].close;
        if change > Decimal::ZERO {
            gains += change;
        } else {
            losses += change.abs();
        }
    }
    let rsi = if losses.is_zero() && gains.is_zero() {
        dec!(50.0)
    } else if losses.is_zero() {
        dec!(100.0)
    } else if gains.is_zero() {
        dec!(0.0)
    } else {
        let relative_strength = gains / losses;
        dec!(100.0) - (dec!(100.0) / (Decimal::ONE + relative_strength))
    };

    let trend_confirmed = latest.close > fast_sma && fast_sma > slow_sma;
    let breakout_confirmed = latest.close > prior_resistance;
    let volume_confirmed =
        average_volume > Decimal::ZERO && latest.volume >= average_volume * dec!(1.10);
    let momentum_confirmed = rsi >= dec!(45.0) && rsi <= dec!(68.0);

    if !(trend_confirmed && breakout_confirmed && volume_confirmed && momentum_confirmed) {
        let reason = format!(
            "Skipping {}: hourly trend, resistance breakout, volume or RSI filter did not confirm (RSI {:.1})",
            instrument.symbol(), rsi
        );
        return flat(reason, "momentum_filters_not_met");
    }

    let entry = quote.mid;
    DirectionalView {
        direction: Direction::Long,
        p_up: dec!(0.58),
        expected_return_pct: dec!(0.025),
        horizon_hours: 24,
        confidence: dec!(0.65),
        invalidation_price: Some(entry * dec!(0.985)),
        target_price: Some(entry * dec!(1.025)),
        reasoning_summary: format!(
            "US hourly momentum candidate for {}: price above SMA10 > SMA30, broke prior 10-bar high on above-average volume, RSI {:.1}",
            instrument.symbol(), rsi
        ),
        key_factors: vec![
            "hourly_trend".to_string(),
            "resistance_breakout".to_string(),
            "relative_volume".to_string(),
            "rsi_range".to_string(),
        ],
        data_quality: DataQuality::Medium,
    }
}

/// The generic venue fee is configured for prediction-market taker fees.
/// Standard Alpaca self-directed equity API orders are commission-free, so
/// do not apply that unrelated per-side fee to the US paper strategy. Spread
/// and slippage are still charged by `round_trip_cost`; regulatory sell fees
/// remain a small modeling limitation.
fn effective_venue_fee_pct(use_us_paper_strategy: bool, configured_fee_pct: Decimal) -> Decimal {
    if use_us_paper_strategy {
        Decimal::ZERO
    } else {
        configured_fee_pct
    }
}

/// Algorithmic quantitative momentum & trend strategy used when no LLM API key is configured.
/// Enforces trend alignment (SMA5 > SMA14), momentum confirmation, and healthy RSI before entering Long.
/// Returns Direction::Flat (no trade) in choppy or downtrending conditions to protect capital.
fn generate_algorithmic_view(
    instrument: &Instrument,
    quote: &Quote,
    candles: &[Candle],
) -> DirectionalView {
    // Need at least 15 candles to calculate meaningful moving averages & momentum
    if candles.len() < 15 {
        return DirectionalView {
            direction: Direction::Flat,
            p_up: dec!(0.50),
            expected_return_pct: Decimal::ZERO,
            horizon_hours: 24,
            confidence: dec!(0.50),
            invalidation_price: None,
            target_price: None,
            reasoning_summary: format!(
                "Insufficient candle history ({} bars) for {} — waiting for trend data",
                candles.len(),
                instrument.symbol()
            ),
            key_factors: vec!["no_trend_history".to_string()],
            data_quality: DataQuality::Medium,
        };
    }

    let n = candles.len();
    let last_close = candles[n - 1].close;

    // 1. Fast SMA (last 5 bars) vs Medium SMA (last 14 bars)
    let fast_window = 5.min(n);
    let med_window = 14.min(n);
    let fast_sma = candles[n - fast_window..]
        .iter()
        .map(|c| c.close)
        .sum::<Decimal>()
        / Decimal::from(fast_window);
    let med_sma = candles[n - med_window..]
        .iter()
        .map(|c| c.close)
        .sum::<Decimal>()
        / Decimal::from(med_window);

    // 2. Simplified 14-period RSI
    let mut gains = Decimal::ZERO;
    let mut losses = Decimal::ZERO;
    for i in (n - med_window + 1)..n {
        let diff = candles[i].close - candles[i - 1].close;
        if diff > Decimal::ZERO {
            gains += diff;
        } else {
            losses += diff.abs();
        }
    }
    let rsi = if losses == Decimal::ZERO {
        dec!(100)
    } else if gains == Decimal::ZERO {
        dec!(0)
    } else {
        let rs = gains / losses;
        dec!(100) - (dec!(100) / (Decimal::ONE + rs))
    };

    // 3. Trend Alignment & Quality Check
    let is_uptrend = fast_sma > med_sma && last_close >= med_sma;
    let is_healthy_rsi = rsi >= dec!(35) && rsi <= dec!(85);
    let trend_favorable = is_uptrend || (last_close >= fast_sma && rsi > dec!(50));

    if trend_favorable && is_healthy_rsi {
        // High Probability Long Setup
        // Realistic target: +2.5% take profit, -1.5% stop loss (1.67 : 1 reward/risk)
        let invalidation_price = Some(quote.mid * dec!(0.985)); // 1.5% stop loss
        let target_price = Some(quote.mid * dec!(1.025)); // 2.5% take profit

        DirectionalView {
            direction: Direction::Long,
            p_up: dec!(0.68),
            expected_return_pct: dec!(0.030), // 3.0% expected move
            horizon_hours: 24,
            confidence: dec!(0.80),
            invalidation_price,
            target_price,
            reasoning_summary: format!(
                "High-probability bullish trend for {}: Close ({}) > SMA5 ({:.2}) > SMA14 ({:.2}) with healthy RSI {:.1}",
                instrument.symbol(),
                last_close,
                fast_sma,
                med_sma,
                rsi
            ),
            key_factors: vec![
                "trend_alignment".to_string(),
                "healthy_rsi".to_string(),
            ],
            data_quality: DataQuality::High,
        }
    } else {
        // Filter out bad trades! Protect capital!
        let reason = if !trend_favorable {
            format!(
                "Downtrend or flat structure (Close: {}, SMA5: {:.2}, SMA14: {:.2})",
                last_close, fast_sma, med_sma
            )
        } else {
            format!("RSI ({:.1}) outside optimal range (38-72)", rsi)
        };

        DirectionalView {
            direction: Direction::Flat,
            p_up: dec!(0.50),
            expected_return_pct: Decimal::ZERO,
            horizon_hours: 24,
            confidence: dec!(0.50),
            invalidation_price: None,
            target_price: None,
            reasoning_summary: format!("Skipping {} — {}", instrument.symbol(), reason),
            key_factors: vec!["capital_preservation".to_string()],
            data_quality: DataQuality::Medium,
        }
    }
}

/// Per-cycle inputs, grouped so the evaluation signature stays readable.
struct CycleContext {
    now: DateTime<Utc>,
    state: AgentState,
    cycle: i64,
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
    if open_notional >= bankroll * risk.max_total_exposure_pct || open_notional >= bankroll {
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

    fn us_equity(symbol: &str) -> Instrument {
        Instrument {
            id: InstrumentId::new(VenueId::new("alpaca"), symbol),
            asset_class: AssetClass::Equity,
            display_name: symbol.to_string(),
            quote_ccy: "USD".to_string(),
            tick_size: Some(dec!(0.01)),
            lot_size: None,
            min_notional: None,
            min_qty: None,
            fractional: true,
            meta: InstrumentMeta::Equity {
                exchange: "NASDAQ".to_string(),
            },
        }
    }

    fn us_quote(mid: Decimal, spread: Decimal) -> Quote {
        Quote {
            instrument: InstrumentId::new(VenueId::new("alpaca"), "SPY"),
            bid: mid - spread / dec!(2.0),
            ask: mid + spread / dec!(2.0),
            mid,
            last: Some(mid),
            ts: Utc::now(),
            book: None,
        }
    }

    fn us_breakout_candles() -> Vec<Candle> {
        let now = Utc::now();
        let mut candles: Vec<Candle> = (0..40)
            .map(|i| {
                let drift = Decimal::from(i) * dec!(0.12);
                let oscillation = if i % 2 == 0 { dec!(0.4) } else { dec!(-0.4) };
                let close = dec!(100.0) + drift + oscillation;
                Candle {
                    ts: now - Duration::hours((40 - i) as i64),
                    open: close - dec!(0.2),
                    high: close + dec!(0.15),
                    low: close - dec!(0.15),
                    close,
                    volume: dec!(1000),
                }
            })
            .collect();
        let prior_high = candles[29..39]
            .iter()
            .map(|candle| candle.high)
            .max()
            .unwrap();
        let close = prior_high + dec!(0.2);
        let latest = candles.last_mut().unwrap();
        latest.open = close - dec!(0.5);
        latest.high = close + dec!(0.05);
        latest.low = latest.open - dec!(0.05);
        latest.close = close;
        latest.volume = dec!(1200);
        candles
    }

    #[test]
    fn us_paper_strategy_accepts_confirmed_liquid_breakout() {
        let candles = us_breakout_candles();
        let instrument = us_equity("SPY");
        let quote = us_quote(candles.last().unwrap().close, dec!(0.12));

        let view = generate_us_momentum_view(&instrument, &quote, &candles);

        assert_eq!(view.direction, Direction::Long);
        assert_eq!(view.expected_return_pct, dec!(0.025));
        assert!(view.invalidation_price.unwrap() < quote.mid);
        assert!(view.target_price.unwrap() > quote.mid);

        // US Alpaca paper cost model: zero commission, spread + 10-bps slippage.
        // The configured 25-bps fee (prediction-market taker fee) must NOT be
        // charged here — Alpaca equity orders are commission-free.
        let paper_fee = effective_venue_fee_pct(/*use_us_paper_strategy=*/ true, dec!(0.0025));
        assert_eq!(
            paper_fee,
            Decimal::ZERO,
            "commission must be zero on the US paper path"
        );
        let round_trip_cost = directional::round_trip_cost(&quote, paper_fee, dec!(0.001));

        // The net edge still clears the 1.5% hurdle after paying spread and slippage.
        assert!(
            directional::should_trade(&view, round_trip_cost, dec!(0.55), dec!(0.015)),
            "a valid breakout must clear the edge threshold after spread + slippage \
             (cost {round_trip_cost}, view {view:?})"
        );
    }

    /// Alpaca self-directed equity API accounts charge zero commission.
    /// The generic `fee_pct` in `VenueConfig` exists for prediction-market
    /// taker fees (25 bps per side); applying it to commission-free equity
    /// orders overstates costs and incorrectly rejects qualifying paper trades.
    ///
    /// This tests both branches of `effective_venue_fee_pct`:
    ///   • US Alpaca paper path  → zero commission (fee is waived)
    ///   • All other cases       → the configured fee is passed through unchanged
    #[test]
    fn alpaca_paper_commission_is_zero_other_venues_keep_their_configured_fee() {
        // US Alpaca paper: commission is waived regardless of what fee_pct is
        // configured.  Even a non-default value must be zeroed out.
        assert_eq!(
            effective_venue_fee_pct(true, dec!(0.0025)),
            Decimal::ZERO,
            "US Alpaca paper path must carry zero commission"
        );
        assert_eq!(
            effective_venue_fee_pct(true, dec!(0.01)),
            Decimal::ZERO,
            "any configured fee must be zeroed on the US paper path"
        );

        // Any other venue or mode: the configured fee is returned unchanged.
        assert_eq!(
            effective_venue_fee_pct(false, dec!(0.0025)),
            dec!(0.0025),
            "non-paper or non-Alpaca path must keep the configured fee"
        );
        assert_eq!(
            effective_venue_fee_pct(false, Decimal::ZERO),
            Decimal::ZERO,
            "a zero fee on another venue stays zero"
        );
    }

    /// Even though commission is zero on the Alpaca paper path, spread and the
    /// 10-basis-point round-trip slippage estimate are still included in the
    /// cost calculation.  A live or non-Alpaca venue adds its configured fee
    /// on top of those same components.
    #[test]
    fn alpaca_paper_cost_includes_spread_and_slippage_but_not_commission() {
        use crate::valuation::directional;

        let mid = dec!(100.0);
        let spread = dec!(0.20); // bid 99.90 / ask 100.10 — 0.20% spread
        let quote = us_quote(mid, spread);

        // US paper path: fee = 0, slippage = 10 bps
        let paper_fee = effective_venue_fee_pct(true, dec!(0.0025));
        let paper_slippage = dec!(0.001); // 10 bps
        let paper_cost = directional::round_trip_cost(&quote, paper_fee, paper_slippage);

        // Generic / live path: fee = 25 bps per side, same slippage
        let generic_fee = effective_venue_fee_pct(false, dec!(0.0025));
        let generic_cost = directional::round_trip_cost(&quote, generic_fee, paper_slippage);

        // Paper cost is positive — spread and slippage are not zeroed.
        assert!(
            paper_cost > Decimal::ZERO,
            "spread and slippage must contribute to the paper cost (got {paper_cost})"
        );

        // Generic cost is higher than paper because it includes the 25-bps
        // commission; the difference equals two sides of commission.
        assert!(
            generic_cost > paper_cost,
            "live/other-venue cost must exceed paper cost by at least the commission \
             (paper {paper_cost}, generic {generic_cost})"
        );
    }

    #[test]
    fn us_paper_strategy_rejects_penny_stocks_and_wide_spreads() {
        let candles = us_breakout_candles();
        let penny_stock = us_equity("PLUG");
        let cheap_quote = us_quote(dec!(5.0), dec!(0.01));
        let cheap_view = generate_us_momentum_view(&penny_stock, &cheap_quote, &candles);
        assert_eq!(cheap_view.direction, Direction::Flat);
        assert_eq!(cheap_view.key_factors, vec!["price_floor"]);

        let liquid_stock = us_equity("SPY");
        let mid = candles.last().unwrap().close;
        let wide_quote = us_quote(mid, mid * dec!(0.01));
        let wide_view = generate_us_momentum_view(&liquid_stock, &wide_quote, &candles);
        assert_eq!(wide_view.direction, Direction::Flat);
        assert_eq!(wide_view.key_factors, vec!["spread_filter"]);
    }

    #[test]
    fn us_paper_strategy_rejects_incomplete_history() {
        let candles: Vec<Candle> = us_breakout_candles().into_iter().take(29).collect();
        let instrument = us_equity("SPY");
        let quote = us_quote(candles.last().unwrap().close, dec!(0.12));

        let view = generate_us_momentum_view(&instrument, &quote, &candles);
        assert_eq!(view.direction, Direction::Flat);
        assert_eq!(view.key_factors, vec!["insufficient_history"]);
    }

    #[test]
    fn us_paper_strategy_rejects_failed_momentum_filters() {
        let instrument = us_equity("SPY");

        // 1. Weak volume (below 1.1x 20-bar avg)
        let mut weak_volume_candles = us_breakout_candles();
        weak_volume_candles.last_mut().unwrap().volume = dec!(1000);
        let quote = us_quote(weak_volume_candles.last().unwrap().close, dec!(0.12));
        let view = generate_us_momentum_view(&instrument, &quote, &weak_volume_candles);
        assert_eq!(view.direction, Direction::Flat);
        assert_eq!(view.key_factors, vec!["momentum_filters_not_met"]);

        // 2. No resistance breakout (close <= prior 10-bar high)
        let mut no_breakout_candles = us_breakout_candles();
        let prior_high = no_breakout_candles[29..39]
            .iter()
            .map(|c| c.high)
            .max()
            .unwrap();
        no_breakout_candles.last_mut().unwrap().close = prior_high - dec!(0.10);
        let quote = us_quote(no_breakout_candles.last().unwrap().close, dec!(0.12));
        let view = generate_us_momentum_view(&instrument, &quote, &no_breakout_candles);
        assert_eq!(view.direction, Direction::Flat);
        assert_eq!(view.key_factors, vec!["momentum_filters_not_met"]);

        // 3. Price below SMA10
        let mut below_sma_candles = us_breakout_candles();
        below_sma_candles.last_mut().unwrap().close = dec!(90.0);
        let quote = us_quote(below_sma_candles.last().unwrap().close, dec!(0.12));
        let view = generate_us_momentum_view(&instrument, &quote, &below_sma_candles);
        assert_eq!(view.direction, Direction::Flat);
        assert_eq!(view.key_factors, vec!["momentum_filters_not_met"]);

        // 4. SMA10 <= SMA30 (bearish trend)
        let now = Utc::now();
        let bear_candles: Vec<Candle> = (0..40)
            .map(|i| {
                let close = dec!(150.0) - Decimal::from(i) * dec!(0.5);
                Candle {
                    ts: now - Duration::hours((40 - i) as i64),
                    open: close + dec!(0.2),
                    high: close + dec!(0.5),
                    low: close - dec!(0.5),
                    close,
                    volume: dec!(1200),
                }
            })
            .collect();
        let quote = us_quote(bear_candles.last().unwrap().close, dec!(0.12));
        let view = generate_us_momentum_view(&instrument, &quote, &bear_candles);
        assert_eq!(view.direction, Direction::Flat);
        assert_eq!(view.key_factors, vec!["momentum_filters_not_met"]);

        // 5. RSI > 68 (overbought filter)
        let mut overbought_candles = us_breakout_candles();
        let len = overbought_candles.len();
        for j in 0..14 {
            let idx = len - 14 + j;
            overbought_candles[idx].close += Decimal::from(j + 1) * dec!(2.5);
            overbought_candles[idx].high += Decimal::from(j + 1) * dec!(3.0);
        }
        let quote = us_quote(overbought_candles.last().unwrap().close, dec!(0.12));
        let view = generate_us_momentum_view(&instrument, &quote, &overbought_candles);
        assert_eq!(view.direction, Direction::Flat);
        assert_eq!(view.key_factors, vec!["momentum_filters_not_met"]);
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
