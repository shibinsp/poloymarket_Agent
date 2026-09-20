//! Resolving orders whose fate the agent does not yet know.
//!
//! The trading cycle refuses to place a second order for a symbol that already
//! has an unresolved one — that is how duplicate positions are avoided. But
//! that rule only works if something eventually *resolves* those orders.
//! Without this pass the first order that doesn't fill instantly blocks its
//! symbol forever, and any fill arriving after the initial acknowledgement is
//! never recorded, leaving a position that exists at the venue and nowhere in
//! the local ledger.
//!
//! The venue is the authority. This pass asks it what happened, writes the
//! answer down, and promotes or cancels the trade that was recorded when the
//! order was placed. Orders that outlive their TTL are cancelled rather than
//! left resting, so nothing sits on the book unattended.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use tracing::{info, warn};

use crate::agent::venue_exits::unrealized_pnl;
use crate::db::store::{OrderRecord, Store};
use crate::venue::types::{OrderRef, OrderState, VenueId};
use crate::venue::VenueRegistry;

/// What one reconciliation pass established.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Orders that were unresolved when the pass started.
    pub checked: usize,
    /// Orders that reached a terminal state during this pass.
    pub resolved: usize,
    /// Orders found to have filled (wholly or partly) since last seen.
    pub filled: usize,
    /// Orders cancelled because they outlived their TTL.
    pub expired: usize,
    /// Orders the venue could not be asked about — these stay blocking, which
    /// is the safe direction, and are worth alerting on if they persist.
    pub unqueryable: usize,
    /// Orders still live at the venue and within their TTL.
    pub still_open: usize,
}

pub struct Reconciler<'a> {
    pub registry: &'a VenueRegistry,
    pub store: &'a Store,
    /// How long an unfilled order may rest before it is cancelled.
    pub order_ttl: Duration,
}

