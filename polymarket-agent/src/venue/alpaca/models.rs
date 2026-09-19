//! Alpaca wire types and the mappings into the venue-agnostic domain.
//!
//! Everything monetary crosses this boundary as a `Decimal`. Alpaca is
//! inconsistent about how it encodes numbers — the trading API (`/v2/account`,
//! `/v2/orders`, `/v2/positions`) sends decimal *strings*, while the market
//! data API (`/v2/stocks/*`, `/v1beta3/crypto/*`) sends JSON *numbers*. Both
//! shapes go through [`value_to_decimal`], which never calls
//! `Decimal::from_f64` and never performs floating point arithmetic.

use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::venue::types::{OrderState, Side, TimeInForce};

// Decimal decoding lives in `crate::json_decimal`: every API- and
// model-supplied number in the codebase has to avoid `Decimal::from_f64`, not
// just Alpaca's, so the decoder is shared rather than owned here.
use crate::json_decimal::{de_decimal, de_decimal_opt};

// === Symbol helpers =======================================================

/// Alpaca spells crypto pairs `BASE/QUOTE` and equities as a bare ticker, so
/// the slash is the only discriminator the API gives us.
pub fn is_crypto_symbol(symbol: &str) -> bool {
    symbol.contains('/')
}

/// Base and quote currency of a crypto pair, e.g. `("BTC", "USD")`.
pub fn split_pair(symbol: &str) -> Option<(&str, &str)> {
    symbol.split_once('/')
}

// === Trading API responses ================================================

#[derive(Debug, Clone, Deserialize)]
pub struct AlpacaAccount {
    pub id: String,
    pub currency: String,
    /// `ACTIVE` for a usable account; `ACCOUNT_CLOSED`, `ONBOARDING`, … otherwise.
    pub status: String,
    #[serde(deserialize_with = "de_decimal")]
    pub cash: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub equity: Decimal,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub buying_power: Option<Decimal>,
    /// Cash usable for non-marginable buys (crypto, fractional shares). This is
    /// the honest "free to deploy" figure; `buying_power` includes margin.
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub non_marginable_buying_power: Option<Decimal>,
    #[serde(default)]
    pub trading_blocked: bool,
    #[serde(default)]
    pub account_blocked: bool,
    #[serde(default)]
    pub trade_suspended_by_user: bool,
    #[serde(default)]
    pub transfers_blocked: bool,
}

