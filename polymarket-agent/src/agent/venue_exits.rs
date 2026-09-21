//! Exits for continuous-asset positions.
//!
//! Prediction markets resolve: hold to expiry and the contract settles itself.
//! Equities and crypto never do, so a position opened by the venue cycle stays
//! open until something closes it. Without this the agent accumulates
//! unbounded exposure with no stop — strictly worse than not trading at all.
//!
//! Positions are marked to market every cycle (so the survival check and
//! dashboard see real numbers rather than entry cost) and closed when the stop,
//! the target, or the maximum holding period is reached.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use tracing::{info, instrument, warn};
use uuid::Uuid;

use crate::db::store::{OrderRecord, Store, VenueOpenTrade};
use crate::venue::types::{
    Instrument, InstrumentId, OrderKind, OrderRequest, OrderState, Side, TimeInForce, VenueId,
};
use crate::venue::{Venue, VenueRegistry};

/// Why a position is being closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    StopLoss,
    TakeProfit,
    MaxHold,
}

impl ExitReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExitReason::StopLoss => "STOP_LOSS",
            ExitReason::TakeProfit => "TAKE_PROFIT",
            ExitReason::MaxHold => "MAX_HOLD",
        }
    }
}

/// Rules applied to an open position, resolved from the trade and config.
#[derive(Debug, Clone, Copy)]
pub struct ExitRules {
    pub stop_price: Option<Decimal>,
    pub target_price: Option<Decimal>,
    pub max_hold_until: Option<DateTime<Utc>>,
}

/// Decide whether an open long should be closed at the current mark.
///
/// Stop is checked before target: if a bar straddles both, assuming the good
/// outcome would flatter every backtest and every logged P&L.
pub fn evaluate_exit(
    mark_price: Decimal,
    now: DateTime<Utc>,
    rules: &ExitRules,
) -> Option<ExitReason> {
    if let Some(stop) = rules.stop_price {
        if mark_price <= stop {
            return Some(ExitReason::StopLoss);
        }
    }
    if let Some(target) = rules.target_price {
        if mark_price >= target {
            return Some(ExitReason::TakeProfit);
        }
    }
    if let Some(deadline) = rules.max_hold_until {
        if now >= deadline {
            return Some(ExitReason::MaxHold);
        }
    }
    None
}

/// Unrealised P&L of a long position at the current mark.
pub fn unrealized_pnl(entry: Decimal, mark: Decimal, qty: Decimal) -> Decimal {
    (mark - entry) * qty
}

/// Mark open continuous positions to market and close any that have hit an
/// exit condition.
pub struct VenueExits<'a> {
    pub registry: &'a VenueRegistry,
    pub store: &'a Store,
    /// Hours after entry at which a position is closed regardless.
    pub max_hold_hours: i64,
    /// How long an exit order may rest before reconciliation cancels it.
    pub order_ttl_seconds: i64,
}