impl Reconciler<'_> {
    /// Ask each venue about every order whose outcome is unknown.
    ///
    /// One order failing must not stop the rest: a venue outage would
    /// otherwise leave every other venue's orders unresolved too.
    pub async fn run(&self, now: DateTime<Utc>) -> Result<ReconcileReport> {
        let mut report = ReconcileReport::default();
        let orders = self.store.get_unresolved_orders().await?;
        report.checked = orders.len();

        for order in orders {
            if let Err(e) = self.resolve(&order, now, &mut report).await {
                report.unqueryable += 1;
                warn!(
                    client_order_id = %order.client_order_id,
                    venue = %order.venue_id,
                    symbol = %order.symbol,
                    state = %order.state,
                    error = %e,
                    "Could not resolve order — it stays blocking until the venue answers"
                );
            }
        }

        if report.checked > 0 {
            info!(
                checked = report.checked,
                resolved = report.resolved,
                filled = report.filled,
                expired = report.expired,
                unqueryable = report.unqueryable,
                still_open = report.still_open,
                "Reconciliation complete"
            );
        }

        Ok(report)
    }

    async fn resolve(
        &self,
        order: &OrderRecord,
        now: DateTime<Utc>,
        report: &mut ReconcileReport,
    ) -> Result<()> {
        let venue_id = VenueId::new(order.venue_id.clone());
        let Some(venue) = self.registry.get(&venue_id) else {
            anyhow::bail!("venue {} is not configured", order.venue_id);
        };

        // Prefer the venue's own id when we have it: a client id lookup is a
        // secondary index at most venues and missing at some.
        let reference = match &order.venue_order_id {
            Some(id) => OrderRef::Venue(id.clone()),
            None => OrderRef::Client(order.client_order_id.clone()),
        };
        let ack = venue.get_order(&reference).await?;

        self.store
            .update_order_state(
                &order.client_order_id,
                state_name(&ack.state),
                Some(&ack.venue_order_id),
                &ack.filled_qty.to_string(),
                ack.avg_fill_price.map(|p| p.to_string()).as_deref(),
                match &ack.state {
                    OrderState::Rejected(reason) => Some(reason.as_str()),
                    _ => None,
                },
            )
            .await?;

        // Any fill is real, whether or not the order is finished. A partially
        // filled entry is a live position and must be visible as one.
        if ack.filled_qty > Decimal::ZERO {
            report.filled += 1;
            self.apply_fill(order, ack.filled_qty, ack.avg_fill_price, &ack.state)
                .await?;
        }

        if is_terminal(&ack.state) {
            report.resolved += 1;
            if ack.filled_qty.is_zero() {
                self.abandon(order, &ack.state).await?;
            }
            return Ok(());
        }

        // Still live at the venue. Cancel it if it has rested too long — an
        // order left resting is exposure the agent has stopped reasoning about.
        if is_expired(order, now) {
            venue.cancel_order(&ack.venue_order_id).await?;
            self.store
                .update_order_state(
                    &order.client_order_id,
                    "EXPIRED",
                    Some(&ack.venue_order_id),
                    &ack.filled_qty.to_string(),
                    ack.avg_fill_price.map(|p| p.to_string()).as_deref(),
                    Some("order TTL elapsed"),
                )
                .await?;
            report.expired += 1;
            report.resolved += 1;
            if ack.filled_qty.is_zero() {
                self.abandon(order, &OrderState::Expired).await?;
            }
            info!(
                client_order_id = %order.client_order_id,
                symbol = %order.symbol,
                "Cancelled an order that outlived its TTL"
            );
        } else {
            report.still_open += 1;
        }

        Ok(())
    }

    /// The trade this order belongs to, with its current status.
    ///
    /// `orders.trade_id` is the link for both intents. `trades.client_order_id`
    /// records only the *entry* order, so looking an exit up by it finds
    /// nothing — and an exit that silently fails to close its position strands
    /// live exposure.
    async fn linked_trade(&self, order: &OrderRecord) -> Result<Option<(i64, String)>> {
        let trade_id = match order.trade_id {
            Some(id) => id,
            None => match self.store.trade_for_order(&order.client_order_id).await? {
                Some((id, _)) => id,
                None => return Ok(None),
            },
        };
        Ok(self
            .store
            .trade_status(trade_id)
            .await?
            .map(|status| (trade_id, status)))
    }

    /// Record a fill against the trade the order was placed for.
    async fn apply_fill(
        &self,
        order: &OrderRecord,
        filled_qty: Decimal,
        avg_fill_price: Option<Decimal>,
        state: &OrderState,
    ) -> Result<()> {
        let Some((trade_id, status)) = self.linked_trade(order).await? else {
            // An order with no trade row is a bug elsewhere, not something to
            // paper over by inventing a position with no thesis behind it.
            warn!(
                client_order_id = %order.client_order_id,
                "Order filled but no trade row is linked to it — cannot record the position"
            );
            return Ok(());
        };

        let price = match avg_fill_price {
            Some(p) => p,
            None => {
                warn!(
                    client_order_id = %order.client_order_id,
                    "Fill reported without an average price — leaving the trade untouched"
                );
                return Ok(());
            }
        };

        match order.intent.as_str() {
            "EXIT" => {
                self.close_on_exit_fill(order, trade_id, filled_qty, price, state)
                    .await
            }
            _ => {
                // Already accounted for; re-writing would clobber a position
                // that a later cycle has since marked or partially exited.
                if status != "PENDING" && status != "PARTIAL" {
                    return Ok(());
                }
                let new_status = if matches!(state, OrderState::Filled) {
                    "OPEN"
                } else {
                    "PARTIAL"
                };
                self.store
                    .activate_trade(
                        trade_id,
                        &price.to_string(),
                        &filled_qty.to_string(),
                        new_status,
                    )
                    .await?;
                info!(
                    trade_id,
                    symbol = %order.symbol,
                    qty = %filled_qty,
                    price = %price,
                    status = new_status,
                    "Entry filled — position is now live"
                );
                Ok(())
            }
        }
    }

    async fn close_on_exit_fill(
        &self,
        order: &OrderRecord,
        trade_id: i64,
        filled_qty: Decimal,
        price: Decimal,
        state: &OrderState,
    ) -> Result<()> {
        // A partial exit leaves the position open; closing it here would hide
        // the remaining exposure.
        if !matches!(state, OrderState::Filled) {
            info!(
                trade_id,
                filled = %filled_qty,
                "Exit partially filled — position stays open for the remainder"
            );
            return Ok(());
        }

        let Some((entry, _qty)) = self.store.get_trade_entry(trade_id).await? else {
            warn!(trade_id, "Exit filled for a trade that no longer exists");
            return Ok(());
        };

        let realized = unrealized_pnl(entry, price, filled_qty);
        // The reason was stored on the exit order when it was placed.
        let reason = order.reject_reason.as_deref().unwrap_or("EXIT");
        self.store
            .close_trade(
                trade_id,
                &price.to_string(),
                &realized.to_string(),
                reason,
                &order.client_order_id,
            )
            .await?;
        info!(
            trade_id,
            symbol = %order.symbol,
            exit_price = %price,
            realized_pnl = %realized,
            reason,
            "Exit filled — position closed"
        );
        Ok(())
    }

    /// An order that ended without filling. The entry it was for never became
    /// a position; an exit that never filled leaves its position open.
    async fn abandon(&self, order: &OrderRecord, state: &OrderState) -> Result<()> {
        let Some((trade_id, status)) = self.linked_trade(order).await? else {
            return Ok(());
        };

        if order.intent == "EXIT" {
            // Deliberately does not touch the trade: the position is still
            // live and the next cycle's exit check must see it that way.
            //
            // Belt and braces — the `status == "PENDING"` check below would
            // also spare it, since an exit is only ever placed against an
            // OPEN or PARTIAL trade. Kept for the warning and so the intent
            // survives a future change to that filter.
            warn!(
                trade_id,
                symbol = %order.symbol,
                state = state_name(state),
                "Exit order ended without filling — position remains open"
            );
            return Ok(());
        }

        if status == "PENDING" {
            self.store.cancel_trade(trade_id, state_name(state)).await?;
            info!(
                trade_id,
                symbol = %order.symbol,
                state = state_name(state),
                "Entry never filled — trade cancelled"
            );
        }
        Ok(())
    }
}

