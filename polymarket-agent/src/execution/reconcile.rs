//! Does the ledger still describe reality?
//!
//! Distinct from `agent::reconcile`, which resolves *orders* — asking the
//! venue what happened to a submission whose fate is unknown. This pass asks
//! a different and blunter question: forget the orders, does the set of
//! positions the agent believes it holds match the set the venue says it
//! holds? Every downstream decision assumes it does. Sizing reads local
//! exposure, the exit pass reads local positions, the breakers read local
//! equity — and none of them would notice being wrong.
//!
//! The failure this catches is specific and expensive. A fill that arrives
//! during a crash, a manual trade in the broker's web UI, an order that
//! filled after the process was killed: each leaves a position at the venue
//! that the agent will never mark, never stop out and never close, while it
//! goes on sizing new entries as though the capital were free.
//!
//! The venue is the authority. Local records are the thing being audited.
//!
//! **One deliberate omission.** The plan called for a cash check —
//! venue balance against a locally-expected balance, within ±max(1%, $1).
//! That is not implemented, because this codebase keeps no local cash ledger
//! to compare against, and the nearest available number is not a substitute:
//! Alpaca's `available` is `non_marginable_buying_power`, which moves with
//! margin state, dividends and settlement timing, none of which the agent
//! records. Comparing against it would produce mismatches that are not
//! mismatches, and a false halt is an outage with extra steps. What *is*
//! checked is that the venue reports an equity figure at all, because the
//! circuit breaker is blind without one.

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{info, instrument, warn};

use crate::db::store::Store;
use crate::venue::types::AssetClass;
use crate::venue::{Venue, VenueRegistry};

/// What one venue's audit concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Local records and the venue agree.
    Clean,
    /// The venue could not be asked. Nothing is known to be wrong, and
    /// nothing is known to be right. Not a halt: a venue that cannot answer
    /// also cannot accept orders, so trading there has already stopped.
    Unverified,
    /// They disagree. This is a halt.
    Mismatch,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Unverified => "unverified",
            Self::Mismatch => "mismatch",
        }
    }
}

/// A position one side knows about and the other does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanPosition {
    pub symbol: String,
    pub qty: Decimal,
}

/// A position both sides know about, in different sizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QtyMismatch {
    pub symbol: String,
    pub local_qty: Decimal,
    pub venue_qty: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueReconcileReport {
    pub venue_id: String,
    pub verdict: Verdict,
    /// Held at the venue, absent from the ledger. The dangerous direction:
    /// nothing will ever exit these.
    pub missing_locally: Vec<OrphanPosition>,
    /// In the ledger, absent at the venue. Usually a close the agent did not
    /// record; it inflates exposure and starves sizing.
    pub missing_on_venue: Vec<OrphanPosition>,
    pub qty_mismatches: Vec<QtyMismatch>,
    /// Orders resting at the venue that the ledger has no record of.
    pub unknown_open_orders: Vec<String>,
    /// Account equity as the venue reports it, if it will.
    pub equity: Option<Decimal>,
    pub detail: String,
}

impl VenueReconcileReport {
    pub fn passed(&self) -> bool {
        self.verdict == Verdict::Clean
    }

    pub fn mismatch_count(&self) -> usize {
        self.missing_locally.len() + self.missing_on_venue.len() + self.qty_mismatches.len()
    }
}

/// Position sizes below this are treated as flat.
///
/// Venues report dust: a "closed" crypto position can leave 3e-9 of a coin
/// behind, and a fractional equity sale can leave a sliver that the venue
/// itself will not let you trade. Treating dust as a real position would make
/// every audit fail forever, which trains an operator to ignore the alert —
/// the failure mode that makes every other control here pointless.
const DUST: Decimal = dec!(0.000001);

/// How far two quantities may differ and still be called equal.
fn qty_tolerance(asset_class: AssetClass) -> Decimal {
    match asset_class {
        // Crypto quantities carry eight decimals and are computed from
        // fractional fills on both sides; the last place is noise.
        AssetClass::CryptoSpot => dec!(0.00000001),
        // Share counts are exact, including Alpaca's fractional shares, which
        // are quoted to six places and arrive as decimal strings on both
        // sides. Any difference at all is a real difference.
        AssetClass::Equity | AssetClass::PredictionBinary => Decimal::ZERO,
    }
}

fn asset_class_of(label: &str) -> AssetClass {
    match label {
        "crypto_spot" => AssetClass::CryptoSpot,
        "prediction_binary" => AssetClass::PredictionBinary,
        _ => AssetClass::Equity,
    }
}

