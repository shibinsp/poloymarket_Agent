//! Coinbase Advanced Trade wire types.
//!
//! Every numeric field arrives as a **string** — Coinbase quotes prices and
//! sizes as decimal text, which is the one thing about this API that suits a
//! `Decimal` codebase. They are parsed at the edge rather than carried as
//! strings, so a malformed price fails where it can be attributed.

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

/// Parse a Coinbase decimal string, naming the field.
///
/// Coinbase omits fields rather than sending nulls in places the docs imply
/// are always present, so the caller decides what absence means.
pub fn money(value: &str, field: &str) -> Result<Decimal> {
    Decimal::from_str(value.trim())
        .with_context(|| format!("Coinbase sent an unparseable {field}: {value:?}"))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProductsResponse {
    #[serde(default)]
    pub products: Vec<Product>,
    /// Total products matching the query, which is larger than `products.len()`
    /// whenever the answer was paged. Coinbase pages `/products` by `offset`.
    #[serde(default)]
    pub num_products: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Product {
    pub product_id: String,
    #[serde(default)]
    pub base_currency_id: Option<String>,
    #[serde(default)]
    pub quote_currency_id: Option<String>,
    /// Price increment, as a decimal string ("0.01").
    #[serde(default)]
    pub quote_increment: Option<String>,
    /// Size increment ("0.00000001").
    #[serde(default)]
    pub base_increment: Option<String>,
    /// Smallest order *value* Coinbase accepts for this product.
    #[serde(default)]
    pub quote_min_size: Option<String>,
    /// Smallest order *size*.
    #[serde(default)]
    pub base_min_size: Option<String>,
    /// Coinbase's own flags. `trading_disabled` and friends default to false
    /// when absent, which is the permissive direction — but `list_instruments`
    /// requires an explicit `status` of "online" as well, so a product with
    /// no flags at all still has to say it is online to be traded.
    #[serde(default)]
    pub trading_disabled: bool,
    #[serde(default)]
    pub is_disabled: bool,
    #[serde(default)]
    pub view_only: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    /// "SPOT" or "FUTURE". Only spot is traded here.
    #[serde(default)]
    pub product_type: Option<String>,
}

impl Product {
    /// Whether this product can actually be traded right now.
    pub fn tradeable(&self) -> bool {
        !self.trading_disabled
            && !self.is_disabled
            && !self.view_only
            && self
                .status
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("online"))
            && self
                .product_type
                .as_deref()
                .map_or(true, |t| t.eq_ignore_ascii_case("SPOT"))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProductBookResponse {
    pub pricebook: PriceBook,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceBook {
    pub product_id: String,
    #[serde(default)]
    pub bids: Vec<BookLevel>,
    #[serde(default)]
    pub asks: Vec<BookLevel>,
    #[serde(default)]
    pub time: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BookLevel {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CandlesResponse {
    #[serde(default)]
    pub candles: Vec<CandleRow>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CandleRow {
    /// Unix seconds, as a string.
    pub start: String,
    pub low: String,
    pub high: String,
    pub open: String,
    pub close: String,
    pub volume: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountsResponse {
    #[serde(default)]
    pub accounts: Vec<Account>,
    /// Coinbase creates one account row per supported currency, so real
    /// accounts run well past a single page. Reading only the first page
    /// silently drops both cash and holdings.
    #[serde(default)]
    pub has_next: bool,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub available_balance: Option<Amount>,
    #[serde(default)]
    pub hold: Option<Amount>,
    /// Present on some account types; absent on others.
    #[serde(default)]
    pub active: Option<bool>,
}

impl Account {
    /// Spendable now: what an order can be funded from.
    pub fn available(&self) -> Result<Decimal> {
        amount_or_zero(self.available_balance.as_ref(), "available_balance")
    }

    /// Everything the account holds in this currency, spendable or not.
    ///
    /// `hold` is money committed to a resting order. It is still the
    /// account's — omitting it from a *position* makes the holding appear to
    /// shrink the moment an exit order rests, which reads as drift and halts
    /// the agent; omitting it from *equity* books an unrealised loss of the
    /// whole order the instant it is placed.
    pub fn total(&self) -> Result<Decimal> {
        Ok(self.available()? + amount_or_zero(self.hold.as_ref(), "hold")?)
    }
}

fn amount_or_zero(amount: Option<&Amount>, field: &str) -> Result<Decimal> {
    match amount {
        Some(a) => money(&a.value, field),
        None => Ok(Decimal::ZERO),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Amount {
    pub value: String,
    #[serde(default)]
    pub currency: Option<String>,
}

/// `POST /orders` wraps its result twice: success in `success_response`,
/// rejection in `error_response`, with a boolean beside them.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateOrderResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub success_response: Option<CreateOrderSuccess>,
    #[serde(default)]
    pub error_response: Option<CreateOrderError>,
    /// Present on some responses and not others; the id also appears inside
    /// `success_response`.
    #[serde(default)]
    pub order_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateOrderSuccess {
    pub order_id: String,
    #[serde(default)]
    pub client_order_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateOrderError {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub error_details: Option<String>,
    #[serde(default)]
    pub preview_failure_reason: Option<String>,
}

impl CreateOrderError {
    /// The most specific reason Coinbase gave.
    ///
    /// It populates a different one of these depending on where the order was
    /// refused — `message` for a business rule, `preview_failure_reason` for
    /// a pre-trade check — so taking only the first would frequently record a
    /// rejection with no reason attached.
    pub fn reason(&self) -> String {
        [
            self.message.as_deref(),
            self.error_details.as_deref(),
            self.preview_failure_reason.as_deref(),
            self.error.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|s| !s.trim().is_empty())
        .unwrap_or("Coinbase rejected the order without a reason")
        .to_string()
    }
}

/// `POST /orders/batch_cancel` answers HTTP 200 with a per-order result.
///
/// The envelope succeeding says nothing about whether anything was cancelled,
/// so an adapter that reads only the status code reports a still-resting order
/// as cancelled — and the kill switch then reports a book it never closed.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchCancelResponse {
    #[serde(default)]
    pub results: Vec<CancelResult>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CancelResult {
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub failure_reason: Option<String>,
}

impl CancelResult {
    /// Whether a refusal still means nothing is left resting.
    ///
    /// Coinbase refuses a cancel for an order that is already gone — filled,
    /// already cancelled, or never existed. That is the outcome the caller
    /// wanted, so treating it as a failure would have the reconciler retry
    /// forever against an order that cannot be cancelled because it is done.
    pub fn is_already_resolved(&self) -> bool {
        self.failure_reason.as_deref().is_some_and(|r| {
            let r = r.to_uppercase();
            r.contains("DUPLICATE_CANCEL_REQUEST") || r.contains("INVALID_CANCEL_REQUEST")
        })
    }

    pub fn reason(&self) -> &str {
        self.failure_reason
            .as_deref()
            .filter(|r| !r.trim().is_empty())
            .unwrap_or("Coinbase refused the cancel without a reason")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderResponse {
    pub order: Order,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrdersResponse {
    #[serde(default)]
    pub orders: Vec<Order>,
    #[serde(default)]
    pub has_next: bool,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Order {
    pub order_id: String,
    #[serde(default)]
    pub client_order_id: Option<String>,
    #[serde(default)]
    pub product_id: Option<String>,
    #[serde(default)]
    pub side: Option<String>,
    /// OPEN | FILLED | CANCELLED | EXPIRED | FAILED | PENDING | QUEUED …
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub filled_size: Option<String>,
    #[serde(default)]
    pub average_filled_price: Option<String>,
    #[serde(default)]
    pub total_fees: Option<String>,
    #[serde(default)]
    pub reject_reason: Option<String>,
    #[serde(default)]
    pub reject_message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn a_price_string_parses_to_a_decimal() {
        assert_eq!(money("60000.12", "price").unwrap(), dec!(60000.12));
        assert_eq!(money(" 0.00000001 ", "size").unwrap(), dec!(0.00000001));
    }

    #[test]
    fn an_unparseable_price_names_the_field() {
        let err = money("n/a", "quote_increment").unwrap_err();
        assert!(format!("{err:#}").contains("quote_increment"), "{err:#}");
    }

    fn product(status: Option<&str>) -> Product {
        Product {
            product_id: "BTC-USD".to_string(),
            base_currency_id: Some("BTC".to_string()),
            quote_currency_id: Some("USD".to_string()),
            quote_increment: Some("0.01".to_string()),
            base_increment: Some("0.00000001".to_string()),
            quote_min_size: Some("1".to_string()),
            base_min_size: Some("0.000016".to_string()),
            trading_disabled: false,
            is_disabled: false,
            view_only: false,
            status: status.map(str::to_string),
            display_name: Some("BTC-USD".to_string()),
            product_type: Some("SPOT".to_string()),
        }
    }

    #[test]
    fn an_online_spot_product_is_tradeable() {
        assert!(product(Some("online")).tradeable());
        assert!(product(Some("ONLINE")).tradeable(), "status case varies");
    }

    /// Each of these flags means the venue will reject the order. Discovering
    /// that at submission wastes a valuation and blocks the symbol until
    /// reconciliation resolves it.
    #[test]
    fn every_disabled_flag_makes_a_product_untradeable() {
        let mut p = product(Some("online"));
        p.trading_disabled = true;
        assert!(!p.tradeable(), "trading_disabled");

        let mut p = product(Some("online"));
        p.is_disabled = true;
        assert!(!p.tradeable(), "is_disabled");

        let mut p = product(Some("online"));
        p.view_only = true;
        assert!(!p.tradeable(), "view_only");
    }

    /// Absence is not permission. A product with no status has not said it is
    /// online, and Coinbase omits the field on delisted pairs.
    #[test]
    fn a_product_with_no_status_is_not_tradeable() {
        assert!(!product(None).tradeable());
        assert!(!product(Some("offline")).tradeable());
    }

    #[test]
    fn futures_are_not_traded_here() {
        let mut p = product(Some("online"));
        p.product_type = Some("FUTURE".to_string());
        assert!(!p.tradeable());
    }

    /// Money committed to a resting order is still the account's. Omitting it
    /// makes a position appear to shrink the moment an exit rests — which the
    /// reconciler reads as drift and halts on.
    #[test]
    fn an_accounts_total_counts_what_is_held_against_resting_orders() {
        let account: Account = serde_json::from_value(serde_json::json!({
            "currency": "BTC",
            "available_balance": {"value": "0.004"},
            "hold": {"value": "0.006"}
        }))
        .unwrap();
        assert_eq!(account.available().unwrap(), dec!(0.004));
        assert_eq!(account.total().unwrap(), dec!(0.01));
    }

    #[test]
    fn an_absent_hold_is_zero_rather_than_an_error() {
        let account: Account = serde_json::from_value(serde_json::json!({
            "currency": "USD", "available_balance": {"value": "100"}
        }))
        .unwrap();
        assert_eq!(account.total().unwrap(), dec!(100));
    }

    /// Coinbase populates a different field depending on where the order was
    /// refused. Taking only the first would record most rejections blank.
    #[test]
    fn a_rejection_reason_falls_through_to_whichever_field_is_set() {
        let only_preview = CreateOrderError {
            error: None,
            message: None,
            error_details: None,
            preview_failure_reason: Some("PREVIEW_INSUFFICIENT_FUND".to_string()),
        };
        assert_eq!(only_preview.reason(), "PREVIEW_INSUFFICIENT_FUND");

        let prefers_message = CreateOrderError {
            error: Some("INVALID_REQUEST".to_string()),
            message: Some("size too small".to_string()),
            error_details: None,
            preview_failure_reason: None,
        };
        assert_eq!(prefers_message.reason(), "size too small");
    }

    fn cancel(success: bool, reason: Option<&str>) -> CancelResult {
        CancelResult {
            order_id: Some("cb-1".to_string()),
            success,
            failure_reason: reason.map(str::to_string),
        }
    }

    /// An order that is already gone cannot be cancelled and does not need to
    /// be — that is the outcome the caller wanted. Treating it as a failure
    /// would have the reconciler retry forever against a finished order.
    #[test]
    fn a_refusal_for_an_order_that_is_already_gone_is_not_a_failure() {
        assert!(cancel(false, Some("DUPLICATE_CANCEL_REQUEST")).is_already_resolved());
        assert!(cancel(false, Some("INVALID_CANCEL_REQUEST")).is_already_resolved());
    }

    /// Everything else leaves an order resting. Reading it as resolved is how
    /// the kill switch reports a book it never closed.
    #[test]
    fn any_other_refusal_leaves_an_order_resting() {
        assert!(!cancel(false, Some("UNKNOWN_CANCEL_FAILURE_REASON")).is_already_resolved());
        assert!(!cancel(false, Some("COMMANDER_REJECTED_CANCEL_ORDER")).is_already_resolved());
        assert!(!cancel(false, None).is_already_resolved());
        assert_eq!(
            cancel(false, None).reason(),
            "Coinbase refused the cancel without a reason"
        );
    }

    #[test]
    fn a_rejection_with_nothing_set_still_says_something() {
        let blank = CreateOrderError {
            error: Some("   ".to_string()),
            message: None,
            error_details: None,
            preview_failure_reason: None,
        };
        assert!(blank.reason().contains("without a reason"));
    }
}
