//! US-listed liquid stocks and ETFs for automated intraday testing.
//!
//! Maps symbols (SPY, QQQ, AAPL, MSFT, NVDA, AMZN) to US market metadata.
//! All instruments trade in USD, with $0.01 tick size and fractional or whole share quantities.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::venue::types::{AssetClass, Instrument, InstrumentId, InstrumentMeta, VenueId};

/// Metadata for a verified US-listed cash equity or ETF instrument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsInstrument {
    pub symbol: String,
    pub name: String,
    pub exchange: String,
    pub tick_size: Decimal,
    pub lot_size: Decimal,
    pub min_notional: Decimal,
}

impl UsInstrument {
    pub fn new(symbol: &str, name: &str, exchange: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            name: name.to_string(),
            exchange: exchange.to_string(),
            tick_size: dec!(0.01),
            lot_size: dec!(1.0),
            min_notional: dec!(1.0),
        }
    }

    /// Convert to the venue-agnostic `Instrument` struct.
    pub fn to_venue_instrument(&self, venue_id: &VenueId) -> Instrument {
        Instrument {
            id: InstrumentId::new(venue_id.clone(), self.symbol.clone()),
            asset_class: AssetClass::Equity,
            display_name: format!("{} ({})", self.name, self.symbol),
            quote_ccy: "USD".to_string(),
            tick_size: Some(self.tick_size),
            lot_size: Some(self.lot_size),
            min_notional: Some(self.min_notional),
            min_qty: Some(dec!(0.001)), // Alpaca and US brokers support fractional shares
            fractional: true,
            meta: InstrumentMeta::Equity {
                exchange: self.exchange.clone(),
            },
        }
    }
}

/// Standard curated watchlist of liquid US equities and ETFs.
pub fn us_watchlist() -> Vec<UsInstrument> {
    vec![
        UsInstrument::new("SPY", "SPDR S&P 500 ETF Trust", "NYSE"),
        UsInstrument::new("QQQ", "Invesco QQQ Trust", "NASDAQ"),
        UsInstrument::new("AAPL", "Apple Inc.", "NASDAQ"),
        UsInstrument::new("NVDA", "NVIDIA Corp.", "NASDAQ"),
        UsInstrument::new("MSFT", "Microsoft Corp.", "NASDAQ"),
        UsInstrument::new("AMZN", "Amazon.com Inc.", "NASDAQ"),
        UsInstrument::new("TSLA", "Tesla Inc.", "NASDAQ"),
        UsInstrument::new("META", "Meta Platforms Inc.", "NASDAQ"),
        UsInstrument::new("GOOGL", "Alphabet Inc.", "NASDAQ"),
        UsInstrument::new("PLUG", "Plug Power Inc. ($1.40)", "NASDAQ"),
        UsInstrument::new("NIO", "NIO Inc. ADR ($3.27)", "NYSE"),
        UsInstrument::new("LCID", "Lucid Group Inc. ($3.51)", "NASDAQ"),
        UsInstrument::new("NOK", "Nokia Corporation ADR ($8.73)", "NYSE"),
        UsInstrument::new("SOFI", "SoFi Technologies Inc. ($14.52)", "NASDAQ"),
        UsInstrument::new("SIRI", "Sirius XM Holdings Inc. ($27.28)", "NASDAQ"),
    ]
}

/// Lookup a US instrument by symbol.
pub fn find_us_instrument(query: &str) -> Option<UsInstrument> {
    let q = query.trim().to_uppercase();
    us_watchlist().into_iter().find(|inst| inst.symbol == q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_watchlist_has_top_liquid_names() {
        let list = us_watchlist();
        assert_eq!(list.len(), 15);
        let spy = find_us_instrument("SPY").unwrap();
        assert_eq!(spy.exchange, "NYSE");
        assert_eq!(spy.tick_size, dec!(0.01));
    }

    #[test]
    fn converts_to_venue_instrument_with_usd() {
        let aapl = find_us_instrument("AAPL").unwrap();
        let inst = aapl.to_venue_instrument(&VenueId::new("alpaca"));
        assert_eq!(inst.quote_ccy, "USD");
        assert!(inst.fractional);
        assert_eq!(inst.tick_size, Some(dec!(0.01)));
    }
}
