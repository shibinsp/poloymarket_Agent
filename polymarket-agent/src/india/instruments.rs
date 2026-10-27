//! NSE cash equity instruments and Upstox identifier mappings.
//!
//! Maps standard trading symbols to verified Upstox instrument keys (`NSE_EQ|ISIN`).
//! All instruments trade in INR, with ₹0.05 tick size and whole share quantities (lot size = 1).

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::venue::types::{AssetClass, Instrument, InstrumentId, InstrumentMeta, VenueId};

/// Metadata for a verified NSE cash equity instrument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NseInstrument {
    pub symbol: String,
    pub isin: String,
    pub name: String,
    pub upstox_key: String,
    pub tick_size: Decimal,
    pub lot_size: Decimal,
}

impl NseInstrument {
    pub fn new(symbol: &str, isin: &str, name: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            isin: isin.to_string(),
            name: name.to_string(),
            upstox_key: format!("NSE_EQ|{isin}"),
            tick_size: dec!(0.05),
            lot_size: dec!(1.0),
        }
    }

    /// Convert to the venue-agnostic `Instrument` struct.
    pub fn to_venue_instrument(&self, venue_id: &VenueId) -> Instrument {
        Instrument {
            id: InstrumentId::new(venue_id.clone(), self.symbol.clone()),
            asset_class: AssetClass::Equity,
            display_name: format!("{} ({})", self.name, self.symbol),
            quote_ccy: "INR".to_string(),
            tick_size: Some(self.tick_size),
            lot_size: Some(self.lot_size),
            min_notional: Some(dec!(1.0)),
            min_qty: Some(dec!(1.0)),
            fractional: false, // NSE cash equities strictly require whole shares
            meta: InstrumentMeta::Equity {
                exchange: "NSE".to_string(),
            },
        }
    }
}

/// Standard curated watchlist of top liquid NSE cash equities matching Upstox Pro watchlist.
pub fn nse_watchlist() -> Vec<NseInstrument> {
    vec![
        NseInstrument::new("RELIANCE", "INE002A01018", "Reliance Industries Ltd"),
        NseInstrument::new("HDFCBANK", "INE040A01034", "HDFC Bank Ltd"),
        NseInstrument::new("TCS", "INE467B01029", "Tata Consultancy Services Ltd"),
        NseInstrument::new("BHARTIARTL", "INE397D01024", "Bharti Airtel Ltd"),
        NseInstrument::new("ICICIBANK", "INE090A01021", "ICICI Bank Ltd"),
        NseInstrument::new("SBIN", "INE062A01020", "State Bank of India"),
        NseInstrument::new("INFY", "INE009A01021", "Infosys Ltd"),
        NseInstrument::new(
            "LICI",
            "INE115A01015",
            "Life Insurance Corporation of India",
        ),
        NseInstrument::new("HINDUNILVR", "INE030A01027", "Hindustan Unilever Ltd"),
        NseInstrument::new("BAJFINANCE", "INE296A01024", "Bajaj Finance Ltd"),
        NseInstrument::new("ITC", "INE154A01025", "ITC Ltd"),
        NseInstrument::new("LT", "INE018A01030", "Larsen & Toubro Ltd"),
        NseInstrument::new("TATAMOTORS", "INE155A01022", "Tata Motors Ltd"),
    ]
}

/// Lookup an NSE instrument by symbol or ISIN.
pub fn find_nse_instrument(query: &str) -> Option<NseInstrument> {
    let q = query.trim().to_uppercase();
    nse_watchlist()
        .into_iter()
        .find(|inst| inst.symbol == q || inst.isin == q || inst.upstox_key == q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchlist_has_verified_keys() {
        let list = nse_watchlist();
        assert_eq!(list.len(), 13);
        let rel = find_nse_instrument("RELIANCE").unwrap();
        assert_eq!(rel.isin, "INE002A01018");
        assert_eq!(rel.upstox_key, "NSE_EQ|INE002A01018");
        assert_eq!(rel.tick_size, dec!(0.05));
        assert_eq!(rel.lot_size, dec!(1.0));
    }

    #[test]
    fn converts_to_venue_instrument_with_inr_and_whole_shares() {
        let tcs = find_nse_instrument("TCS").unwrap();
        let inst = tcs.to_venue_instrument(&VenueId::new("upstox_sandbox"));
        assert_eq!(inst.quote_ccy, "INR");
        assert!(!inst.fractional);
        assert_eq!(inst.min_qty, Some(dec!(1.0)));
        assert_eq!(inst.tick_size, Some(dec!(0.05)));
    }
}
