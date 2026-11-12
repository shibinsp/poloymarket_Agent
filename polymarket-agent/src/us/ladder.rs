//! Capital ladder for the US books (added 9 Nov 2026 at the user's request:
//! "if this cap we will get this profit").
//!
//! The US books share one Alpaca paper account, so the copies are kept here,
//! not sent as orders. Each real trade, once closed, is re-sized at every
//! starting amount, the same way as `reports/capital_ladder.py`:
//!   breakout - fractional shares, the amount split across that night's entries
//!              (Alpaca's $1 minimum order applies);
//!   options  - whole contracts the amount can pay for;
//!   spreads  - whole spreads, each tying up (width - credit) x 100.
//! The cash a trade may use is min(amount + realized, amount).
//! No orders are placed; this only reads closed trades and writes ledger rows.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, Utc};
use chrono_tz::America::New_York;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::db::store::Store;

/// Starting amounts in dollars.
pub const AMOUNTS: [i64; 12] = [10, 15, 20, 25, 50, 100, 250, 500, 750, 1_000, 1_500, 2_000];
/// First New York day the ladder covers (the capital-ladder week).
pub const START_DAY: &str = "2026-11-09";
/// 9 Nov 00:00 New York in UTC, for the breakout trades' created_at.
const START_UTC: &str = "2026-11-09 05:00:00";

pub const ORB: &str = "us_orb";
pub const OPTIONS: &str = "us_options";
pub const SPREADS: &str = "us_spreads";
pub const IBS: &str = "us_ibs";
pub const BOOKS: [&str; 4] = [ORB, OPTIONS, SPREADS, IBS];

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s.trim())
        .or_else(|_| Decimal::from_scientific(s.trim()))
        .unwrap_or_default()
}

/// New York date of a stored UTC timestamp.
pub fn ny_day(ts: &str) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(ts)
        .map(|t| t.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S%.f").map(|n| n.and_utc())
        })
        .ok()?;
    Some(t.with_timezone(&New_York).date_naive().to_string())
}

/// Cash a trade may use: profits are not added, losses shrink it.
pub fn cash(amount: Decimal, realized: Decimal) -> Decimal {
    (amount + realized).min(amount)
}

/// Whole option contracts `cash` can buy at `premium` a share.
pub fn contracts(cash: Decimal, premium: Decimal) -> i64 {
    if premium <= Decimal::ZERO {
        return 0;
    }
    (cash / (premium * dec!(100))).floor().to_i64().unwrap_or(0).max(0)
}

/// Spreads handed out one at a time across the day's trades while the cash
/// covers each one's margin.
pub fn allocate_spreads(mut cash: Decimal, need: &[Decimal]) -> Vec<i64> {
    let mut n = vec![0; need.len()];
    if need.iter().any(|m| *m <= Decimal::ZERO) {
        return n;
    }
    loop {
        let mut added = false;
        for (i, m) in need.iter().enumerate() {
            if *m <= cash {
                n[i] += 1;
                cash -= *m;
                added = true;
            }
        }
        if !added {
            return n;
        }
    }
}

async fn realized_before(store: &Store, book: &str, amount: i64, day: &str) -> Result<Decimal> {
    Ok(store
        .us_ladder_rows(book, amount)
        .await?
        .iter()
        .filter(|(d, _, _)| d.as_str() < day)
        .map(|(_, _, p)| dec(p))
        .sum())
}

async fn realized_all(store: &Store, book: &str, amount: i64) -> Result<Decimal> {
    Ok(store
        .us_ladder_rows(book, amount)
        .await?
        .iter()
        .map(|(_, _, p)| dec(p))
        .sum())
}