pub struct StateReconciler<'a> {
    pub registry: &'a VenueRegistry,
    pub store: &'a Store,
}

impl StateReconciler<'_> {
    /// Audit every venue. One venue failing must not stop the others.
    #[instrument(skip(self), fields(otel.name = "agent.reconcile_state"))]
    pub async fn run(&self, now: DateTime<Utc>, cycle: i64) -> Result<Vec<VenueReconcileReport>> {
        // Read the ledger once and partition it, rather than re-querying per
        // venue: two queries a cycle apart could straddle a fill and produce
        // a mismatch that never existed at any single instant.
        let open = self.store.get_open_venue_trades().await?;

        let mut by_venue: HashMap<String, Vec<&crate::db::store::VenueOpenTrade>> = HashMap::new();
        for trade in &open {
            by_venue
                .entry(trade.venue_id.clone())
                .or_default()
                .push(trade);
        }

        let mut reports = Vec::new();
        for venue in self.registry.all() {
            let local = by_venue
                .get(venue.id().as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let report = self.audit(venue, local, now).await;
            if let Err(e) = self.persist(&report, cycle).await {
                // The audit result is more important than the audit trail;
                // losing the row must not lose the finding.
                warn!(venue = %report.venue_id, error = %e, "Failed to record reconciliation run");
            }
            reports.push(report);
        }
        Ok(reports)
    }

    async fn audit(
        &self,
        venue: &dyn Venue,
        local: &[&crate::db::store::VenueOpenTrade],
        _now: DateTime<Utc>,
    ) -> VenueReconcileReport {
        let venue_id = venue.id().to_string();

        let venue_positions = match venue.positions().await {
            Ok(p) => p,
            Err(e) => {
                // Critically *not* "everything is missing". An API error that
                // reads as an empty book would report every open position as
                // gone and halt on a transient 503.
                return VenueReconcileReport {
                    venue_id,
                    verdict: Verdict::Unverified,
                    missing_locally: Vec::new(),
                    missing_on_venue: Vec::new(),
                    qty_mismatches: Vec::new(),
                    unknown_open_orders: Vec::new(),
                    equity: None,
                    detail: format!("positions unavailable: {e}"),
                };
            }
        };

        // Local ledger, netted per symbol: two partial entries on the same
        // symbol are one position at the venue.
        let mut local_qty: HashMap<String, (Decimal, AssetClass)> = HashMap::new();
        for trade in local {
            let key = trade.symbol.to_ascii_uppercase();
            let entry = local_qty
                .entry(key)
                .or_insert((Decimal::ZERO, asset_class_of(&trade.asset_class)));
            entry.0 += trade.quantity;
        }

        let mut venue_qty: HashMap<String, Decimal> = HashMap::new();
        for pos in &venue_positions {
            *venue_qty
                .entry(pos.instrument.symbol.to_ascii_uppercase())
                .or_insert(Decimal::ZERO) += pos.qty;
        }

        let mut missing_locally = Vec::new();
        let mut missing_on_venue = Vec::new();
        let mut qty_mismatches = Vec::new();

        for (symbol, (lq, class)) in &local_qty {
            let vq = venue_qty.get(symbol).copied().unwrap_or(Decimal::ZERO);
            // Both sides dust is flat on both sides. Without this, a
            // sub-DUST local leftover against a flat venue skipped the
            // missing-on-venue branch (its own `lq > DUST` guard fails) and
            // fell through to the quantity comparison, which for equities has
            // a zero tolerance — so the audit failed forever on a remainder
            // too small to trade. That is the "trains an operator to ignore
            // the alert" outcome DUST exists to prevent.
            if vq.abs() <= DUST && lq.abs() <= DUST {
                continue;
            }
            if vq.abs() <= DUST && lq.abs() > DUST {
                missing_on_venue.push(OrphanPosition {
                    symbol: symbol.clone(),
                    qty: *lq,
                });
            } else if (*lq - vq).abs() > qty_tolerance(*class) {
                qty_mismatches.push(QtyMismatch {
                    symbol: symbol.clone(),
                    local_qty: *lq,
                    venue_qty: vq,
                });
            }
        }

        for (symbol, vq) in &venue_qty {
            if vq.abs() <= DUST {
                continue;
            }
            if !local_qty.contains_key(symbol) {
                missing_locally.push(OrphanPosition {
                    symbol: symbol.clone(),
                    qty: *vq,
                });
            }
        }

        // Resting orders the ledger has never heard of. An orphan from a
        // crash between "submitted" and "recorded" shows up here and nowhere
        // else.
        // `None` means the comparison could not be made, which is not the
        // same as "nothing was found" — see the verdict below.
        let unknown_open_orders: Option<Vec<String>> = match venue.open_orders().await {
            Ok(acks) => {
                // A store failure here must not read as an empty known-set.
                // It would mark every resting order unknown, and every
                // unknown order is a Mismatch, which is an UntilResume halt
                // needing a human — so one second of SQLite lock contention
                // would stop the agent until somebody noticed. The positions
                // branch above guards exactly this; this one did not.
                match self.store.get_unresolved_orders().await {
                    Ok(orders) => {
                        let known: std::collections::HashSet<_> =
                            orders.into_iter().map(|o| o.client_order_id).collect();
                        Some(
                            acks.into_iter()
                                .filter(|a| !known.contains(&a.client_order_id))
                                .map(|a| a.client_order_id)
                                .collect(),
                        )
                    }
                    Err(e) => {
                        warn!(
                            venue = %venue_id,
                            error = %e,
                            "Could not read local orders — resting orders are unverified, not unknown"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                // The venue would not say. Same reasoning: unverified, not
                // clean, and certainly not a mismatch.
                warn!(venue = %venue_id, error = %e, "Could not list open orders during reconciliation");
                None
            }
        };
        let orders_unverified = unknown_open_orders.is_none();
        let unknown_open_orders = unknown_open_orders.unwrap_or_default();

        let equity = venue.balance().await.ok().and_then(|b| b.total);

        let mismatched = !missing_locally.is_empty()
            || !missing_on_venue.is_empty()
            || !qty_mismatches.is_empty()
            || !unknown_open_orders.is_empty();

        let verdict = if mismatched {
            Verdict::Mismatch
        } else if orders_unverified || equity.is_none() {
            // Not a mismatch — nothing disagrees — but the breakers cannot
            // run without an equity figure, so it must not read as clean.
            Verdict::Unverified
        } else {
            Verdict::Clean
        };

        let detail = if mismatched {
            let mut parts = Vec::new();
            if !missing_locally.is_empty() {
                parts.push(format!(
                    "{} position(s) held at the venue but absent locally: {}",
                    missing_locally.len(),
                    describe(&missing_locally)
                ));
            }
            if !missing_on_venue.is_empty() {
                parts.push(format!(
                    "{} position(s) recorded locally but absent at the venue: {}",
                    missing_on_venue.len(),
                    describe(&missing_on_venue)
                ));
            }
            if !qty_mismatches.is_empty() {
                parts.push(format!(
                    "{} quantity mismatch(es): {}",
                    qty_mismatches.len(),
                    qty_mismatches
                        .iter()
                        .map(|m| format!(
                            "{} local {} vs venue {}",
                            m.symbol, m.local_qty, m.venue_qty
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !unknown_open_orders.is_empty() {
                parts.push(format!(
                    "{} resting order(s) with no local record: {}",
                    unknown_open_orders.len(),
                    unknown_open_orders.join(", ")
                ));
            }
            parts.join("; ")
        } else if orders_unverified {
            "resting orders could not be compared against the ledger".to_string()
        } else if equity.is_none() {
            "venue reports no account equity — the circuit breaker cannot run".to_string()
        } else {
            "ledger matches the venue".to_string()
        };

        info!(
            venue = %venue_id,
            verdict = verdict.as_str(),
            local_positions = local_qty.len(),
            venue_positions = venue_qty.len(),
            "Reconciled venue state"
        );

        VenueReconcileReport {
            venue_id,
            verdict,
            missing_locally,
            missing_on_venue,
            qty_mismatches,
            unknown_open_orders,
            equity,
            detail,
        }
    }

    async fn persist(&self, report: &VenueReconcileReport, cycle: i64) -> Result<()> {
        self.store
            .insert_reconciliation_run(
                &report.venue_id,
                cycle,
                report.equity,
                report.missing_locally.len() as i64,
                report.missing_on_venue.len() as i64,
                report.qty_mismatches.len() as i64,
                report.unknown_open_orders.len() as i64,
                report.passed(),
                &report.detail,
            )
            .await
    }
}

fn describe(positions: &[OrphanPosition]) -> String {
    positions
        .iter()
        .map(|p| format!("{} x{}", p.symbol, p.qty))
        .collect::<Vec<_>>()
        .join(", ")
}