impl VenueExits<'_> {
    #[instrument(skip(self), fields(otel.name = "venue.exits", cycle = cycle), err)]
    pub async fn run(&self, now: DateTime<Utc>, cycle: i64) -> Result<usize> {
        let open = self.store.get_open_venue_trades().await?;
        let mut closed = 0usize;

        // Trades whose exit order is still live. Without this the pass
        // re-submits a full-size SELL on every cycle for as long as the first
        // one rests unfilled, and a one-unit long becomes a multi-unit short
        // as they fill. The entry side already guards this way in
        // `VenueCycle::run`; the exit side needs it just as much, and the
        // consequence here is worse — an entry duplicate is an unwanted
        // position, a duplicate exit inverts the one you have.
        let exiting: HashSet<i64> = self
            .store
            .get_unresolved_orders()
            .await?
            .into_iter()
            .filter(|o| o.intent == "EXIT")
            .filter_map(|o| o.trade_id)
            .collect();

        let mut instruments: HashMap<String, Vec<Instrument>> = HashMap::new();

        for trade in open {
            if exiting.contains(&trade.id) {
                // Still mark it to market — an exit in flight does not stop
                // the position needing a current valuation — but do not act.
                if let Err(e) = self.mark_only(&trade, now).await {
                    warn!(
                        trade_id = trade.id,
                        error = %e,
                        "Could not mark a position whose exit is already resting"
                    );
                }
                continue;
            }
            match self.process(&trade, now, cycle, &mut instruments).await {
                Ok(true) => closed += 1,
                Ok(false) => {}
                Err(e) => warn!(
                    trade_id = trade.id,
                    symbol = %trade.symbol,
                    error = %e,
                    "Exit evaluation failed — position left open"
                ),
            }
        }

        Ok(closed)
    }

    /// Mark a position to market without evaluating an exit. Used when an
    /// exit order is already resting: the valuation is still wanted, the
    /// second order is not.
    async fn mark_only(&self, trade: &VenueOpenTrade, now: DateTime<Utc>) -> Result<()> {
        let venue_id = VenueId::new(trade.venue_id.clone());
        let Some(venue) = self.registry.get(&venue_id) else {
            return Ok(());
        };
        let instrument_id = InstrumentId::new(venue_id, trade.symbol.clone());
        let quote = venue.quote(&instrument_id).await?;
        let mark = quote.taker_price(Side::Sell);
        let entry = trade.avg_fill_price.unwrap_or(trade.entry_price);
        let pnl = unrealized_pnl(entry, mark, trade.quantity);
        self.store
            .mark_trade(trade.id, &mark.to_string(), &pnl.to_string(), now)
            .await
    }

    /// Returns whether an exit order was submitted.
    async fn process(
        &self,
        trade: &VenueOpenTrade,
        now: DateTime<Utc>,
        cycle: i64,
        instruments: &mut HashMap<String, Vec<Instrument>>,
    ) -> Result<bool> {
        let venue_id = VenueId::new(trade.venue_id.clone());
        let Some(venue) = self.registry.get(&venue_id) else {
            // The venue that opened this position is no longer configured, so
            // it cannot be priced or closed. Say so rather than pretend.
            warn!(
                trade_id = trade.id,
                venue = %trade.venue_id,
                "Open position on an unconfigured venue — cannot mark or exit it"
            );
            return Ok(false);
        };

        let instrument_id = InstrumentId::new(venue_id, trade.symbol.clone());
        let quote = venue.quote(&instrument_id).await?;
        // Value a long at what it could be sold for, not the midpoint.
        let mark = quote.taker_price(Side::Sell);

        let entry: Decimal = trade.avg_fill_price.unwrap_or(trade.entry_price);
        let qty = trade.quantity;
        let pnl = unrealized_pnl(entry, mark, qty);

        self.store
            .mark_trade(trade.id, &mark.to_string(), &pnl.to_string(), now)
            .await?;

        let rules = ExitRules {
            stop_price: trade.stop_price,
            target_price: trade.target_price,
            max_hold_until: trade
                .opened_at
                .map(|opened| opened + Duration::hours(self.max_hold_hours)),
        };

        let Some(reason) = evaluate_exit(mark, now, &rules) else {
            return Ok(false);
        };

        info!(
            trade_id = trade.id,
            symbol = %trade.symbol,
            reason = reason.as_str(),
            entry = %entry,
            mark = %mark,
            unrealized_pnl = %pnl,
            "Exit triggered"
        );

        self.submit_exit(
            venue,
            trade,
            &instrument_id,
            mark,
            qty,
            reason,
            cycle,
            instruments,
        )
        .await?;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    async fn submit_exit(
        &self,
        venue: &dyn Venue,
        trade: &VenueOpenTrade,
        instrument_id: &InstrumentId,
        mark: Decimal,
        qty: Decimal,
        reason: ExitReason,
        cycle: i64,
        instruments: &mut HashMap<String, Vec<Instrument>>,
    ) -> Result<()> {
        // Re-resolve the instrument so lot/tick rules come from the venue
        // rather than being reconstructed from the stored row.
        let instrument = self.resolve(venue, instrument_id, instruments).await?;
        let limit_price = instrument.round_price(mark, Side::Sell);
        let client_order_id = Uuid::new_v4().to_string();

        self.store
            .insert_order(&OrderRecord {
                id: None,
                client_order_id: client_order_id.clone(),
                venue_order_id: None,
                venue_id: trade.venue_id.clone(),
                symbol: trade.symbol.clone(),
                side: Side::Sell.to_string(),
                intent: "EXIT".to_string(),
                trade_id: Some(trade.id),
                limit_price: Some(limit_price.to_string()),
                qty: qty.to_string(),
                filled_qty: "0".to_string(),
                avg_fill_price: None,
                state: "PENDING".to_string(),
                reject_reason: Some(reason.as_str().to_string()),
                cycle: Some(cycle),
                // Exits are priced off the mark, which is the mid.
                mid_at_submit: Some(mark.to_string()),
                submitted_at: None,
                updated_at: None,
                expires_at: Some(
                    (Utc::now() + Duration::seconds(self.order_ttl_seconds)).to_rfc3339(),
                ),
            })
            .await?;

        let request = OrderRequest {
            instrument,
            side: Side::Sell,
            kind: OrderKind::Limit { price: limit_price },
            qty,
            tif: TimeInForce::Gtc,
            extended_hours: false,
            client_order_id: client_order_id.clone(),
        };

        let ack = match venue.place_order(&request).await {
            Ok(ack) => ack,
            Err(e) => {
                // The position stays OPEN: an exit we cannot confirm has not
                // happened, and marking it closed would hide live exposure.
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
                return Err(e);
            }
        };

        // The venue's own state decides this, not whether *some* quantity
        // filled. Treating a partial fill as FILLED marks the order terminal,
        // so `get_unresolved_orders` drops it and reconciliation never looks
        // again — stranding the unsold remainder both at the venue and off
        // the ledger. `reconcile::close_on_exit_fill` already insists on a
        // full fill; this path has to agree with it.
        let state = match ack.state {
            OrderState::Filled => "FILLED",
            OrderState::PartiallyFilled => "PARTIALLY_FILLED",
            OrderState::Rejected(_) => "REJECTED",
            OrderState::Cancelled => "CANCELLED",
            OrderState::Expired => "EXPIRED",
            OrderState::Unknown => "UNKNOWN",
            OrderState::Accepted => "ACCEPTED",
        };
        self.store
            .update_order_state(
                &client_order_id,
                state,
                Some(&ack.venue_order_id),
                &ack.filled_qty.to_string(),
                ack.avg_fill_price.map(|p| p.to_string()).as_deref(),
                None,
            )
            .await?;

        // Only a *complete* fill closes the position. Anything less leaves the
        // trade open with its order still unresolved, which is what the next
        // reconciliation pass is for.
        if matches!(ack.state, OrderState::Filled) && ack.filled_qty > Decimal::ZERO {
            let exit_price = ack.avg_fill_price.unwrap_or(limit_price);
            let entry = trade.avg_fill_price.unwrap_or(trade.entry_price);
            let realized = unrealized_pnl(entry, exit_price, ack.filled_qty);
            self.store
                .close_trade(
                    trade.id,
                    &exit_price.to_string(),
                    &realized.to_string(),
                    reason.as_str(),
                    &client_order_id,
                )
                .await?;
            // Score the forecast that opened this position: it said the
            // price would be higher at the horizon, and now we know.
            if let Err(e) = crate::valuation::calibration::resolve_directional_prediction(
                self.store.pool(),
                &trade.venue_id,
                &trade.symbol,
                entry,
                exit_price,
            )
            .await
            {
                warn!(trade_id = trade.id, error = %e, "Could not score the forecast");
            }

            info!(
                trade_id = trade.id,
                realized_pnl = %realized,
                reason = reason.as_str(),
                "Position closed"
            );
        }

        Ok(())
    }

    /// The instrument, for its tick and lot rules.
    ///
    /// Cached for the pass. Each call is a full instrument listing at the
    /// venue — for Alpaca a `/v2/assets` fetch — and it used to run once per
    /// exiting position, discarding all but one row. Ten positions leaving in
    /// one cycle meant ten whole-universe downloads on top of ten quotes,
    /// against an API whose rate limit the agent is already budgeting for.
    async fn resolve(
        &self,
        venue: &dyn Venue,
        id: &InstrumentId,
        cache: &mut HashMap<String, Vec<Instrument>>,
    ) -> Result<Instrument> {
        let key = venue.id().to_string();
        if !cache.contains_key(&key) {
            let listed = venue
                .list_instruments(&crate::venue::types::ScanFilter::default())
                .await?;
            cache.insert(key.clone(), listed);
        }
        cache
            .get(&key)
            .and_then(|list| list.iter().find(|i| &i.id == id).cloned())
            .ok_or_else(|| anyhow::anyhow!("Instrument {id} is no longer listed — cannot exit it"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::store::{OrderRecord, VenueTradeRecord};
    use crate::venue::session::TradingSession;
    use crate::venue::test_support::StubVenue;
    use crate::venue::types::{AssetClass, OrderAck};
    use crate::venue::VenueRegistry;
    use rust_decimal_macros::dec;

    async fn store_with_open_position() -> Store {
        let store = Store::new(":memory:").await.expect("in-memory store");
        store
            .insert_venue_trade(&VenueTradeRecord {
                cycle: 1,
                venue_id: "stub".to_string(),
                symbol: "BTC/USD".to_string(),
                asset_class: "crypto_spot".to_string(),
                display_name: Some("BTC/USD".to_string()),
                side: "BUY".to_string(),
                entry_price: "100".to_string(),
                quantity: "1".to_string(),
                notional: "100".to_string(),
                avg_fill_price: Some("100".to_string()),
                edge_at_entry: "0.05".to_string(),
                fair_value: "104".to_string(),
                confidence: "0.8".to_string(),
                risk_pct: "0.0075".to_string(),
                stop_pct: "0.03".to_string(),
                status: "OPEN".to_string(),
                stop_price: Some("97".to_string()),
                target_price: Some("110".to_string()),
                horizon_hours: Some(24),
                client_order_id: Some("entry-1".to_string()),
                venue_order_id: Some("v-entry-1".to_string()),
            })
            .await
            .expect("insert position");
        store
    }

    fn registry_at(mark: Decimal, ack: OrderAck) -> VenueRegistry {
        VenueRegistry::new(vec![Box::new(
            StubVenue::with_classes(
                "stub",
                TradingSession::Always,
                &[("BTC/USD", AssetClass::CryptoSpot)],
                false,
            )
            .quoting(mark)
            .acking(ack),
        )])
    }

    fn ack(state: OrderState, filled: Decimal) -> OrderAck {
        OrderAck {
            venue_order_id: "v-exit-1".to_string(),
            client_order_id: String::new(),
            state,
            filled_qty: filled,
            avg_fill_price: if filled > Decimal::ZERO {
                Some(dec!(96))
            } else {
                None
            },
            fees: Decimal::ZERO,
        }
    }

    /// The worst bug this pass can have: with an exit already resting, a
    /// stopped-out position was re-sold every cycle, so a one-unit long
    /// became a multi-unit short as the duplicates filled.
    #[tokio::test]
    async fn an_exit_already_resting_is_not_submitted_again() {
        let store = store_with_open_position().await;
        // Mark below the 97 stop, so an exit is due on every pass.
        let registry = registry_at(dec!(90), ack(OrderState::Accepted, Decimal::ZERO));
        let exits = VenueExits {
            registry: &registry,
            store: &store,
            max_hold_hours: 72,
            order_ttl_seconds: 300,
        };
        exits.run(Utc::now(), 1).await.expect("first pass");
        let after_first = store.get_unresolved_orders().await.unwrap().len();
        assert_eq!(after_first, 1, "the exit order should be on the books");

        // Second pass: the exit is still resting and the stop is still breached.
        exits.run(Utc::now(), 2).await.expect("second pass");
        let after_second = store.get_unresolved_orders().await.unwrap();
        assert_eq!(
            after_second.len(),
            1,
            "a second exit was submitted while the first was still live: {after_second:?}"
        );
    }

    /// A partial fill leaves units at the venue. Closing the trade on it
    /// strands them: the order goes terminal so reconciliation stops looking,
    /// and the ledger says flat while the position is not.
    #[tokio::test]
    async fn a_partially_filled_exit_does_not_close_the_trade() {
        let store = store_with_open_position().await;
        let registry = registry_at(dec!(90), ack(OrderState::PartiallyFilled, dec!(0.3)));
        let exits = VenueExits {
            registry: &registry,
            store: &store,
            max_hold_hours: 72,
            order_ttl_seconds: 300,
        };
        exits.run(Utc::now(), 1).await.expect("pass");

        let still_open = store.get_open_venue_trades().await.unwrap();
        assert_eq!(still_open.len(), 1, "a 0.3 fill of 1.0 is not a close");

        let orders = store.get_unresolved_orders().await.unwrap();
        assert_eq!(
            orders.len(),
            1,
            "a partially filled order must stay unresolved so reconciliation revisits it"
        );
        assert_eq!(orders[0].state, "PARTIALLY_FILLED");
    }

    /// The exit reason is written when the order is placed and read back when
    /// it fills. A later update must not erase it, or every exit closes as a
    /// generic "EXIT" and why the agent sold is lost.
    #[tokio::test]
    async fn the_exit_reason_survives_a_later_state_update() {
        // orders.trade_id is a foreign key, so the position has to exist.
        let store = store_with_open_position().await;
        let trade_id = store.get_open_venue_trades().await.unwrap()[0].id;
        store
            .insert_order(&OrderRecord {
                id: None,
                client_order_id: "exit-1".to_string(),
                venue_order_id: None,
                venue_id: "stub".to_string(),
                symbol: "BTC/USD".to_string(),
                side: "SELL".to_string(),
                intent: "EXIT".to_string(),
                trade_id: Some(trade_id),
                limit_price: Some("96".to_string()),
                qty: "1".to_string(),
                filled_qty: "0".to_string(),
                avg_fill_price: None,
                state: "PENDING".to_string(),
                reject_reason: Some("STOP_LOSS".to_string()),
                cycle: Some(1),
                mid_at_submit: None,
                submitted_at: None,
                updated_at: None,
                expires_at: None,
            })
            .await
            .expect("insert order");

        // The ack path passes None for both optional fields.
        store
            .update_order_state("exit-1", "ACCEPTED", Some("v-1"), "0", None, None)
            .await
            .expect("update");

        let order = store
            .get_unresolved_orders()
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.client_order_id == "exit-1")
            .expect("order");
        assert_eq!(order.reject_reason.as_deref(), Some("STOP_LOSS"));
    }

    fn rules(
        stop: Option<Decimal>,
        target: Option<Decimal>,
        deadline: Option<DateTime<Utc>>,
    ) -> ExitRules {
        ExitRules {
            stop_price: stop,
            target_price: target,
            max_hold_until: deadline,
        }
    }

    #[test]
    fn stop_loss_triggers_at_or_below_the_stop() {
        let r = rules(Some(dec!(95)), None, None);
        assert_eq!(
            evaluate_exit(dec!(94), Utc::now(), &r),
            Some(ExitReason::StopLoss)
        );
        // Boundary is inclusive — at the stop is stopped out.
        assert_eq!(
            evaluate_exit(dec!(95), Utc::now(), &r),
            Some(ExitReason::StopLoss)
        );
        assert_eq!(evaluate_exit(dec!(96), Utc::now(), &r), None);
    }

    #[test]
    fn take_profit_triggers_at_or_above_the_target() {
        let r = rules(None, Some(dec!(110)), None);
        assert_eq!(
            evaluate_exit(dec!(110), Utc::now(), &r),
            Some(ExitReason::TakeProfit)
        );
        assert_eq!(evaluate_exit(dec!(109), Utc::now(), &r), None);
    }

    #[test]
    fn stop_wins_when_both_would_trigger() {
        // A move that spans both levels must not be recorded as the profitable
        // one — that would flatter every P&L figure the agent reports.
        let r = rules(Some(dec!(95)), Some(dec!(90)), None);
        assert_eq!(
            evaluate_exit(dec!(94), Utc::now(), &r),
            Some(ExitReason::StopLoss)
        );
    }

    #[test]
    fn max_hold_closes_a_position_that_has_gone_nowhere() {
        let now = Utc::now();
        let r = rules(
            Some(dec!(90)),
            Some(dec!(110)),
            Some(now - Duration::hours(1)),
        );
        // Price is between stop and target, but the deadline has passed.
        assert_eq!(evaluate_exit(dec!(100), now, &r), Some(ExitReason::MaxHold));

        let not_yet = rules(None, None, Some(now + Duration::hours(1)));
        assert_eq!(evaluate_exit(dec!(100), now, &not_yet), None);
    }

    #[test]
    fn a_position_with_no_rules_is_never_auto_closed() {
        assert_eq!(
            evaluate_exit(dec!(100), Utc::now(), &rules(None, None, None)),
            None
        );
    }

    #[test]
    fn unrealized_pnl_follows_the_mark() {
        // 10 units bought at 100, now worth 105.
        assert_eq!(unrealized_pnl(dec!(100), dec!(105), dec!(10)), dec!(50));
        // And losses are negative rather than absolute.
        assert_eq!(unrealized_pnl(dec!(100), dec!(95), dec!(10)), dec!(-50));
        assert_eq!(
            unrealized_pnl(dec!(100), dec!(100), dec!(10)),
            Decimal::ZERO
        );
    }

    #[test]
    fn exit_reasons_have_stable_stored_names() {
        assert_eq!(ExitReason::StopLoss.as_str(), "STOP_LOSS");
        assert_eq!(ExitReason::TakeProfit.as_str(), "TAKE_PROFIT");
        assert_eq!(ExitReason::MaxHold.as_str(), "MAX_HOLD");
    }
}
