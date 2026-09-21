//! Account equity for venues that report holdings without prices.
//!
//! Spot crypto venues answer "how much of each coin do you hold", never "what
//! is the account worth". But equity is the one input every loss limit is
//! measured against, so without it `check_breakers` halts rather than trade
//! with no risk controls — and reporting *cash* instead would read every entry
//! as an instant loss of the full notional and trip the drawdown breaker on a
//! flat book.
//!
//! This lives here rather than in each adapter because both need exactly the
//! same arithmetic, and the first version duplicated it byte-for-byte: every
//! correction below — the dust floor, the single-currency rule, what a failed
//! quote means — would otherwise have had to be made twice and would drift.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::warn;

use super::types::{InstrumentId, VenueId};
use super::Venue;

/// Holdings at or below this are not worth a network round trip.
///
/// A closed crypto position routinely leaves a few billionths of a coin
/// behind. Without a floor, that remainder demands a quote like any other
/// holding — and a single failed book call over $0.0006 of dust would blank
/// the account's equity and halt the agent. The same constant and the same
/// reasoning as the reconciler's own dust handling.
pub const DUST: Decimal = dec!(0.000001);

/// One non-cash holding to be marked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holding {
    /// The asset itself — `BTC`, not a pair.
    pub asset: String,
    /// Everything held, including what is committed to a resting order.
    pub qty: Decimal,
    /// The pair to price it with, when the venue has one configured.
    /// Otherwise `{asset}/{ccy}` is used, which is a *pricing* choice and not
    /// the symbol-attribution guess that reconciliation must never make.
    pub symbol: Option<String>,
}

/// Cash plus every holding marked at the current mid, in `ccy`.
///
/// `None` when a holding above the dust floor cannot be priced. That is not
/// caution for its own sake: a partial equity is a wrong equity, and no
/// breaker can tell one from a real loss — a missing position reads exactly
/// like a position that went to zero. A flat book needs no quotes at all,
/// which is the common case for a deployment that has not traded yet.
///
/// Every holding is priced, not only the configured ones. Leaving an
/// unconfigured holding out would report a partial equity *as authoritative*,
/// which is the failure this function exists to prevent — and an operator's
/// pre-existing coins are exactly the case the venue adapters call normal.
pub async fn mark_equity(
    venue: &dyn Venue,
    venue_id: &VenueId,
    ccy: &str,
    cash: Decimal,
    holdings: &[Holding],
) -> Option<Decimal> {
    let mut total = cash;

    for holding in holdings {
        if holding.qty <= DUST {
            continue;
        }
        let symbol = holding
            .symbol
            .clone()
            .unwrap_or_else(|| format!("{}/{}", holding.asset.to_uppercase(), ccy));
        let id = InstrumentId::new(venue_id.clone(), symbol.clone());

        match venue.quote(&id).await {
            Ok(quote) => total += holding.qty * quote.mid,
            Err(e) => {
                warn!(
                    venue = %venue_id,
                    symbol = %symbol,
                    qty = %holding.qty.normalize(),
                    error = %format!("{e:#}"),
                    "Could not price a holding, so account equity cannot be computed — the \
                     risk limits will report themselves unevaluable rather than run against \
                     a number that is missing a position"
                );
                return None;
            }
        }
    }

    Some(total)
}

/// The single currency a venue's pairs quote in.
///
/// `Balance` carries one number and one currency, so a venue whose pairs
/// quote in more than one cannot be reported faithfully: cash in the other
/// currency is either dropped from equity or added to it at an implied 1:1,
/// and both are wrong in a way no downstream code can detect. Two venue
/// entries express that configuration honestly; one cannot.
pub fn single_quote_currency(symbols: &[String]) -> Result<Option<String>, Vec<String>> {
    let mut found: Vec<String> = Vec::new();
    for symbol in symbols {
        let Some(quote) = symbol.split('/').nth(1) else {
            continue;
        };
        let quote = quote.trim().to_uppercase();
        if !quote.is_empty() && !found.contains(&quote) {
            found.push(quote);
        }
    }

    match found.len() {
        0 => Ok(None),
        1 => Ok(Some(found.remove(0))),
        _ => {
            found.sort();
            Err(found)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_quote_currency_is_the_normal_case() {
        let symbols = vec!["BTC/USD".to_string(), "ETH/USD".to_string()];
        assert_eq!(
            single_quote_currency(&symbols).unwrap().as_deref(),
            Some("USD")
        );
    }

    #[test]
    fn the_currency_is_read_case_insensitively() {
        let symbols = vec!["btc/usdt".to_string()];
        assert_eq!(
            single_quote_currency(&symbols).unwrap().as_deref(),
            Some("USDT")
        );
    }

    /// Cash in the other currency is either dropped from equity or added at an
    /// implied 1:1, and both are wrong in a way nothing downstream can detect.
    #[test]
    fn two_quote_currencies_are_refused_and_both_are_named() {
        let symbols = vec!["BTC/USD".to_string(), "ETH/USDT".to_string()];
        let err = single_quote_currency(&symbols).unwrap_err();
        assert_eq!(err, vec!["USD".to_string(), "USDT".to_string()]);
    }

    #[test]
    fn no_symbols_yields_no_currency_rather_than_an_error() {
        assert_eq!(single_quote_currency(&[]).unwrap(), None);
    }

    /// A closed position leaves billionths of a coin behind. Demanding a quote
    /// for that would let $0.0006 of dust blank the account's equity.
    #[test]
    fn dust_is_below_the_floor() {
        assert!(dec!(0.0000000001) <= DUST);
        assert!(dec!(0.001) > DUST);
    }
}
