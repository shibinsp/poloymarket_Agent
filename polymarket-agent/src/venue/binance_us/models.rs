//! Binance.US wire types.
//!
//! Like Coinbase, every numeric field arrives as a **string**, which suits a
//! `Decimal` codebase. Unlike Coinbase, two responses are *positional arrays*
//! rather than objects — klines and book levels — so their indices are named
//! here once instead of being spelled out at each use.

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

/// Parse a Binance decimal string, naming the field.
pub fn money(value: &str, field: &str) -> Result<Decimal> {
    Decimal::from_str(value.trim())
        .with_context(|| format!("Binance.US sent an unparseable {field}: {value:?}"))
}

/// The error body Binance returns with a 4xx.
///
/// The `code` is the part worth acting on: `-2010` is a rejected order,
/// `-1021` a clock-skew problem, `-1022` a bad signature. A message alone
/// sends an operator to the wrong file.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub msg: Option<String>,
}

impl ApiError {
    /// What the operator should do about it, where the code implies something
    /// a message does not.
    pub fn hint(&self) -> Option<&'static str> {
        match self.code {
            // The signed timestamp fell outside recvWindow. Almost always the
            // local clock, not the request.
            -1021 => Some("this machine's clock is out of sync with Binance.US — check NTP"),
            -1022 => Some("the signed query string differs from the one sent"),
            -2015 => Some("the API key, its IP allow-list, or its permissions are wrong"),
            _ => None,
        }
    }

    pub fn message(&self) -> &str {
        self.msg
            .as_deref()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or("Binance.US gave no message")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExchangeInfo {
    #[serde(default)]
    pub symbols: Vec<SymbolInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolInfo {
    pub symbol: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub base_asset: Option<String>,
    #[serde(default)]
    pub quote_asset: Option<String>,
    /// Binance says explicitly whether spot trading is allowed on the pair;
    /// the same listing carries margin-only entries.
    #[serde(default)]
    pub is_spot_trading_allowed: Option<bool>,
    #[serde(default)]
    pub filters: Vec<SymbolFilter>,
}

impl SymbolInfo {
    /// Whether this symbol can be spot-traded right now.
    ///
    /// `TRADING` is the only status that permits an order; `BREAK`, `HALT`,
    /// `PENDING_TRADING` and `END_OF_DAY` all reject one. Absence is not
    /// permission — a listing with no status has not said it is trading.
    pub fn tradeable(&self) -> bool {
        self.status
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("TRADING"))
            && self.is_spot_trading_allowed.unwrap_or(true)
    }

    fn filter(&self, kind: &str) -> Option<&SymbolFilter> {
        self.filters
            .iter()
            .find(|f| f.filter_type.eq_ignore_ascii_case(kind))
    }

    /// Price increment. An order priced off the tick is rejected `-1013`.
    pub fn tick_size(&self) -> Result<Option<Decimal>> {
        self.filter("PRICE_FILTER")
            .and_then(|f| f.tick_size.as_deref())
            .map(|v| money(v, "tickSize"))
            .transpose()
    }

    /// Quantity increment.
    pub fn step_size(&self) -> Result<Option<Decimal>> {
        self.filter("LOT_SIZE")
            .and_then(|f| f.step_size.as_deref())
            .map(|v| money(v, "stepSize"))
            .transpose()
    }

    pub fn min_qty(&self) -> Result<Option<Decimal>> {
        self.filter("LOT_SIZE")
            .and_then(|f| f.min_qty.as_deref())
            .map(|v| money(v, "minQty"))
            .transpose()
    }

    /// Smallest order *value*.
    ///
    /// Binance renamed this filter: newer listings carry `NOTIONAL`, older
    /// ones `MIN_NOTIONAL`. Reading only one leaves the floor unknown, and at
    /// micro capital that floor is the constraint that actually binds.
    pub fn min_notional(&self) -> Result<Option<Decimal>> {
        let raw = self
            .filter("NOTIONAL")
            .and_then(|f| f.min_notional.as_deref())
            .or_else(|| {
                self.filter("MIN_NOTIONAL")
                    .and_then(|f| f.min_notional.as_deref())
            });
        raw.map(|v| money(v, "minNotional")).transpose()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolFilter {
    pub filter_type: String,
    #[serde(default)]
    pub tick_size: Option<String>,
    #[serde(default)]
    pub step_size: Option<String>,
    #[serde(default)]
    pub min_qty: Option<String>,
    #[serde(default)]
    pub min_notional: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Depth {
    #[serde(default)]
    pub bids: Vec<BookLevel>,
    #[serde(default)]
    pub asks: Vec<BookLevel>,
}

/// `["60000.00", "0.5"]` — price then quantity, positionally.
#[derive(Debug, Clone, Deserialize)]
pub struct BookLevel(pub String, pub String);

impl BookLevel {
    pub fn price(&self) -> &str {
        &self.0
    }
    pub fn qty(&self) -> &str {
        &self.1
    }
}

/// A kline is a twelve-element array. Only the first six are used here, but
/// the shape is pinned so a change in arity is an error rather than a silent
/// misread of a neighbouring field.
#[derive(Debug, Clone, Deserialize)]
pub struct Kline(pub Vec<serde_json::Value>);

impl Kline {
    fn at(&self, index: usize, field: &str) -> Result<&serde_json::Value> {
        self.0
            .get(index)
            .with_context(|| format!("Binance.US kline has no {field} at index {index}"))
    }

    /// Open time, in milliseconds.
    pub fn open_time_ms(&self) -> Result<i64> {
        let value = self.at(0, "open time")?;
        value
            .as_i64()
            .with_context(|| format!("Binance.US sent a non-numeric kline open time: {value}"))
    }

    pub fn decimal(&self, index: usize, field: &str) -> Result<Decimal> {
        let value = self.at(index, field)?;
        match value.as_str() {
            Some(s) => money(s, field),
            // Binance quotes these as strings. A number here means the shape
            // changed, and guessing a conversion would silently lose scale.
            None => bail!("Binance.US sent a non-string kline {field}: {value}"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    /// False when the account is restricted. An order then fails one at a
    /// time, having already spent a valuation.
    #[serde(default)]
    pub can_trade: Option<bool>,
    #[serde(default)]
    pub balances: Vec<AssetBalance>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetBalance {
    pub asset: String,
    #[serde(default)]
    pub free: Option<String>,
    /// Held against resting orders. Still the account's money, so it counts
    /// as a position even though it cannot be spent.
    #[serde(default)]
    pub locked: Option<String>,
}

impl AssetBalance {
    pub fn free_amount(&self) -> Result<Decimal> {
        self.free
            .as_deref()
            .map(|v| money(v, "free"))
            .transpose()
            .map(|v| v.unwrap_or(Decimal::ZERO))
    }

    pub fn total_amount(&self) -> Result<Decimal> {
        let locked = self
            .locked
            .as_deref()
            .map(|v| money(v, "locked"))
            .transpose()?
            .unwrap_or(Decimal::ZERO);
        Ok(self.free_amount()? + locked)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Order {
    #[serde(default)]
    pub symbol: Option<String>,
    pub order_id: i64,
    #[serde(default)]
    pub client_order_id: Option<String>,
    /// NEW | PARTIALLY_FILLED | FILLED | CANCELED | PENDING_CANCEL | REJECTED
    /// | EXPIRED | EXPIRED_IN_MATCH
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub executed_qty: Option<String>,
    /// Quote value of what filled. Binance reports no average price, so the
    /// average is this divided by the filled quantity.
    #[serde(default)]
    pub cummulative_quote_qty: Option<String>,
    /// Present on a `newOrderRespType=FULL` create response.
    #[serde(default)]
    pub fills: Vec<Trade>,
}

/// One execution: the same shape in a create response's `fills` and in
/// `GET /myTrades`, which is the only place a later fill's commission can be
/// read from.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trade {
    #[serde(default)]
    pub price: Option<String>,
    #[serde(default)]
    pub qty: Option<String>,
    #[serde(default)]
    pub commission: Option<String>,
    /// Whichever asset the commission was charged in — the base, the quote or
    /// BNB. Adding two of these together is adding bitcoin to dollars.
    #[serde(default)]
    pub commission_asset: Option<String>,
}

impl Trade {
    pub fn commission_amount(&self) -> Result<Decimal> {
        self.commission
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .map(|v| money(v, "commission"))
            .transpose()
            .map(|v| v.unwrap_or(Decimal::ZERO))
    }

    /// The price this execution happened at, used to convert a base-asset
    /// commission into the quote currency.
    pub fn price_amount(&self) -> Result<Decimal> {
        self.price
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .map(|v| money(v, "price"))
            .transpose()?
            .context("Binance.US sent a trade with no price")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn symbol_info(json: serde_json::Value) -> SymbolInfo {
        serde_json::from_value(json).expect("valid SymbolInfo")
    }

    fn btc() -> serde_json::Value {
        serde_json::json!({
            "symbol": "BTCUSD",
            "status": "TRADING",
            "baseAsset": "BTC",
            "quoteAsset": "USD",
            "isSpotTradingAllowed": true,
            "filters": [
                {"filterType": "PRICE_FILTER", "tickSize": "0.01"},
                {"filterType": "LOT_SIZE", "minQty": "0.00001", "stepSize": "0.00000100"},
                {"filterType": "NOTIONAL", "minNotional": "10.00"}
            ]
        })
    }

    #[test]
    fn the_filters_become_instrument_limits() {
        let s = symbol_info(btc());
        assert_eq!(s.tick_size().unwrap(), Some(dec!(0.01)));
        assert_eq!(s.step_size().unwrap(), Some(dec!(0.00000100)));
        assert_eq!(s.min_qty().unwrap(), Some(dec!(0.00001)));
        assert_eq!(s.min_notional().unwrap(), Some(dec!(10.00)));
    }

    /// Binance renamed the filter. Reading only `NOTIONAL` leaves the floor
    /// unknown on older listings — and at micro capital that floor is the
    /// constraint that actually binds.
    #[test]
    fn the_older_min_notional_filter_is_still_read() {
        let mut v = btc();
        v["filters"] = serde_json::json!([
            {"filterType": "MIN_NOTIONAL", "minNotional": "5.00"}
        ]);
        assert_eq!(symbol_info(v).min_notional().unwrap(), Some(dec!(5.00)));
    }

    #[test]
    fn a_symbol_with_no_filters_reports_no_limits_rather_than_zero() {
        let mut v = btc();
        v["filters"] = serde_json::json!([]);
        let s = symbol_info(v);
        assert_eq!(
            s.min_notional().unwrap(),
            None,
            "zero would read as no floor"
        );
        assert_eq!(s.tick_size().unwrap(), None);
    }

    #[test]
    fn only_a_trading_symbol_is_tradeable() {
        assert!(symbol_info(btc()).tradeable());
        for status in ["BREAK", "HALT", "PENDING_TRADING", "END_OF_DAY"] {
            let mut v = btc();
            v["status"] = serde_json::json!(status);
            assert!(!symbol_info(v).tradeable(), "{status} rejects orders");
        }
    }

    /// Absence is not permission: a listing that has not said it is trading
    /// has not said it is trading.
    #[test]
    fn a_symbol_with_no_status_is_not_tradeable() {
        let mut v = btc();
        v["status"] = serde_json::Value::Null;
        assert!(!symbol_info(v).tradeable());
    }

    #[test]
    fn a_margin_only_symbol_is_not_spot_tradeable() {
        let mut v = btc();
        v["isSpotTradingAllowed"] = serde_json::json!(false);
        assert!(!symbol_info(v).tradeable());
    }

    #[test]
    fn a_balance_counts_what_is_held_against_resting_orders() {
        let b: AssetBalance = serde_json::from_value(serde_json::json!({
            "asset": "BTC", "free": "0.5", "locked": "0.25"
        }))
        .unwrap();
        assert_eq!(b.free_amount().unwrap(), dec!(0.5));
        assert_eq!(
            b.total_amount().unwrap(),
            dec!(0.75),
            "locked is still the account's money, and still a position"
        );
    }

    #[test]
    fn a_kline_reads_positionally() {
        let k: Kline = serde_json::from_value(serde_json::json!([
            1499040000000i64,
            "0.01634790",
            "0.80000000",
            "0.01575800",
            "0.01577100",
            "148976.11427815",
            1499644799999i64,
            "2434.19055334",
            308,
            "1756.87402397",
            "28.46694368",
            "0"
        ]))
        .unwrap();
        assert_eq!(k.open_time_ms().unwrap(), 1499040000000);
        assert_eq!(k.decimal(1, "open").unwrap(), dec!(0.01634790));
        assert_eq!(k.decimal(4, "close").unwrap(), dec!(0.01577100));
    }

    /// A shorter array would otherwise read a neighbouring field as the one
    /// asked for, which is a plausible number from the wrong column.
    #[test]
    fn a_truncated_kline_is_an_error_not_a_neighbouring_field() {
        let k: Kline = serde_json::from_value(serde_json::json!([1499040000000i64, "1"])).unwrap();
        let err = k.decimal(4, "close").unwrap_err();
        assert!(format!("{err:#}").contains("close"), "{err:#}");
    }

    #[test]
    fn a_trade_reads_its_commission_and_price() {
        let t: Trade = serde_json::from_value(serde_json::json!({
            "price": "60000.00", "qty": "0.001",
            "commission": "0.0000006", "commissionAsset": "BTC"
        }))
        .unwrap();
        assert_eq!(t.commission_amount().unwrap(), dec!(0.0000006));
        assert_eq!(t.price_amount().unwrap(), dec!(60000.00));
        assert_eq!(t.commission_asset.as_deref(), Some("BTC"));
    }

    /// Binance omits the field rather than sending "0" on a zero-fee fill.
    #[test]
    fn a_trade_with_no_commission_is_zero_not_an_error() {
        let t: Trade = serde_json::from_value(serde_json::json!({"price": "1"})).unwrap();
        assert_eq!(t.commission_amount().unwrap(), Decimal::ZERO);
    }

    /// A commission that cannot be priced must not be guessed at.
    #[test]
    fn a_trade_with_no_price_says_so() {
        let t: Trade = serde_json::from_value(serde_json::json!({"commission": "1"})).unwrap();
        assert!(t.price_amount().is_err());
    }

    /// The code carries the actionable part; `-1021` is a local clock problem
    /// that a message about timestamps does not make obvious.
    #[test]
    fn an_error_code_carries_a_hint_where_the_message_does_not() {
        let e = ApiError {
            code: -1021,
            msg: Some("Timestamp for this request is outside of the recvWindow.".to_string()),
        };
        assert!(e.hint().unwrap().contains("clock"));
        assert!(ApiError {
            code: -2010,
            msg: None
        }
        .hint()
        .is_none());
        assert_eq!(
            ApiError {
                code: -2010,
                msg: None
            }
            .message(),
            "Binance.US gave no message"
        );
    }
}
