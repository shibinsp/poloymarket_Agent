//! Decoding numbers from JSON without ever touching `f64`.
//!
//! `RULES.md` requires `rust_decimal::Decimal` for money. The trap is that
//! `#[derive(Deserialize)]` on a `Decimal` field is not the problem — declaring
//! the field `f64` and converting afterwards is, because by then the binary
//! rounding has already happened. Anything that arrives as JSON and ends up as
//! a price, a size or a threshold goes through here: venue responses, and model
//! output, which is JSON the same way an API response is.

use std::str::FromStr;

use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

// === Decimal decoding =====================================================

/// Parse a decimal from its textual form, tolerating scientific notation.
///
/// `Decimal::from_str` rejects exponents, which show up when a JSON number
/// like `1e-8` is re-rendered as text, so those are routed to
/// `from_scientific` instead.
pub fn parse_decimal_str(raw: &str) -> Result<Decimal, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty decimal string".to_string());
    }
    let parsed = if trimmed.contains(['e', 'E']) {
        Decimal::from_scientific(trimmed)
    } else {
        Decimal::from_str(trimmed)
    };
    parsed.map_err(|e| format!("invalid decimal {trimmed:?}: {e}"))
}

/// Convert a JSON scalar to a `Decimal`.
///
/// A JSON string is parsed directly. A JSON number is rendered back to its
/// shortest round-trip decimal text and parsed from that: `serde_json` prints
/// floats with ryū, which reproduces the literal the server sent for any value
/// carrying 15 or fewer significant digits — far more than Alpaca publishes for
/// prices or sizes. No value ever reaches `Decimal` through `from_f64`, so no
/// binary-floating-point rounding is baked into a price or a quantity.
pub fn value_to_decimal(value: &Value) -> Result<Decimal, String> {
    match value {
        Value::String(s) => parse_decimal_str(s),
        Value::Number(n) => parse_decimal_str(&n.to_string()),
        other => Err(format!(
            "expected a decimal as a JSON string or number, got {other}"
        )),
    }
}

/// serde adapter for a required decimal field.
pub fn de_decimal<'de, D>(deserializer: D) -> Result<Decimal, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    value_to_decimal(&value).map_err(serde::de::Error::custom)
}

/// serde adapter for an optional decimal field. `null`, an absent key and an
/// empty string all mean "not reported"; anything else must parse.
pub fn de_decimal_opt<'de, D>(deserializer: D) -> Result<Option<Decimal>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(v) => value_to_decimal(&v)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn decimals_parse_from_strings_and_numbers_without_float_drift() {
        // Alpaca sends order quantities as strings and bar prices as numbers;
        // both must land on the exact same Decimal.
        assert_eq!(
            value_to_decimal(&serde_json::json!("191.36")).unwrap(),
            dec!(191.36)
        );
        assert_eq!(
            value_to_decimal(&serde_json::json!(191.36)).unwrap(),
            dec!(191.36)
        );
        // A quantity with eight decimals — the crypto case that f64 mangles.
        assert_eq!(
            value_to_decimal(&serde_json::json!("0.00012345")).unwrap(),
            dec!(0.00012345)
        );
        assert_eq!(
            value_to_decimal(&serde_json::json!(0.00012345)).unwrap(),
            dec!(0.00012345)
        );
        // Integers stay integral rather than gaining a fractional tail.
        assert_eq!(
            value_to_decimal(&serde_json::json!(100)).unwrap(),
            dec!(100)
        );
    }

    #[test]
    fn decimal_parsing_rejects_nonsense_instead_of_defaulting() {
        assert!(value_to_decimal(&serde_json::json!("not-a-price")).is_err());
        assert!(value_to_decimal(&serde_json::json!(true)).is_err());
        assert!(value_to_decimal(&serde_json::json!(null)).is_err());
    }

    #[test]
    fn optional_decimals_treat_null_and_empty_as_absent() {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default, deserialize_with = "de_decimal_opt")]
            price: Option<Decimal>,
        }
        let null: Wire = serde_json::from_str(r#"{"price": null}"#).unwrap();
        assert_eq!(null.price, None);
        let empty: Wire = serde_json::from_str(r#"{"price": ""}"#).unwrap();
        assert_eq!(empty.price, None);
        let missing: Wire = serde_json::from_str("{}").unwrap();
        assert_eq!(missing.price, None);
        let present: Wire = serde_json::from_str(r#"{"price": "1.25"}"#).unwrap();
        assert_eq!(present.price, Some(dec!(1.25)));
    }
}
