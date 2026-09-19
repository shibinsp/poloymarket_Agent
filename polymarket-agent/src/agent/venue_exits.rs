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

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use tracing::{info, warn};
use uuid::Uuid;

use crate::db::store::{OrderRecord, Store, VenueOpenTrade};
use crate::venue::types::{
    Instrument, InstrumentId, OrderKind, OrderRequest, Side, TimeInForce, VenueId,
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
}

impl VenueExits<'_> {
    pub async fn run(&self, now: DateTime<Utc>, cycle: i64) -> Result<usize> {
        let open = self.store.get_open_venue_trades().await?;
        let mut closed = 0usize;

        for trade in open {
            match self.process(&trade, now, cycle).await {
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

    /// Returns whether an exit order was submitted.
    async fn process(
        &self,
        trade: &VenueOpenTrade,
        now: DateTime<Utc>,
        cycle: i64,
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

        self.submit_exit(venue, trade, &instrument_id, mark, qty, reason, cycle)
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
    ) -> Result<()> {
        // Re-resolve the instrument so lot/tick rules come from the venue
        // rather than being reconstructed from the stored row.
        let instrument = self.resolve(venue, instrument_id).await?;
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
                submitted_at: None,
                updated_at: None,
                expires_at: None,
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

        self.store
            .update_order_state(
                &client_order_id,
                if ack.filled_qty > Decimal::ZERO {
                    "FILLED"
                } else {
                    "ACCEPTED"
                },
                Some(&ack.venue_order_id),
                &ack.filled_qty.to_string(),
                ack.avg_fill_price.map(|p| p.to_string()).as_deref(),
                None,
            )
            .await?;

        // Only a fill closes the position. An accepted exit order leaves the
        // trade open until it actually executes.
        if ack.filled_qty > Decimal::ZERO {
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
            info!(
                trade_id = trade.id,
                realized_pnl = %realized,
                reason = reason.as_str(),
                "Position closed"
            );
        }

        Ok(())
    }

    async fn resolve(&self, venue: &dyn Venue, id: &InstrumentId) -> Result<Instrument> {
        let filter = crate::venue::types::ScanFilter {
            symbols: vec![id.symbol.clone()],
            ..Default::default()
        };
        venue
            .list_instruments(&filter)
            .await?
            .into_iter()
            .find(|i| &i.id == id)
            .ok_or_else(|| anyhow::anyhow!("Instrument {id} is no longer listed — cannot exit it"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

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