/// Add every newly closed US trade to the ladder.
pub async fn sync(store: &Store, now: DateTime<Utc>) -> Result<usize> {
    let mut added = 0;

    // Breakout: split across the night's entries.
    let done = store.us_ladder_done(ORB).await?;
    let mut per_night: HashMap<String, i64> = HashMap::new();
    for ts in store.us_ladder_orb_entries(START_UTC).await? {
        if let Some(d) = ny_day(&ts) {
            *per_night.entry(d).or_default() += 1;
        }
    }
    for (id, symbol, created, qty, fill, pnl) in store.us_ladder_orb_sources(START_UTC).await? {
        if done.contains(&id) {
            continue;
        }
        let Some(day) = ny_day(&created) else { continue };
        let notional = dec(&qty).abs() * dec(&fill);
        if notional <= Decimal::ZERO {
            continue;
        }
        let ret = dec(&pnl) / notional;
        let n = Decimal::from((*per_night.get(&day).unwrap_or(&1)).max(1));
        for amount in AMOUNTS {
            let c = cash(Decimal::from(amount), realized_before(store, ORB, amount, &day).await?) / n;
            let (units, p) = if c >= Decimal::ONE {
                ((c / dec(&fill)).round_dp(6), c * ret)
            } else {
                (Decimal::ZERO, Decimal::ZERO)
            };
            store.insert_us_ladder(ORB, amount, id, &day, &symbol, units, p, now).await?;
        }
        added += 1;
    }

    // Options: whole contracts.
    let done = store.us_ladder_done(OPTIONS).await?;
    for (id, day, contract, entry, exit) in store.us_ladder_option_sources(START_DAY).await? {
        if done.contains(&id) {
            continue;
        }
        let (e, x) = (dec(&entry), dec(&exit));
        for amount in AMOUNTS {
            let c = cash(Decimal::from(amount), realized_all(store, OPTIONS, amount).await?);
            let k = contracts(c, e);
            let p = (x - e) * dec!(100) * Decimal::from(k);
            store.insert_us_ladder(OPTIONS, amount, id, &day, &contract, Decimal::from(k), p, now).await?;
        }
        added += 1;
    }

    // IBS: a third of the amount per fund, fractional shares.
    let done = store.us_ladder_done(IBS).await?;
    for (id, day, symbol, qty, entry, pnl) in store.us_ladder_ibs_sources().await? {
        if done.contains(&id) {
            continue;
        }
        let notional = dec(&qty) * dec(&entry);
        if notional <= Decimal::ZERO {
            continue;
        }
        let ret = dec(&pnl) / notional;
        let parts = Decimal::from(crate::us::ibs::FUNDS.len() as u32);
        for amount in AMOUNTS {
            let c = cash(Decimal::from(amount), realized_before(store, IBS, amount, &day).await?) / parts;
            let (units, p) = if c >= Decimal::ONE {
                ((c / dec(&entry)).round_dp(6), c * ret)
            } else {
                (Decimal::ZERO, Decimal::ZERO)
            };
            store.insert_us_ladder(IBS, amount, id, &day, &symbol, units, p, now).await?;
        }
        added += 1;
    }

    // Spreads: a day is sized once all its spreads are finished.
    let done = store.us_ladder_done(SPREADS).await?;
    let mut by_day: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for row in store.us_ladder_spread_sources(START_DAY).await? {
        by_day.entry(row.1.clone()).or_default().push(row);
    }
    for (day, rows) in by_day {
        if rows.iter().any(|r| r.7 != "CLOSED" && r.7 != "CANCELLED") {
            continue;
        }
        let closed: Vec<_> = rows
            .into_iter()
            .filter(|r| r.7 == "CLOSED" && r.5.is_some() && r.6.is_some() && !done.contains(&r.0))
            .collect();
        if closed.is_empty() {
            continue;
        }
        let need: Vec<Decimal> = closed
            .iter()
            .map(|r| (dec(&r.4) - dec(&r.3) - dec(r.5.as_deref().unwrap_or("0"))) * dec!(100))
            .collect();
        for amount in AMOUNTS {
            let c = cash(Decimal::from(amount), realized_before(store, SPREADS, amount, &day).await?);
            let n = allocate_spreads(c, &need);
            for (r, k) in closed.iter().zip(n) {
                let per = (dec(r.5.as_deref().unwrap_or("0")) - dec(r.6.as_deref().unwrap_or("0"))) * dec!(100);
                let label = format!("{} {}/{}", r.2, r.3, r.4);
                store
                    .insert_us_ladder(SPREADS, amount, r.0, &day, &label, Decimal::from(k), per * Decimal::from(k), now)
                    .await?;
            }
        }
        added += closed.len();
    }
    Ok(added)
}

/// Check for newly closed US trades every minute. Paper bookkeeping only.
pub fn spawn_us_ladder_worker(store: Store) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!("US capital ladder started ($10-$2,000 copies of each closed US trade; no orders)");
        loop {
            match sync(&store, Utc::now()).await {
                Ok(0) => {}
                Ok(n) => info!(trades = n, "US capital ladder: closed trades re-sized at every amount"),
                Err(e) => warn!(error = %format!("{e:#}"), "US capital ladder sync failed"),
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ny_day_handles_both_timestamp_shapes() {
        assert_eq!(ny_day("2026-11-10 02:00:00").as_deref(), Some("2026-11-09"));
        assert_eq!(ny_day("2026-11-09T15:00:00+00:00").as_deref(), Some("2026-11-09"));
    }

    #[test]
    fn cash_keeps_profits_aside() {
        assert_eq!(cash(dec!(100), dec!(20)), dec!(100));
        assert_eq!(cash(dec!(100), dec!(-30)), dec!(70));
    }

    #[test]
    fn small_amounts_cannot_buy_a_contract() {
        assert_eq!(contracts(dec!(250), dec!(1.58)), 1);
        assert_eq!(contracts(dec!(100), dec!(1.58)), 0);
    }

    #[test]
    fn spreads_go_out_one_at_a_time() {
        assert_eq!(allocate_spreads(dec!(400), &[dec!(170), dec!(180)]), vec![1, 1]);
        assert_eq!(allocate_spreads(dec!(500), &[dec!(170), dec!(180)]), vec![1, 1]);
        assert_eq!(allocate_spreads(dec!(530), &[dec!(170), dec!(180)]), vec![2, 1]);
        assert_eq!(allocate_spreads(dec!(100), &[dec!(170)]), vec![0]);
    }
}