impl AlpacaAccount {
    /// Why this account cannot trade, or `None` when it can.
    pub fn blocked_reason(&self) -> Option<String> {
        if self.account_blocked {
            return Some("account_blocked is set".to_string());
        }
        if self.trading_blocked {
            return Some("trading_blocked is set".to_string());
        }
        if self.trade_suspended_by_user {
            return Some("trade_suspended_by_user is set".to_string());
        }
        if !self.status.eq_ignore_ascii_case("ACTIVE") {
            return Some(format!("account status is {}", self.status));
        }
        None
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlpacaClock {
    pub timestamp: DateTime<Utc>,
    pub is_open: bool,
    pub next_open: DateTime<Utc>,
    pub next_close: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlpacaAsset {
    pub symbol: String,
    /// `us_equity` or `crypto`.
    pub class: String,
    #[serde(default)]
    pub exchange: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub tradable: bool,
    #[serde(default)]
    pub fractionable: bool,
    /// Crypto only: smallest quantity the venue will accept.
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub min_order_size: Option<Decimal>,
    /// Crypto only: quantity increment.
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub min_trade_increment: Option<Decimal>,
    /// Crypto only: price increment.
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub price_increment: Option<Decimal>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlpacaOrder {
    pub id: String,
    #[serde(default)]
    pub client_order_id: String,
    pub symbol: String,
    pub status: String,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub qty: Option<Decimal>,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub filled_qty: Option<Decimal>,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub filled_avg_price: Option<Decimal>,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub limit_price: Option<Decimal>,
    #[serde(default)]
    pub side: String,
    #[serde(default, rename = "type")]
    pub order_type: String,
    #[serde(default)]
    pub time_in_force: String,
    #[serde(default)]
    pub extended_hours: bool,
    /// Pagination cursor for `/v2/orders`, which pages by submission time
    /// rather than by offset.
    #[serde(default)]
    pub submitted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlpacaPosition {
    pub symbol: String,
    #[serde(default)]
    pub asset_class: String,
    /// Negative for a short position.
    #[serde(deserialize_with = "de_decimal")]
    pub qty: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub avg_entry_price: Decimal,
}

/// One entry of the 207 Multi-Status body returned by `DELETE /v2/orders`.
#[derive(Debug, Clone, Deserialize)]
pub struct CancelAllEntry {
    #[serde(default)]
    pub id: String,
    pub status: u16,
}

// === Market data responses ================================================

#[derive(Debug, Clone, Deserialize)]
pub struct LatestQuotesResponse {
    #[serde(default)]
    pub quotes: std::collections::HashMap<String, EquityQuote>,
}

/// Alpaca's compact quote encoding: `bp`/`ap` are the prices, `bs`/`as` the
/// sizes. The sizes are deliberately *not* surfaced as depth — see the note on
/// `AlpacaVenue::quote` for why.
#[derive(Debug, Clone, Deserialize)]
pub struct EquityQuote {
    #[serde(rename = "t")]
    pub ts: DateTime<Utc>,
    #[serde(rename = "bp", deserialize_with = "de_decimal")]
    pub bid_price: Decimal,
    #[serde(rename = "ap", deserialize_with = "de_decimal")]
    pub ask_price: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CryptoOrderbooksResponse {
    #[serde(default)]
    pub orderbooks: std::collections::HashMap<String, CryptoOrderbook>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CryptoOrderbook {
    #[serde(rename = "t")]
    pub ts: DateTime<Utc>,
    #[serde(rename = "b", default)]
    pub bids: Vec<BookEntry>,
    #[serde(rename = "a", default)]
    pub asks: Vec<BookEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BookEntry {
    #[serde(rename = "p", deserialize_with = "de_decimal")]
    pub price: Decimal,
    #[serde(rename = "s", deserialize_with = "de_decimal")]
    pub size: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BarsResponse {
    #[serde(default)]
    pub bars: std::collections::HashMap<String, Vec<AlpacaBar>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlpacaBar {
    #[serde(rename = "t")]
    pub ts: DateTime<Utc>,
    #[serde(rename = "o", deserialize_with = "de_decimal")]
    pub open: Decimal,
    #[serde(rename = "h", deserialize_with = "de_decimal")]
    pub high: Decimal,
    #[serde(rename = "l", deserialize_with = "de_decimal")]
    pub low: Decimal,
    #[serde(rename = "c", deserialize_with = "de_decimal")]
    pub close: Decimal,
    #[serde(rename = "v", default, deserialize_with = "de_decimal_opt")]
    pub volume: Option<Decimal>,
}

// === Order submission =====================================================

/// The `POST /v2/orders` body.
///
/// Quantities and prices are sent as strings so a `Decimal` reaches Alpaca
/// with its exact scale; serialising them as JSON numbers would round-trip
/// through a float on the way in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NewOrder {
    pub symbol: String,
    pub qty: String,
    pub side: &'static str,
    #[serde(rename = "type")]
    pub order_type: &'static str,
    pub time_in_force: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<String>,
    pub extended_hours: bool,
    /// The whole point of the idempotency story: a retry after a timeout
    /// carries the same id and Alpaca rejects the duplicate instead of
    /// placing a second order.
    pub client_order_id: String,
}

pub fn side_str(side: Side) -> &'static str {
    match side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

/// Alpaca's `time_in_force` vocabulary. `Gtd` has no Alpaca equivalent, so it
/// returns `None` rather than being silently downgraded to `gtc` — an order
/// that outlives its intended expiry is a risk event, not a rounding detail.
pub fn tif_str(tif: TimeInForce) -> Option<&'static str> {
    match tif {
        TimeInForce::Gtc => Some("gtc"),
        TimeInForce::Ioc => Some("ioc"),
        TimeInForce::Day => Some("day"),
        TimeInForce::Gtd(_) => None,
    }
}

// === Status mapping =======================================================

/// Map an Alpaca order status onto [`OrderState`].
///
/// Where a status is ambiguous the mapping is deliberately conservative — it
/// prefers "still open" over "finished", because treating a live order as
/// terminal is what produces double placements:
///
/// * `pending_cancel` — a cancel has been requested but the order can still
///   fill, so it stays `Accepted`.
/// * `done_for_day` — no further activity *today*, but a GTC order resumes
///   tomorrow, so it stays `Accepted` rather than becoming `Expired`.
/// * `replaced` — the original will never fill again; the replacement is a
///   separate order, so this is `Cancelled`.
/// * anything unrecognised — [`OrderState::Unknown`], which is neither open
///   nor terminal and therefore forces the caller to re-query before acting.
///   A new Alpaca status must never be silently absorbed into a wrong state.
pub fn order_state(status: &str, reject_reason: Option<&str>) -> OrderState {
    match status {
        "new"
        | "accepted"
        | "pending_new"
        | "accepted_for_bidding"
        | "calculated"
        | "held"
        | "pending_replace"
        | "pending_cancel"
        | "pending_review"
        | "stopped"
        | "suspended"
        | "done_for_day" => OrderState::Accepted,
        "partially_filled" => OrderState::PartiallyFilled,
        "filled" => OrderState::Filled,
        "canceled" | "replaced" => OrderState::Cancelled,
        "expired" => OrderState::Expired,
        "rejected" => OrderState::Rejected(
            reject_reason
                .unwrap_or("Alpaca reported status=rejected")
                .to_string(),
        ),
        other => {
            warn!(
                status = other,
                "Unrecognised Alpaca order status — reporting Unknown so the caller \
                 re-queries rather than assuming the order is done"
            );
            OrderState::Unknown
        }
    }
}

// === Diagnostics ==========================================================

impl fmt::Display for AlpacaOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} ({})",
            self.id, self.symbol, self.status, self.client_order_id
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_statuses_map_to_accepted() {
        for status in [
            "new",
            "accepted",
            "pending_new",
            "accepted_for_bidding",
            "calculated",
            "held",
            "pending_replace",
            "pending_review",
            "stopped",
            "suspended",
        ] {
            assert_eq!(order_state(status, None), OrderState::Accepted, "{status}");
        }
    }

    #[test]
    fn terminal_statuses_map_to_their_domain_states() {
        assert_eq!(
            order_state("partially_filled", None),
            OrderState::PartiallyFilled
        );
        assert_eq!(order_state("filled", None), OrderState::Filled);
        assert_eq!(order_state("canceled", None), OrderState::Cancelled);
        assert_eq!(order_state("replaced", None), OrderState::Cancelled);
        assert_eq!(order_state("expired", None), OrderState::Expired);
        assert_eq!(
            order_state("rejected", Some("insufficient buying power")),
            OrderState::Rejected("insufficient buying power".to_string())
        );
    }

    #[test]
    fn ambiguous_statuses_stay_open_rather_than_terminal() {
        // Treating either of these as finished is how a still-live order gets
        // re-placed.
        for status in ["pending_cancel", "done_for_day"] {
            let state = order_state(status, None);
            assert_eq!(state, OrderState::Accepted, "{status}");
            assert!(state.is_open());
            assert!(!state.is_terminal());
        }
    }

    #[test]
    fn unknown_status_is_unknown_not_a_guess() {
        // A status Alpaca adds later must not be absorbed into Accepted or
        // Filled — Unknown is neither open nor terminal, which forces a
        // re-query before any retry.
        let state = order_state("some_future_status", None);
        assert_eq!(state, OrderState::Unknown);
        assert!(!state.is_open());
        assert!(!state.is_terminal());
    }

    #[test]
    fn time_in_force_maps_and_refuses_gtd() {
        assert_eq!(tif_str(TimeInForce::Gtc), Some("gtc"));
        assert_eq!(tif_str(TimeInForce::Ioc), Some("ioc"));
        assert_eq!(tif_str(TimeInForce::Day), Some("day"));
        // Alpaca has no good-till-date; downgrading it silently would leave an
        // order alive past its intended expiry.
        assert_eq!(tif_str(TimeInForce::Gtd(Utc::now())), None);
    }

    #[test]
    fn crypto_symbols_are_detected_by_the_slash() {
        assert!(is_crypto_symbol("BTC/USD"));
        assert!(!is_crypto_symbol("AAPL"));
        assert_eq!(split_pair("ETH/USDT"), Some(("ETH", "USDT")));
        assert_eq!(split_pair("AAPL"), None);
    }

    #[test]
    fn account_blocked_reason_covers_every_gate() {
        let base = serde_json::json!({
            "id": "acct", "currency": "USD", "status": "ACTIVE",
            "cash": "100.00", "equity": "100.00"
        });
        let ok: AlpacaAccount = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(ok.blocked_reason(), None);

        for (field, needle) in [
            ("trading_blocked", "trading_blocked"),
            ("account_blocked", "account_blocked"),
            ("trade_suspended_by_user", "trade_suspended_by_user"),
        ] {
            let mut json = base.clone();
            json[field] = serde_json::Value::Bool(true);
            let acct: AlpacaAccount = serde_json::from_value(json).unwrap();
            assert!(acct.blocked_reason().unwrap().contains(needle));
        }

        let mut json = base;
        json["status"] = serde_json::Value::String("ACCOUNT_CLOSED".to_string());
        let acct: AlpacaAccount = serde_json::from_value(json).unwrap();
        assert!(acct.blocked_reason().unwrap().contains("ACCOUNT_CLOSED"));
    }
}