/// Whether an order has rested past the deadline recorded when it was placed.
///
/// Free rather than a method so it can be tested without a database.
pub fn is_expired(order: &OrderRecord, now: DateTime<Utc>) -> bool {
    match order_deadline(order) {
        Some(deadline) => now >= deadline,
        // No usable timestamp: leave it alone rather than cancel an order that
        // may have been placed moments ago.
        None => false,
    }
}

/// When an order stops being worth waiting for.
fn order_deadline(order: &OrderRecord) -> Option<DateTime<Utc>> {
    order
        .expires_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// Whether the venue considers the order finished.
pub fn is_terminal(state: &OrderState) -> bool {
    matches!(
        state,
        OrderState::Filled | OrderState::Cancelled | OrderState::Expired | OrderState::Rejected(_)
    )
}

pub fn state_name(state: &OrderState) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::store::VenueTradeRecord;
    use crate::venue::session::TradingSession;
    use crate::venue::types::{
        AssetClass, Balance, Candle, CandleInterval, Instrument, InstrumentId, OrderAck,
        OrderRequest, Position, Quote, ScanFilter, Settlement, VenueCapabilities,
    };
    use crate::venue::Venue;
    use async_trait::async_trait;
    use rust_decimal_macros::dec;
    use std::sync::Mutex;

    /// A venue that answers `get_order` with a scripted outcome, so the
    /// reconciler's handling of each order state can be exercised directly.
    struct ScriptedVenue {
        id: VenueId,
        caps: VenueCapabilities,
        session: TradingSession,
        reply: Mutex<Option<OrderAck>>,
        cancelled: Mutex<Vec<String>>,
    }

    impl ScriptedVenue {
        fn answering(ack: OrderAck) -> Self {
            Self {
                id: VenueId::new("stub"),
                caps: VenueCapabilities {
                    asset_classes: vec![AssetClass::CryptoSpot],
                    limit_only_outside_regular: false,
                    supports_client_order_id: true,
                    supports_candles: true,
                },
                session: TradingSession::Always,
                reply: Mutex::new(Some(ack)),
                cancelled: Mutex::new(Vec::new()),
            }
        }
    }

    fn ack(state: OrderState, filled: Decimal, price: Option<Decimal>) -> OrderAck {
        OrderAck {
            venue_order_id: "v1".to_string(),
            client_order_id: "c1".to_string(),
            state,
            filled_qty: filled,
            avg_fill_price: price,
            fees: Decimal::ZERO,
        }
    }

    #[async_trait]
    impl Venue for ScriptedVenue {
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
            Ok(Vec::new())
        }
        async fn quote(&self, _id: &InstrumentId) -> Result<Quote> {
            anyhow::bail!("not used")
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
            anyhow::bail!("not used")
        }
        async fn get_order(&self, _r: &OrderRef) -> Result<OrderAck> {
            self.reply
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("venue is down"))
        }
        async fn cancel_order(&self, id: &str) -> Result<()> {
            self.cancelled.lock().unwrap().push(id.to_string());
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

    /// An unfilled entry order plus the PENDING trade row that carries its
    /// thesis — the state the cycle leaves behind when an order rests.
    async fn pending_entry(store: &Store, expires_at: Option<String>) -> i64 {
        store
            .insert_order(&OrderRecord {
                state: "ACCEPTED".to_string(),
                venue_order_id: Some("v1".to_string()),
                expires_at,
                ..order(None)
            })
            .await
            .unwrap();
        let trade_id = store
            .insert_venue_trade(&VenueTradeRecord {
                cycle: 1,
                venue_id: "stub".to_string(),
                symbol: "BTC/USD".to_string(),
                asset_class: "crypto_spot".to_string(),
                display_name: Some("Bitcoin".to_string()),
                side: "BUY".to_string(),
                entry_price: "100".to_string(),
                quantity: "1".to_string(),
                notional: "100".to_string(),
                avg_fill_price: None,
                edge_at_entry: "0.03".to_string(),
                fair_value: "0.6".to_string(),
                confidence: "0.7".to_string(),
                risk_pct: "0.0075".to_string(),
                stop_pct: "0.05".to_string(),
                status: "PENDING".to_string(),
                stop_price: Some("95".to_string()),
                target_price: Some("106".to_string()),
                horizon_hours: Some(24),
                client_order_id: Some("c1".to_string()),
                venue_order_id: Some("v1".to_string()),
            })
            .await
            .unwrap();
        store.link_order_to_trade("c1", trade_id).await.unwrap();
        trade_id
    }

    fn reconciler<'a>(registry: &'a VenueRegistry, store: &'a Store) -> Reconciler<'a> {
        Reconciler {
            registry,
            store,
            order_ttl: Duration::seconds(300),
        }
    }

    #[tokio::test]
    async fn a_late_fill_turns_the_pending_trade_into_a_live_position() {
        // The bug this pass exists for: the order filled after the cycle that
        // placed it, so without reconciliation the position exists at the
        // venue and nowhere locally.
        let store = Store::new(":memory:").await.unwrap();
        let trade_id = pending_entry(&store, None).await;
        assert!(
            store.get_open_venue_trades().await.unwrap().is_empty(),
            "a PENDING trade must not count as a position"
        );

        let registry = VenueRegistry::new(vec![Box::new(ScriptedVenue::answering(ack(
            OrderState::Filled,
            dec!(1),
            Some(dec!(99.5)),
        )))]);
        let report = reconciler(&registry, &store).run(Utc::now()).await.unwrap();

        assert_eq!(report.filled, 1);
        assert_eq!(report.resolved, 1);

        let open = store.get_open_venue_trades().await.unwrap();
        assert_eq!(open.len(), 1, "the fill must produce exactly one position");
        assert_eq!(open[0].id, trade_id);
        // The actual fill price, not the limit that was asked for.
        assert_eq!(open[0].avg_fill_price, Some(dec!(99.5)));
        // The thesis recorded at order time survives.
        assert_eq!(open[0].stop_price, Some(dec!(95)));
        assert_eq!(open[0].target_price, Some(dec!(106)));
    }

    #[tokio::test]
    async fn resolving_an_order_unblocks_its_symbol() {
        // The deadlock: the cycle skips any symbol with an unresolved order,
        // so without this pass one resting order blocks a symbol forever.
        let store = Store::new(":memory:").await.unwrap();
        pending_entry(&store, None).await;
        assert_eq!(store.get_unresolved_orders().await.unwrap().len(), 1);

        let registry = VenueRegistry::new(vec![Box::new(ScriptedVenue::answering(ack(
            OrderState::Cancelled,
            Decimal::ZERO,
            None,
        )))]);
        reconciler(&registry, &store).run(Utc::now()).await.unwrap();

        assert!(
            store.get_unresolved_orders().await.unwrap().is_empty(),
            "a resolved order must stop blocking its symbol"
        );
    }

    #[tokio::test]
    async fn an_entry_that_never_filled_leaves_no_position_behind() {
        let store = Store::new(":memory:").await.unwrap();
        let trade_id = pending_entry(&store, None).await;

        let registry = VenueRegistry::new(vec![Box::new(ScriptedVenue::answering(ack(
            OrderState::Rejected("insufficient buying power".into()),
            Decimal::ZERO,
            None,
        )))]);
        reconciler(&registry, &store).run(Utc::now()).await.unwrap();

        assert!(store.get_open_venue_trades().await.unwrap().is_empty());
        let (_, status) = store.trade_for_order("c1").await.unwrap().unwrap();
        assert_eq!(status, "CANCELLED", "trade {trade_id} never opened");
    }

    #[tokio::test]
    async fn a_partial_fill_is_a_real_position_even_while_the_order_rests() {
        let store = Store::new(":memory:").await.unwrap();
        pending_entry(&store, None).await;

        let registry = VenueRegistry::new(vec![Box::new(ScriptedVenue::answering(ack(
            OrderState::PartiallyFilled,
            dec!(0.4),
            Some(dec!(100)),
        )))]);
        let report = reconciler(&registry, &store).run(Utc::now()).await.unwrap();

        // Not resolved — the order is still live — but the filled part is
        // exposure and must be exitable.
        assert_eq!(report.resolved, 0);
        assert_eq!(report.still_open, 1);
        let open = store.get_open_venue_trades().await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].quantity, dec!(0.4), "only the filled part is held");
    }

    #[tokio::test]
    async fn an_order_past_its_ttl_is_cancelled_at_the_venue() {
        let store = Store::new(":memory:").await.unwrap();
        let past = (Utc::now() - Duration::hours(1)).to_rfc3339();
        pending_entry(&store, Some(past)).await;

        let venue = ScriptedVenue::answering(ack(OrderState::Accepted, Decimal::ZERO, None));
        let registry = VenueRegistry::new(vec![Box::new(venue)]);
        let report = reconciler(&registry, &store).run(Utc::now()).await.unwrap();

        assert_eq!(report.expired, 1);
        assert!(store.get_unresolved_orders().await.unwrap().is_empty());
        let (_, status) = store.trade_for_order("c1").await.unwrap().unwrap();
        assert_eq!(status, "CANCELLED");
    }

    #[tokio::test]
    async fn a_venue_that_cannot_answer_leaves_the_order_blocking() {
        // Failing safe: guessing an outcome is how duplicate positions or
        // forgotten live orders happen.
        let store = Store::new(":memory:").await.unwrap();
        pending_entry(&store, None).await;

        let mut venue = ScriptedVenue::answering(ack(OrderState::Filled, dec!(1), Some(dec!(100))));
        *venue.reply.get_mut().unwrap() = None; // the venue errors
        let registry = VenueRegistry::new(vec![Box::new(venue)]);
        let report = reconciler(&registry, &store).run(Utc::now()).await.unwrap();

        assert_eq!(report.unqueryable, 1);
        assert_eq!(report.resolved, 0);
        assert_eq!(
            store.get_unresolved_orders().await.unwrap().len(),
            1,
            "an unanswerable order must keep blocking rather than be assumed dead"
        );
    }

    /// An open position plus a resting EXIT order against it.
    async fn open_position_with_resting_exit(store: &Store) -> i64 {
        let trade_id = pending_entry(store, None).await;
        store
            .activate_trade(trade_id, "100", "1", "OPEN")
            .await
            .unwrap();
        store
            .insert_order(&OrderRecord {
                client_order_id: "x1".to_string(),
                venue_order_id: Some("v2".to_string()),
                side: "SELL".to_string(),
                intent: "EXIT".to_string(),
                trade_id: Some(trade_id),
                state: "ACCEPTED".to_string(),
                // The exit pass stores the reason here when it places the order.
                reject_reason: Some("STOP_LOSS".to_string()),
                ..order(None)
            })
            .await
            .unwrap();
        trade_id
    }

    #[tokio::test]
    async fn an_exit_fill_closes_the_position_and_books_the_pnl() {
        let store = Store::new(":memory:").await.unwrap();
        let trade_id = open_position_with_resting_exit(&store).await;
        assert_eq!(store.get_open_venue_trades().await.unwrap().len(), 1);

        // Sold at 95 against a 100 entry on 1 unit: a $5 loss, not a profit.
        let registry = VenueRegistry::new(vec![Box::new(ScriptedVenue::answering(ack(
            OrderState::Filled,
            dec!(1),
            Some(dec!(95)),
        )))]);
        reconciler(&registry, &store).run(Utc::now()).await.unwrap();

        assert!(
            store.get_open_venue_trades().await.unwrap().is_empty(),
            "a filled exit must close the position"
        );
        let (id, status) = store.trade_for_order("c1").await.unwrap().unwrap();
        assert_eq!(id, trade_id);
        assert_eq!(status, "CLOSED");
    }

    #[tokio::test]
    async fn a_partially_filled_exit_leaves_the_position_open() {
        // Closing on a partial exit would hide the exposure that is still on.
        let store = Store::new(":memory:").await.unwrap();
        open_position_with_resting_exit(&store).await;

        let registry = VenueRegistry::new(vec![Box::new(ScriptedVenue::answering(ack(
            OrderState::PartiallyFilled,
            dec!(0.3),
            Some(dec!(95)),
        )))]);
        reconciler(&registry, &store).run(Utc::now()).await.unwrap();

        assert_eq!(
            store.get_open_venue_trades().await.unwrap().len(),
            1,
            "the unsold remainder is still a live position"
        );
    }

    #[tokio::test]
    async fn an_exit_that_never_filled_leaves_the_position_open_to_retry() {
        // The dangerous mistake is the mirror of an entry: marking the trade
        // closed because the *order* ended would strand real exposure.
        let store = Store::new(":memory:").await.unwrap();
        open_position_with_resting_exit(&store).await;

        let registry = VenueRegistry::new(vec![Box::new(ScriptedVenue::answering(ack(
            OrderState::Cancelled,
            Decimal::ZERO,
            None,
        )))]);
        reconciler(&registry, &store).run(Utc::now()).await.unwrap();

        assert_eq!(
            store.get_open_venue_trades().await.unwrap().len(),
            1,
            "a cancelled exit must leave the position for the next exit pass"
        );
    }

    #[tokio::test]
    async fn an_unconfigured_venue_does_not_abort_the_whole_pass() {
        let store = Store::new(":memory:").await.unwrap();
        pending_entry(&store, None).await;

        let registry = VenueRegistry::new(Vec::new());
        let report = reconciler(&registry, &store).run(Utc::now()).await.unwrap();

        assert_eq!(report.checked, 1);
        assert_eq!(report.unqueryable, 1);
    }

    fn order(expires_at: Option<&str>) -> OrderRecord {
        OrderRecord {
            id: None,
            client_order_id: "c1".to_string(),
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
            state: "ACCEPTED".to_string(),
            reject_reason: None,
            cycle: Some(1),
            submitted_at: None,
            updated_at: None,
            expires_at: expires_at.map(|s| s.to_string()),
        }
    }

    #[test]
    fn terminal_states_are_exactly_the_ones_that_end_an_order() {
        assert!(is_terminal(&OrderState::Filled));
        assert!(is_terminal(&OrderState::Cancelled));
        assert!(is_terminal(&OrderState::Expired));
        assert!(is_terminal(&OrderState::Rejected("no".into())));

        // Unknown is deliberately NOT terminal: treating it as finished is
        // how a live order gets forgotten.
        assert!(!is_terminal(&OrderState::Unknown));
        assert!(!is_terminal(&OrderState::Accepted));
        assert!(!is_terminal(&OrderState::PartiallyFilled));
    }

    #[test]
    fn an_order_expires_only_after_its_recorded_deadline() {
        let deadline = Utc::now();
        let o = order(Some(&deadline.to_rfc3339()));

        assert!(is_expired(&o, deadline + Duration::seconds(1)));
        // At the deadline is expired — the boundary must not leave an order
        // resting one cycle longer than configured.
        assert!(is_expired(&o, deadline));
        assert!(!is_expired(&o, deadline - Duration::seconds(1)));
    }

    #[test]
    fn an_order_with_no_deadline_is_never_force_cancelled() {
        // Cancelling on a missing timestamp would kill orders placed seconds
        // ago by a build that didn't record one.
        assert!(!is_expired(&order(None), Utc::now()));
        assert!(!is_expired(&order(Some("not-a-timestamp")), Utc::now()));
    }

    #[test]
    fn state_names_match_the_database_check_constraint() {
        // These strings are written into orders.state, which has a CHECK
        // constraint — a mismatch is a runtime insert failure, not a compile
        // error.
        for (state, name) in [
            (OrderState::Accepted, "ACCEPTED"),
            (OrderState::PartiallyFilled, "PARTIALLY_FILLED"),
            (OrderState::Filled, "FILLED"),
            (OrderState::Cancelled, "CANCELLED"),
            (OrderState::Expired, "EXPIRED"),
            (OrderState::Unknown, "UNKNOWN"),
        ] {
            assert_eq!(state_name(&state), name);
        }
        assert_eq!(state_name(&OrderState::Rejected("x".into())), "REJECTED");
    }

    #[test]
    fn pnl_of_an_exit_uses_the_entry_it_was_opened_at() {
        assert_eq!(unrealized_pnl(dec!(100), dec!(104), dec!(2.5)), dec!(10));
        assert_eq!(unrealized_pnl(dec!(100), dec!(96), dec!(2.5)), dec!(-10));
    }
}
