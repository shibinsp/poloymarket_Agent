//! Upstox API v2 wire formats and payload definitions.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Standard response wrapper for Upstox API v2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxApiResponse<T> {
    pub status: String,
    pub data: Option<T>,
    pub errors: Option<Vec<UpstoxApiError>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxApiError {
    pub message: String,
    pub error_code: Option<String>,
    pub property_path: Option<String>,
}

/// Upstox order placement request.
/// Endpoint: `POST https://api.upstox.com/v2/order/place`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxOrderRequestPayload {
    pub quantity: i64,
    /// Product: "D" for Delivery (CNC), "I" for Intraday (MIS)
    pub product: String,
    pub validity: String, // "DAY", "IOC"
    pub price: Decimal,
    pub tag: Option<String>,
    pub instrument_token: String, // e.g. "NSE_EQ|INE002A01018"
    pub order_type: String,       // "LIMIT", "MARKET"
    pub transaction_type: String, // "BUY", "SELL"
    pub disclosed_quantity: i64,
    pub trigger_price: Decimal,
    pub is_amo: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxOrderPlaceData {
    pub order_id: String,
}

/// Upstox order status record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxOrderRecord {
    pub order_id: String,
    pub exchange: Option<String>,
    pub trading_symbol: Option<String>,
    pub instrument_token: Option<String>,
    pub status: String, // "complete", "rejected", "open", "cancelled"
    pub quantity: i64,
    pub filled_quantity: Option<i64>,
    pub price: Option<Decimal>,
    pub average_price: Option<Decimal>,
    pub order_timestamp: Option<String>,
    pub status_message: Option<String>,
    pub tag: Option<String>,
}

/// Upstox position record.
/// Endpoint: `GET https://api.upstox.com/v2/portfolio/short-term-positions`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxPositionRecord {
    pub trading_symbol: String,
    pub instrument_token: String,
    pub quantity: i64,
    pub buy_amount: Option<Decimal>,
    pub sell_amount: Option<Decimal>,
    pub buy_price: Option<Decimal>,
    pub sell_price: Option<Decimal>,
    pub last_price: Option<Decimal>,
    pub pnl: Option<Decimal>,
    pub product: String,
}

/// Upstox funds and margin response.
/// Endpoint: `GET https://api.upstox.com/v2/user/get-funds-and-margin`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxFundsData {
    pub equity: Option<UpstoxEquityMargin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxEquityMargin {
    pub available_margin: Decimal,
    pub used_margin: Decimal,
    pub payin_amount: Decimal,
    pub span_margin: Decimal,
    pub adhoc_margin: Decimal,
    pub notional_cash: Decimal,
    pub exposure_margin: Decimal,
}

/// Upstox market quote response.
/// Endpoint: `GET https://api.upstox.com/v2/market-quote/quotes?instrument_key=...`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxQuoteData {
    pub last_price: Decimal,
    pub volume: Option<i64>,
    pub ohlc: Option<UpstoxOhlc>,
    pub depth: Option<UpstoxMarketDepth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxOhlc {
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxMarketDepth {
    pub buy: Option<Vec<UpstoxDepthLevel>>,
    pub sell: Option<Vec<UpstoxDepthLevel>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstoxDepthLevel {
    pub quantity: i64,
    pub price: Decimal,
    pub orders: Option<i64>,
}
