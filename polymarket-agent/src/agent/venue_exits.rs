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
use crate::venue::session::SessionKind;
use crate::venue::types::{
    Instrument, InstrumentId, OrderKind, OrderRequest, OrderState, Side, TimeInForce, VenueId,
    PROTECTIVE_STOP_PREFIX,
};
use crate::venue::{Venue, VenueRegistry};

/// How often, and how far apart, to re-read a stop just cancelled before
/// leaving the exit to the next cycle: about three seconds in all.
const CANCEL_CONFIRM_TRIES: usize = 6;
const CANCEL_CONFIRM_WAIT_MS: u64 = 500;

/// Why a position is being closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    StopLoss,
    /// A stop that has been raised above the initial stop (breakeven or trail).
    TrailingStop,
    TakeProfit,
    MaxHold,
    SessionClose,
    EmergencyStop,
    SmaDeathCross,
}

impl ExitReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExitReason::StopLoss => "STOP_LOSS",
            ExitReason::TrailingStop => "TRAILING_STOP",
            ExitReason::TakeProfit => "TAKE_PROFIT",
            ExitReason::MaxHold => "MAX_HOLD",
            ExitReason::SessionClose => "SESSION_CLOSE",
            ExitReason::EmergencyStop => "EMERGENCY_STOP",
            ExitReason::SmaDeathCross => "SMA_DEATH_CROSS",
        }
    }
}

/// Rules applied to an open position, resolved from the trade and config.
#[derive(Debug, Clone, Copy)]
pub struct ExitRules {
    /// A short: the stop sits above the price and the target below it.
    pub is_short: bool,
    pub stop_price: Option<Decimal>,
    /// The stop has been ratcheted above its entry level.
    pub stop_trailed: bool,
    pub target_price: Option<Decimal>,
    pub max_hold_until: Option<DateTime<Utc>>,
    pub session_close_due: bool,
    pub emergency_stop: bool,
    pub sma_exit_due: bool,
}

/// How to record an exit order the venue call failed on: the state, and what
/// to write over the order's stored exit reason.
///
/// Only a refusal overwrites the reason, because a refused order never fills.
/// A timeout or dropped connection may still have reached the venue: on 3 Nov
/// 2026 ORCL's 15:45 sale came back as an error, filled anyway, and the trade
/// was closed with the error text as its reason instead of SESSION_CLOSE.
pub fn failed_exit_record(error: &str) -> (&'static str, Option<&str>) {
    let refused = error.contains("HTTP 4")
        || error.contains("rejected")
        || error.contains("below the venue's minimum")
        || error.contains("rounds to zero")
        || error.contains("insufficient qty")
        || error.contains("not tradable");
    if refused {
        ("REJECTED", Some(error))
    } else {
        ("UNKNOWN", None)
    }
}

