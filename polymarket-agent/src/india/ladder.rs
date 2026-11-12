//! Capital ladder: paper copies of each real India book at every starting
//! amount (added 9 Nov 2026 at the user's request, "use every cap").
//!
//! Each copy takes exactly the trades its real book takes (same stock, side,
//! price and stop; TimesFM filter unchanged) and buys as many whole shares as
//! its own amount can pay for after charges. Like the real books, losses come
//! off the amount and profits are set aside. A copy whose amount cannot buy
//! one share skips that trade. Copies close at the same stop or 15:15.
//! Display only: kept out of the daily target, reviews and the trades list.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tracing::{info, warn};

use crate::db::store::Store;
use crate::india::orb::{manage_open, trading_cash, whole_shares, UpstoxBars};

/// Starting amounts in rupees.
pub const AMOUNTS: [u32; 8] = [500, 1_000, 2_500, 5_000, 7_500, 10_000, 15_000, 20_000];

/// Ledger name of one copy, e.g. `india_orb_cap500`.
pub fn ledger(book: &str, amount: u32) -> String {
    format!("{book}_cap{amount}")
}

/// True for a ladder copy's ledger name.
pub fn is_ladder(strategy: &str) -> bool {
    strategy
        .rsplit_once("_cap")
        .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Copy a trade the real `book` just opened onto every amount's ledger.
#[allow(clippy::too_many_arguments)]
pub async fn mirror_entry(
    store: &Store,
    book: &str,
    day: &str,
    symbol: &str,
    buy: bool,
    price: Decimal,
    stop: Decimal,
    now: DateTime<Utc>,
) {
    for amount in AMOUNTS {
        let name = ledger(book, amount);
        let capital = Decimal::from(amount);
        let lost = match store.intraday_losses(&name).await {
            Ok(l) => l,
            Err(e) => {
                warn!(ledger = %name, error = %e, "Ladder: could not read losses");
                continue;
            }
        };
        let qty = whole_shares(trading_cash(capital, lost), price, buy);
        if qty < 1 {
            info!(ledger = %name, symbol, %price, "Ladder: amount cannot buy one share; skipped");
            continue;
        }
        if let Err(e) = store
            .insert_intraday_trade(
                &name,
                day,
                symbol,
                buy,
                qty,
                &price.to_string(),
                &stop.to_string(),
                &capital.to_string(),
                now,
            )
            .await
        {
            warn!(ledger = %name, error = %e, "Ladder: could not open the copy");
        }
    }
}

/// Close every amount's open copy of `book` at its stop or 15:15.
pub async fn manage_all(data: &UpstoxBars, store: &Store, book: &str, now: DateTime<Utc>) {
    for amount in AMOUNTS {
        let name = ledger(book, amount);
        if let Err(e) = manage_open(data, store, &name, now).await {
            warn!(ledger = %name, error = %e, "Ladder: could not manage the open copy");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_names_round_trip() {
        assert_eq!(ledger("india_orb", 500), "india_orb_cap500");
        assert!(is_ladder("india_gapgo_cap20000"));
        assert!(!is_ladder("india_orb"));
        assert!(!is_ladder("india_orb_tfm_skip"));
        assert!(!is_ladder("india_orb_cap"));
    }
}