/// Decide whether an open long should be closed at the current mark.
///
/// Emergency stop, SMA death cross and session close take precedence over price boundaries.
/// Stop is checked before target: if a bar straddles both, assuming the good
/// outcome would flatter every backtest and every logged P&L.
pub fn evaluate_exit(
    mark_price: Decimal,
    now: DateTime<Utc>,
    rules: &ExitRules,
) -> Option<ExitReason> {
    if rules.emergency_stop {
        return Some(ExitReason::EmergencyStop);
    }
    if rules.sma_exit_due {
        return Some(ExitReason::SmaDeathCross);
    }
    if rules.session_close_due {
        return Some(ExitReason::SessionClose);
    }
    if let Some(stop) = rules.stop_price {
        let hit = if rules.is_short {
            mark_price >= stop
        } else {
            mark_price <= stop
        };
        if hit {
            return Some(if rules.stop_trailed {
                ExitReason::TrailingStop
            } else {
                ExitReason::StopLoss
            });
        }
    }
    if let Some(target) = rules.target_price {
        let reached = if rules.is_short {
            mark_price <= target
        } else {
            mark_price >= target
        };
        if reached {
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
/// The break-even stop for a position that has gained at least the risk it
/// started with (1R), or `None` while it has not, or once the stop is already
/// at or past the entry. Rounded to a cent on the side that never gives back
/// more than the entry.
pub fn breakeven_stop(
    entry: Decimal,
    stop: Option<Decimal>,
    mark: Decimal,
    is_short: bool,
) -> Option<Decimal> {
    let stop = stop?;
    if !is_short && stop < entry && mark - entry >= entry - stop {
        Some(entry.round_dp_with_strategy(2, rust_decimal::RoundingStrategy::ToPositiveInfinity))
    } else if is_short && stop > entry && entry - mark >= stop - entry {
        Some(entry.round_dp_with_strategy(2, rust_decimal::RoundingStrategy::ToNegativeInfinity))
    } else {
        None
    }
}

pub fn unrealized_pnl(entry: Decimal, mark: Decimal, qty: Decimal) -> Decimal {
    (mark - entry) * qty
}

/// P&L of a long or a short; `qty` is always positive.
pub fn position_pnl(entry: Decimal, mark: Decimal, qty: Decimal, is_short: bool) -> Decimal {
    let pnl = unrealized_pnl(entry, mark, qty);
    if is_short {
        -pnl
    } else {
        pnl
    }
}

/// The side that closes a position, which is also the side of the book it
/// is valued on: the bid for a long, the ask for a short.
fn closing_side(is_short: bool) -> Side {
    if is_short {
        Side::Buy
    } else {
        Side::Sell
    }
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
    /// Account-wide emergency stop flag.
    pub emergency_stop: bool,
    /// Optional US strategy config for SMA crossover exits.
    pub us_strategy: Option<&'a crate::config::UsStrategyConfig>,
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
        let unresolved = self.store.get_unresolved_orders().await?;
        // A resting stop is not an exit under way: the exit it guards against
        // may still be due.
        let exiting: HashSet<i64> = unresolved
            .iter()
            .filter(|o| o.intent == "EXIT" && !o.is_protective_stop())
            .filter_map(|o| o.trade_id)
            .collect();
        // An entry still working can add to the position, so its stop waits
        // until the size is final.
        let entering: HashSet<i64> = unresolved
            .iter()
            .filter(|o| o.intent == "ENTRY")
            .filter_map(|o| o.trade_id)
            .collect();
        let mut stops: HashMap<i64, Vec<OrderRecord>> = HashMap::new();
        for order in unresolved.into_iter().filter(|o| o.is_protective_stop()) {
            if let Some(trade_id) = order.trade_id {
                stops.entry(trade_id).or_default().push(order);
            }
        }

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
            let trade_stops = stops.get(&trade.id).map(Vec::as_slice).unwrap_or(&[]);
            match self
                .process(&trade, trade_stops, now, cycle, &mut instruments)
                .await
            {
                Ok(true) => closed += 1,
                Ok(false) => {
                    if crate::us::orb::is_intraday_strategy(trade.strategy.as_deref())
                        && trade_stops.is_empty()
                        && !entering.contains(&trade.id)
                    {
                        if let Err(e) = self
                            .place_resting_stop(&trade, now, cycle, &mut instruments)
                            .await
                        {
                            warn!(
                                trade_id = trade.id,
                                symbol = %trade.symbol,
                                error = %format!("{e:#}"),
                                "Could not place a resting stop — the agent's own check still covers it"
                            );
                        }
                    }
                }
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
        let mark = quote.fair_price(closing_side(trade.is_short));
        let entry = trade.avg_fill_price.unwrap_or(trade.entry_price);
        let pnl = position_pnl(entry, mark, trade.quantity, trade.is_short);
        self.store
            .mark_trade(trade.id, &mark.to_string(), &pnl.to_string(), now)
            .await
    }

    /// Returns whether an exit order was submitted.
    async fn process(
        &self,
        trade: &VenueOpenTrade,
        // The position's protective stops still unresolved at the venue.
        stops: &[OrderRecord],
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
        // Value a position at what closing it would get, not the midpoint,
        // unless the book is too wide for that side to mean anything.
        let side = closing_side(trade.is_short);
        let mark = quote.fair_price(side);
        if let (Some(spread), Some(last)) = (quote.spread_pct(), quote.last) {
            if spread > crate::venue::types::WIDE_SPREAD {
                info!(
                    symbol = %trade.symbol,
                    bid = %quote.bid,
                    ask = %quote.ask,
                    last = %last,
                    "Wide quote: marked at the last trade, not the midpoint"
                );
            }
        }

        let entry: Decimal = trade.avg_fill_price.unwrap_or(trade.entry_price);
        let qty = trade.quantity;
        let pnl = position_pnl(entry, mark, qty, trade.is_short);

        self.store
            .mark_trade(trade.id, &mark.to_string(), &pnl.to_string(), now)
            .await?;

        let is_equity = trade.asset_class.eq_ignore_ascii_case("equity");
        let is_us_alpaca_equity = is_equity && trade.venue_id.eq_ignore_ascii_case("alpaca");
        // An opening-range breakout is intraday whatever the swing strategy
        // is: it takes the session-close exit, never the SMA rules.
        let is_orb_position = crate::us::orb::is_intraday_strategy(trade.strategy.as_deref());
        let is_sma_position = is_us_alpaca_equity
            && !is_orb_position
            && self
                .us_strategy
                .map(|s| s.strategy.eq_ignore_ascii_case("sma_crossover"))
                .unwrap_or(false);
        // Daily crossover positions are meant to survive overnight. Intraday
        // session-close exits would invalidate the 20/50-day holding rule.
        let session_close_due =
            is_equity && !is_sma_position && venue.session().is_intraday_exit_due(now, 15);

        // Break-even after +1R for U.S. intraday trades, at the user's request
        // on 3 Nov 2026 after RIVN went +1.52R and closed at a loss. Tested on
        // 210 trades: +$74.76 against +$74.45, so protection, not an edge.
        if is_orb_position && is_us_alpaca_equity && !session_close_due {
            if let Some(be) = breakeven_stop(entry, trade.stop_price, mark, trade.is_short) {
                info!(
                    trade_id = trade.id,
                    symbol = %trade.symbol,
                    from = ?trade.stop_price,
                    to = %be,
                    mark = %mark,
                    "Up 1R: moving the stop to break-even"
                );
                self.store.update_trade_stop(trade.id, &be.to_string()).await?;
                // The old stop holds the shares; it must be gone before the
                // new one can rest. If it fired or is still cancelling, the
                // next pass sees that and places the new one.
                if self.clear_resting_stops(venue, stops, now).await? {
                    let mut moved = trade.clone();
                    moved.stop_price = Some(be);
                    if let Err(e) = self.place_resting_stop(&moved, now, cycle, instruments).await {
                        warn!(
                            trade_id = trade.id,
                            symbol = %trade.symbol,
                            error = %format!("{e:#}"),
                            "Could not rest the break-even stop — the agent's own check still covers it"
                        );
                    }
                }
                return Ok(false);
            }
        }

        let completed: Vec<crate::venue::types::Candle> = if is_sma_position {
            venue
                .candles(&instrument_id, crate::venue::types::CandleInterval::D1, 65)
                .await
                .map(|bars| {
                    bars.into_iter()
                        .filter(|c| c.ts + Duration::hours(24) <= now)
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let sma_exit_due = if is_sma_position {
            let strategy = self.us_strategy.expect("SMA position has strategy config");
            let minimum_bars = strategy.slow_sma_period.saturating_add(1);
            if strategy.moving_average_type.eq_ignore_ascii_case("SMA")
                && strategy.fast_sma_period > 0
                && strategy.fast_sma_period < strategy.slow_sma_period
                && completed.len() >= minimum_bars
                && crate::us::sma_crossover::validate_daily_candles(
                    &trade.symbol,
                    &completed,
                    now,
                    minimum_bars,
                    crate::us::sma_crossover::MAX_PERMISSIBLE_STALE_DAYS,
                )
                .is_ok()
            {
                let closes: Vec<Decimal> = completed.iter().map(|c| c.close).collect();
                let n = closes.len();
                if let (Some(fast_curr), Some(slow_curr), Some(fast_prev), Some(slow_prev)) = (
                    crate::us::sma_crossover::calculate_sma(&closes[..n], strategy.fast_sma_period),
                    crate::us::sma_crossover::calculate_sma(&closes[..n], strategy.slow_sma_period),
                    crate::us::sma_crossover::calculate_sma(
                        &closes[..n - 1],
                        strategy.fast_sma_period,
                    ),
                    crate::us::sma_crossover::calculate_sma(
                        &closes[..n - 1],
                        strategy.slow_sma_period,
                    ),
                ) {
                    fast_prev >= slow_prev && fast_curr < slow_curr
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        let (stop_price, stop_trailed) = if is_sma_position {
            self.ratchet_sma_stop(trade, entry, mark, &completed)
                .await?
        } else {
            (trade.stop_price, false)
        };

        // A stop resting at the venue fires on real trades. Checking the same
        // level against a thin quote here could only add false alarms.
        let stop_rests_at_venue = stops
            .iter()
            .any(|o| o.state == "ACCEPTED" || o.state == "PARTIALLY_FILLED");

        let rules = ExitRules {
            is_short: trade.is_short,
            stop_price: if stop_rests_at_venue {
                None
            } else {
                stop_price
            },
            stop_trailed,
            target_price: if is_sma_position {
                None
            } else {
                trade.target_price
            },
            max_hold_until: if is_sma_position {
                None
            } else {
                trade
                    .opened_at
                    .map(|opened| opened + Duration::hours(self.max_hold_hours))
            },
            session_close_due,
            emergency_stop: self.emergency_stop,
            sma_exit_due,
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

        // The stop holds the shares, so Alpaca refuses a sale until it is gone.
        if !self.clear_resting_stops(venue, stops, now).await? {
            return Ok(false);
        }

        if let Err(e) = self
            .submit_exit(
                venue,
                trade,
                &instrument_id,
                // The exit has to cross the spread to fill, however wide it is.
                quote.taker_price(side),
                Some(quote.mid),
                qty,
                reason,
                cycle,
                instruments,
            )
            .await
        {
            // The stop is already gone. A refused sale leaves the shares with
            // nothing at the venue guarding them, so put the stop back until
            // the next pass retries the exit. A sale whose fate is unknown may
            // be live and holding the shares; reconciliation settles that one.
            if failed_exit_record(&format!("{e:#}")).0 == "REJECTED" {
                if let Err(stop_err) = self.place_resting_stop(trade, now, cycle, instruments).await
                {
                    warn!(
                        trade_id = trade.id,
                        symbol = %trade.symbol,
                        error = %format!("{stop_err:#}"),
                        "Exit refused and the stop could not be put back"
                    );
                }
            }
            return Err(e);
        }
        Ok(true)
    }

    /// Take a position's resting stops down. Returns whether the way is clear
    /// to exit: not if a stop fired first, which has already closed the trade,
    /// or if one has not finished cancelling, which the next cycle retries.
    async fn clear_resting_stops(
        &self,
        venue: &dyn Venue,
        stops: &[OrderRecord],
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let reconciler = crate::agent::reconcile::Reconciler {
            registry: self.registry,
            store: self.store,
            order_ttl: Duration::seconds(self.order_ttl_seconds),
        };
        let mut clear = true;
        for stop in stops {
            if let Some(id) = &stop.venue_order_id {
                // A refusal usually means it just filled, which the read below
                // will show.
                if let Err(e) = venue.cancel_order(id).await {
                    warn!(venue_order_id = %id, error = %format!("{e:#}"), "Could not cancel a resting stop");
                }
            }
            // A cancel takes a moment to confirm. Leaving it to the next cycle
            // left the 2 Nov 2026 ORB positions with no stop for a full minute
            // before the 15:45 sale, so wait a few seconds for it here.
            let mut report = reconciler.resolve_one(stop, now).await?;
            for _ in 0..CANCEL_CONFIRM_TRIES {
                if report.filled > 0 || report.still_open == 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(CANCEL_CONFIRM_WAIT_MS)).await;
                report = reconciler.resolve_one(stop, now).await?;
            }
            clear &= report.filled == 0 && report.still_open == 0;
        }
        Ok(clear)
    }

    /// Put a stop at the venue, so an open position stays protected when this
    /// process is not running: on 30 Oct 2026 the laptop slept for nearly three
    /// hours with MSTR open and nothing checking its stop.
    ///
    /// A refusal is not retried. It would be refused again every minute, and
    /// the agent's own check goes on covering the position either way.
    async fn place_resting_stop(
        &self,
        trade: &VenueOpenTrade,
        now: DateTime<Utc>,
        cycle: i64,
        instruments: &mut HashMap<String, Vec<Instrument>>,
    ) -> Result<()> {
        let venue_id = VenueId::new(trade.venue_id.clone());
        let (Some(stop_price), Some(venue)) = (trade.stop_price, self.registry.get(&venue_id))
        else {
            return Ok(());
        };
        // Outside regular hours it would only queue, and stops do not trigger then.
        if venue.session_state(now).kind() != Some(SessionKind::Regular)
            || self.store.protective_stop_rejected(trade.id).await?
        {
            return Ok(());
        }

        let instrument_id = InstrumentId::new(venue_id, trade.symbol.clone());
        let instrument = self.resolve(venue, &instrument_id, instruments).await?;
        let side = closing_side(trade.is_short);
        let client_order_id = format!("{PROTECTIVE_STOP_PREFIX}{}", Uuid::new_v4());

        self.store
            .insert_order(&OrderRecord {
                id: None,
                client_order_id: client_order_id.clone(),
                venue_order_id: None,
                venue_id: trade.venue_id.clone(),
                symbol: trade.symbol.clone(),
                side: side.to_string(),
                intent: "EXIT".to_string(),
                trade_id: Some(trade.id),
                limit_price: Some(stop_price.to_string()),
                qty: trade.quantity.to_string(),
                filled_qty: "0".to_string(),
                avg_fill_price: None,
                state: "PENDING".to_string(),
                reject_reason: Some(ExitReason::StopLoss.as_str().to_string()),
                cycle: Some(cycle),
                mid_at_submit: None,
                submitted_at: None,
                updated_at: None,
                // Long enough to outlast the session; the DAY ones end with it.
                expires_at: Some((now + Duration::hours(24)).to_rfc3339()),
            })
            .await?;

        let request = OrderRequest {
            instrument,
            side,
            kind: OrderKind::Stop { price: stop_price },
            qty: trade.quantity,
            // Alpaca takes fractional stops as DAY orders only. A whole-share
            // stop can stay on overnight if this process is not there to close.
            tif: if trade.quantity.fract().is_zero() {
                TimeInForce::Gtc
            } else {
                TimeInForce::Day
            },
            extended_hours: false,
            client_order_id: client_order_id.clone(),
        };

        let ack = match venue.place_order(&request).await {
            Ok(ack) => ack,
            Err(e) => {
                let detail = format!("{e:#}");
                let refused = detail.contains("HTTP 4") || detail.contains("rejected");
                self.store
                    .update_order_state(
                        &client_order_id,
                        if refused { "REJECTED" } else { "UNKNOWN" },
                        None,
                        "0",
                        None,
                        Some(&detail),
                    )
                    .await?;
                return Err(e);
            }
        };

        // Any fill is left for reconciliation to find, which closes the trade
        // the same way however the stop came to fire.
        let state = if crate::agent::reconcile::is_terminal(&ack.state) && ack.filled_qty.is_zero()
        {
            crate::agent::reconcile::state_name(&ack.state)
        } else {
            "ACCEPTED"
        };
        self.store
            .update_order_state(
                &client_order_id,
                state,
                Some(&ack.venue_order_id),
                "0",
                None,
                match &ack.state {
                    OrderState::Rejected(reason) => Some(reason.as_str()),
                    _ => None,
                },
            )
            .await?;
        info!(
            trade_id = trade.id,
            symbol = %trade.symbol,
            stop = %stop_price,
            qty = %trade.quantity,
            state,
            "Resting stop placed at the venue"
        );
        Ok(())
    }

    /// Raise an SMA position's stop to breakeven and then a trailing ATR
    /// level as the trade works, persisting any rise. Returns the stop in
    /// force and whether it now sits above the initial stop.
    async fn ratchet_sma_stop(
        &self,
        trade: &VenueOpenTrade,
        entry: Decimal,
        mark: Decimal,
        completed: &[crate::venue::types::Candle],
    ) -> Result<(Option<Decimal>, bool)> {
        let (Some(current), Some(stop_pct), Some(opened), Some(strategy)) = (
            trade.stop_price,
            trade.stop_pct,
            trade.opened_at,
            self.us_strategy,
        ) else {
            return Ok((trade.stop_price, false));
        };
        let initial = (trade.entry_price * (Decimal::ONE - stop_pct)).round_dp(2);
        // Daily bars are stamped at the session's start, so the entry day's
        // bar is any bar that ends after the fill.
        let highest_high = completed
            .iter()
            .filter(|c| c.ts + Duration::hours(24) > opened)
            .map(|c| c.high)
            .fold(mark, Decimal::max);
        let atr =
            crate::risk::volatility::atr(completed, crate::us::sma_crossover::TRAILING_ATR_PERIOD);
        let managed = crate::us::sma_crossover::managed_stop(
            entry,
            initial,
            highest_high,
            atr,
            &strategy.sma_trailing_stop(),
        );
        let stop = if managed > current {
            info!(
                trade_id = trade.id,
                symbol = %trade.symbol,
                from = %current,
                to = %managed,
                "Raising protective stop"
            );
            self.store
                .update_trade_stop(trade.id, &managed.to_string())
                .await?;
            managed
        } else {
            current
        };
        Ok((Some(stop), stop > initial))
    }

    #[allow(clippy::too_many_arguments)]
    async fn submit_exit(
        &self,
        venue: &dyn Venue,
        trade: &VenueOpenTrade,
        instrument_id: &InstrumentId,
        mark: Decimal,
        // The book's midpoint, as distinct from `mark` (the bid). Slippage is
        // measured against the mid on both sides or the two are not
        // comparable.
        mid: Option<Decimal>,
        qty: Decimal,
        reason: ExitReason,
        cycle: i64,
        instruments: &mut HashMap<String, Vec<Instrument>>,
    ) -> Result<()> {
        // Query venue positions to size exit using reconciled available positions if reported
        let exit_qty = match venue.positions().await {
            Ok(pos_list) => {
                if let Some(pos) = pos_list
                    .iter()
                    .find(|p| p.instrument.symbol.eq_ignore_ascii_case(&trade.symbol))
                {
                    // A venue reports a short as a negative quantity.
                    let held = if trade.is_short { -pos.qty } else { pos.qty };
                    if held > Decimal::ZERO {
                        qty.min(held)
                    } else {
                        // Venue explicitly returned this symbol with 0 quantity (already closed/liquidated)
                        warn!(
                            trade_id = trade.id,
                            symbol = %trade.symbol,
                            "Venue explicitly reports 0 position for symbol — closing local trade record"
                        );
                        let entry = trade.avg_fill_price.unwrap_or(trade.entry_price);
                        let realized = position_pnl(entry, mark, qty, trade.is_short);
                        self.store
                            .close_trade(
                                trade.id,
                                &mark.to_string(),
                                &realized.to_string(),
                                reason.as_str(),
                                "POSITION_CLOSED_AT_VENUE",
                            )
                            .await?;
                        return Ok(());
                    }
                } else {
                    // Position list does not explicitly include symbol (e.g. test mock without position listing)
                    qty
                }
            }
            Err(e) => {
                warn!(
                    trade_id = trade.id,
                    symbol = %trade.symbol,
                    error = %e,
                    "Failed to query venue positions before exit — defaulting to trade quantity"
                );
                qty
            }
        };

        // Re-resolve the instrument so lot/tick rules come from the venue
        // rather than being reconstructed from the stored row.
        let instrument = self.resolve(venue, instrument_id, instruments).await?;
        let side = closing_side(trade.is_short);
        let limit_price = instrument.round_price(mark, side);
        let client_order_id = Uuid::new_v4().to_string();

        self.store
            .insert_order(&OrderRecord {
                id: None,
                client_order_id: client_order_id.clone(),
                venue_order_id: None,
                venue_id: trade.venue_id.clone(),
                symbol: trade.symbol.clone(),
                side: side.to_string(),
                intent: "EXIT".to_string(),
                trade_id: Some(trade.id),
                limit_price: Some(limit_price.to_string()),
                qty: exit_qty.to_string(),
                filled_qty: "0".to_string(),
                avg_fill_price: None,
                state: "PENDING".to_string(),
                reject_reason: Some(reason.as_str().to_string()),
                cycle: Some(cycle),
                // The *mid*, not the mark. `mark` is `taker_price(Sell)`,
                // i.e. the bid — recording it here would compare a sell
                // filling at the bid against the bid and call it zero
                // slippage, when against the mid it cost half the spread.
                // Entries record the true mid, so using the bid for exits
                // measures the two sides against different references and
                // understates sell-side cost systematically.
                mid_at_submit: mid.map(|m| m.to_string()),
                submitted_at: None,
                updated_at: None,
                expires_at: Some(
                    (Utc::now() + Duration::seconds(self.order_ttl_seconds)).to_rfc3339(),
                ),
            })
            .await?;

        // Alpaca takes fractional-share orders as DAY orders only; a GTC exit
        // for a fractional position is rejected, so it could never close.
        let tif = if instrument.asset_class == crate::venue::types::AssetClass::Equity {
            TimeInForce::Day
        } else {
            TimeInForce::Gtc
        };
        let request = OrderRequest {
            instrument,
            side,
            kind: OrderKind::Limit { price: limit_price },
            qty: exit_qty,
            tif,
            extended_hours: false,
            client_order_id: client_order_id.clone(),
        };

        let ack = match venue.place_order(&request).await {
            Ok(ack) => ack,
            Err(e) => {
                let err_str = format!("{e:#}");
                let (final_state, note) = failed_exit_record(&err_str);
                self.store
                    .update_order_state(&client_order_id, final_state, None, "0", None, note)
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
            let realized = position_pnl(entry, exit_price, ack.filled_qty, trade.is_short);
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
                trade.id,
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

            if venue.id().as_str().contains("upstox") || trade.venue_id.contains("upstox") {
                let sbx_order = crate::db::store::IndiaSandboxOrderRecord {
                    id: format!("ord-{}", Uuid::new_v4().simple()),
                    client_order_id: client_order_id.clone(),
                    venue_order_id: Some(ack.venue_order_id.clone()),
                    symbol: trade.symbol.clone(),
                    side: "SELL".to_string(),
                    order_type: "LIMIT".to_string(),
                    quantity: request.qty.to_string().parse::<i64>().unwrap_or(1),
                    price: limit_price.to_string(),
                    trigger_price: None,
                    status: state.to_string(),
                    fill_quantity: ack.filled_qty.to_string().parse::<i64>().unwrap_or(1),
                    average_fill_price: Some(exit_price.to_string()),
                    rejection_reason: None,
                    created_at: Utc::now().to_rfc3339(),
                    updated_at: Utc::now().to_rfc3339(),
                };
                let _ = self.store.save_india_sandbox_order(&sbx_order).await;
            }
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
                strategy: None,
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
            emergency_stop: false,
            us_strategy: None,
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
            emergency_stop: false,
            us_strategy: None,
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

    /// An opening-range position, plus a stub venue with it listed.
    async fn orb_position() -> (Store, i64) {
        let store = Store::new(":memory:").await.expect("in-memory store");
        let trade_id = store
            .insert_venue_trade(&VenueTradeRecord {
                cycle: 1,
                venue_id: "stub".to_string(),
                symbol: "MSTR".to_string(),
                asset_class: "crypto_spot".to_string(),
                display_name: None,
                side: "BUY".to_string(),
                entry_price: "100".to_string(),
                quantity: "1.5".to_string(),
                notional: "150".to_string(),
                avg_fill_price: Some("100".to_string()),
                edge_at_entry: "0".to_string(),
                fair_value: "0".to_string(),
                confidence: "0".to_string(),
                risk_pct: "0.01".to_string(),
                stop_pct: "0.03".to_string(),
                status: "OPEN".to_string(),
                stop_price: Some("97".to_string()),
                target_price: None,
                horizon_hours: Some(7),
                client_order_id: Some("entry-1".to_string()),
                venue_order_id: Some("v-entry-1".to_string()),
                strategy: Some("orb".to_string()),
            })
            .await
            .expect("insert position");
        (store, trade_id)
    }

    fn orb_stub(mark: Decimal, ack: OrderAck) -> StubVenue {
        StubVenue::with_classes(
            "stub",
            TradingSession::Always,
            &[("MSTR", AssetClass::CryptoSpot)],
            false,
        )
        .quoting(mark)
        .acking(ack)
    }

    fn exits<'a>(registry: &'a VenueRegistry, store: &'a Store) -> VenueExits<'a> {
        VenueExits {
            registry,
            store,
            max_hold_hours: 72,
            order_ttl_seconds: 300,
            emergency_stop: false,
            us_strategy: None,
        }
    }

    #[tokio::test]
    async fn an_orb_position_gets_one_resting_stop() {
        let (store, _) = orb_position().await;
        let registry = VenueRegistry::new(vec![Box::new(orb_stub(
            dec!(100),
            ack(OrderState::Accepted, Decimal::ZERO),
        ))]);
        let exits = exits(&registry, &store);

        exits.run(Utc::now(), 1).await.unwrap();
        exits.run(Utc::now(), 2).await.unwrap();

        let orders = store.get_unresolved_orders().await.unwrap();
        assert_eq!(orders.len(), 1, "no second stop: {orders:?}");
        assert!(orders[0].is_protective_stop());
        assert_eq!(store.get_open_venue_trades().await.unwrap().len(), 1);
    }

    /// Thin quotes must not trip what the venue is already watching on real
    /// trades, which is the whole point of having it there.
    #[tokio::test]
    async fn a_resting_stop_replaces_the_agents_own_check() {
        let (store, _) = orb_position().await;
        let registry = VenueRegistry::new(vec![Box::new(orb_stub(
            dec!(100),
            ack(OrderState::Accepted, Decimal::ZERO),
        ))]);
        exits(&registry, &store).run(Utc::now(), 1).await.unwrap();

        // The stub's mark is now through the 97 stop.
        let registry = VenueRegistry::new(vec![Box::new(orb_stub(
            dec!(90),
            ack(OrderState::Accepted, Decimal::ZERO),
        ))]);
        let closed = exits(&registry, &store).run(Utc::now(), 2).await.unwrap();

        assert_eq!(closed, 0);
        let orders = store.get_unresolved_orders().await.unwrap();
        assert!(orders.iter().all(|o| o.is_protective_stop()), "{orders:?}");
    }

    /// A refused stop would be refused again every minute, so it is not retried
    /// and the agent's own check stays in charge.
    #[tokio::test]
    async fn a_refused_stop_is_not_retried_and_the_agent_still_checks() {
        let (store, _) = orb_position().await;
        let stub = orb_stub(
            dec!(100),
            ack(OrderState::Rejected("no".to_string()), Decimal::ZERO),
        );
        let registry = VenueRegistry::new(vec![Box::new(stub)]);
        let exits_pass = exits(&registry, &store);
        exits_pass.run(Utc::now(), 1).await.unwrap();
        exits_pass.run(Utc::now(), 2).await.unwrap();
        assert!(store.get_unresolved_orders().await.unwrap().is_empty());

        let registry = VenueRegistry::new(vec![Box::new(orb_stub(
            dec!(90),
            ack(OrderState::Accepted, Decimal::ZERO),
        ))]);
        let closed = exits(&registry, &store).run(Utc::now(), 3).await.unwrap();
        assert_eq!(closed, 1, "the stop at 97 is breached and nothing rests");
    }

    /// The stop fired in the moment before the exit was due: the position is
    /// closed as a stop, and a second sale would open a short.
    #[tokio::test]
    async fn a_stop_that_fired_first_closes_the_trade_with_no_exit_order() {
        let (store, _) = orb_position().await;
        let registry = VenueRegistry::new(vec![Box::new(orb_stub(
            dec!(100),
            ack(OrderState::Accepted, Decimal::ZERO),
        ))]);
        let exits_pass = exits(&registry, &store);
        exits_pass.run(Utc::now(), 1).await.unwrap();
        let stop = store.get_unresolved_orders().await.unwrap().remove(0);

        let filled = OrderAck {
            avg_fill_price: Some(dec!(96.5)),
            ..ack(OrderState::Filled, dec!(1.5))
        };
        let registry = VenueRegistry::new(vec![Box::new(
            orb_stub(dec!(90), ack(OrderState::Accepted, Decimal::ZERO)).answering_orders(filled),
        )]);
        let venue = registry.all().next().unwrap();
        let clear = exits(&registry, &store)
            .clear_resting_stops(venue, &[stop], Utc::now())
            .await
            .unwrap();

        assert!(!clear, "nothing is left to exit");
        assert!(store.get_open_venue_trades().await.unwrap().is_empty());
        assert!(store.get_unresolved_orders().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_cancelled_stop_clears_the_way_to_exit() {
        let (store, _) = orb_position().await;
        let registry = VenueRegistry::new(vec![Box::new(orb_stub(
            dec!(100),
            ack(OrderState::Accepted, Decimal::ZERO),
        ))]);
        let exits_pass = exits(&registry, &store);
        exits_pass.run(Utc::now(), 1).await.unwrap();
        let stop = store.get_unresolved_orders().await.unwrap().remove(0);

        let registry = VenueRegistry::new(vec![Box::new(
            orb_stub(dec!(100), ack(OrderState::Accepted, Decimal::ZERO))
                .answering_orders(ack(OrderState::Cancelled, Decimal::ZERO)),
        )]);
        let venue = registry.all().next().unwrap();
        let clear = exits(&registry, &store)
            .clear_resting_stops(venue, &[stop], Utc::now())
            .await
            .unwrap();

        assert!(clear);
        assert_eq!(store.get_open_venue_trades().await.unwrap().len(), 1);
    }

    fn rules(
        stop: Option<Decimal>,
        target: Option<Decimal>,
        deadline: Option<DateTime<Utc>>,
    ) -> ExitRules {
        ExitRules {
            is_short: false,
            stop_price: stop,
            stop_trailed: false,
            target_price: target,
            max_hold_until: deadline,
            session_close_due: false,
            emergency_stop: false,
            sma_exit_due: false,
        }
    }

    /// RIVN on 2 Nov 2026: entry 13.06524, stop 12.65, so 1R is 0.41524.
    #[test]
    fn the_stop_moves_to_break_even_at_one_r_and_only_once() {
        use rust_decimal_macros::dec;
        let (entry, stop) = (dec!(13.06524), Some(dec!(12.65)));
        assert_eq!(breakeven_stop(entry, stop, dec!(13.40), false), None, "under 1R");
        assert_eq!(breakeven_stop(entry, stop, dec!(13.49), false), Some(dec!(13.07)));
        assert_eq!(breakeven_stop(entry, Some(dec!(13.07)), dec!(14), false), None, "already moved");
        assert_eq!(breakeven_stop(entry, None, dec!(14), false), None);
        // A short: entry 100, stop 102, 1R down is 98.
        assert_eq!(breakeven_stop(dec!(100.004), Some(dec!(102)), dec!(98.5), true), None);
        assert_eq!(breakeven_stop(dec!(100.004), Some(dec!(102)), dec!(98), true), Some(dec!(100.00)));
    }

    #[test]
    fn session_close_triggers_controlled_exit() {
        let mut r = rules(None, None, None);
        r.session_close_due = true;
        assert_eq!(
            evaluate_exit(dec!(100), Utc::now(), &r),
            Some(ExitReason::SessionClose)
        );
    }

    #[test]
    fn emergency_stop_triggers_immediately() {
        let mut r = rules(Some(dec!(90)), Some(dec!(110)), None);
        r.emergency_stop = true;
        assert_eq!(
            evaluate_exit(dec!(100), Utc::now(), &r),
            Some(ExitReason::EmergencyStop)
        );
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
    fn an_unanswered_exit_keeps_its_reason_and_a_refused_one_records_why() {
        // ORCL, 3 Nov 2026: the call failed without a reply and the sale filled.
        assert_eq!(
            failed_exit_record("Failed to place Alpaca order for ORCL: error sending request"),
            ("UNKNOWN", None)
        );
        let refused = "Failed to place Alpaca order for ORCL: HTTP 403: insufficient qty";
        assert_eq!(failed_exit_record(refused), ("REJECTED", Some(refused)));
    }

    #[test]
    fn exit_reasons_have_stable_stored_names() {
        assert_eq!(ExitReason::StopLoss.as_str(), "STOP_LOSS");
        assert_eq!(ExitReason::TakeProfit.as_str(), "TAKE_PROFIT");
        assert_eq!(ExitReason::MaxHold.as_str(), "MAX_HOLD");
        assert_eq!(ExitReason::TrailingStop.as_str(), "TRAILING_STOP");
    }

    #[test]
    fn a_short_stops_out_above_and_takes_profit_below() {
        let mut r = rules(Some(dec!(105)), Some(dec!(90)), None);
        r.is_short = true;
        assert_eq!(evaluate_exit(dec!(104), Utc::now(), &r), None);
        assert_eq!(
            evaluate_exit(dec!(105), Utc::now(), &r),
            Some(ExitReason::StopLoss)
        );
        assert_eq!(
            evaluate_exit(dec!(90), Utc::now(), &r),
            Some(ExitReason::TakeProfit)
        );
        // The same levels on a long would be a stop below and a target above.
        r.is_short = false;
        assert_eq!(
            evaluate_exit(dec!(104), Utc::now(), &r),
            Some(ExitReason::StopLoss)
        );
    }

    #[test]
    fn a_short_profits_when_the_price_falls() {
        assert_eq!(position_pnl(dec!(100), dec!(95), dec!(4), true), dec!(20));
        assert_eq!(position_pnl(dec!(100), dec!(103), dec!(4), true), dec!(-12));
        assert_eq!(position_pnl(dec!(100), dec!(103), dec!(4), false), dec!(12));
        assert_eq!(closing_side(true), Side::Buy);
        assert_eq!(closing_side(false), Side::Sell);
    }

    #[test]
    fn a_raised_stop_reports_as_a_trailing_stop() {
        let mut r = rules(Some(dec!(105)), None, None);
        r.stop_trailed = true;
        assert_eq!(
            evaluate_exit(dec!(104), Utc::now(), &r),
            Some(ExitReason::TrailingStop)
        );
        assert_eq!(evaluate_exit(dec!(106), Utc::now(), &r), None);
    }
}
