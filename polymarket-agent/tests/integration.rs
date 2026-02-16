//! Integration tests for cross-module functionality.

use polymarket_agent::data::quality::compute_data_quality;
use polymarket_agent::market::category::infer_category;
use polymarket_agent::market::models::MarketCategory;
use polymarket_agent::risk::exit::{evaluate_exit, DEFAULT_MAX_LOSS_PCT};
use polymarket_agent::market::models::Side;
use polymarket_agent::valuation::fair_value::DataQuality;

use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

// ──────────────────────────────────────────
// Category detection tests
// ──────────────────────────────────────────

#[test]
fn category_bitcoin_is_crypto() {
    let cat = infer_category("Will Bitcoin reach $100k by end of 2026?");
    assert_eq!(cat, MarketCategory::Crypto);
}

#[test]
fn category_super_bowl_is_sports() {
    let cat = infer_category("Who will win the Super Bowl LXII?");
    assert_eq!(cat, MarketCategory::Sports);
}

#[test]
fn category_hurricane_is_weather() {
    let cat = infer_category("Will a hurricane make landfall in Florida in September?");
    assert_eq!(cat, MarketCategory::Weather);
}

#[test]
fn category_election_is_politics() {
    let cat = infer_category("Will the Republican candidate win the 2028 presidential election?");
    assert_eq!(cat, MarketCategory::Politics);
}

#[test]
fn category_unknown_falls_back() {
    let cat = infer_category("Will the price of tulips reach $50 per bulb?");
    matches!(cat, MarketCategory::Other(_));
}

// ──────────────────────────────────────────
// Data quality computation tests
// ──────────────────────────────────────────

#[test]
fn data_quality_no_data_is_low() {
    let quality = compute_data_quality(&[]);
    assert_eq!(quality, DataQuality::Low);
}

#[test]
fn data_quality_multiple_fresh_sources_is_high() {
    use polymarket_agent::data::DataPoint;
    let points = vec![
        DataPoint {
            source: "source_a".to_string(),
            category: MarketCategory::Crypto,
            timestamp: Utc::now(),
            payload: serde_json::json!({}),
            confidence: dec!(0.9),
            relevance_to: vec![],
        },
        DataPoint {
            source: "source_b".to_string(),
            category: MarketCategory::Crypto,
            timestamp: Utc::now(),
            payload: serde_json::json!({}),
            confidence: dec!(0.85),
            relevance_to: vec![],
        },
        DataPoint {
            source: "source_c".to_string(),
            category: MarketCategory::Crypto,
            timestamp: Utc::now(),
            payload: serde_json::json!({}),
            confidence: dec!(0.80),
            relevance_to: vec![],
        },
    ];
    let quality = compute_data_quality(&points);
    assert_eq!(quality, DataQuality::High);
}

// ──────────────────────────────────────────
// Exit signal / stop-loss tests
// ──────────────────────────────────────────

#[test]
fn exit_hold_when_within_tolerance() {
    let signal = evaluate_exit("mkt1", dec!(0.50), Side::Yes, dec!(0.45), DEFAULT_MAX_LOSS_PCT);
    assert!(!signal.should_exit);
}

#[test]
fn exit_triggered_on_large_loss() {
    // Bought YES at 0.70, now at 0.40 → ~-43% loss
    let signal = evaluate_exit("mkt2", dec!(0.70), Side::Yes, dec!(0.40), DEFAULT_MAX_LOSS_PCT);
    assert!(signal.should_exit);
}

#[test]
fn exit_no_side_loss() {
    // Bought NO at 0.30 (effective entry complement = 0.70)
    // Current midpoint 0.90 → complement = 0.10 → large loss vs 0.70 entry
    let signal = evaluate_exit("mkt3", dec!(0.30), Side::No, dec!(0.90), DEFAULT_MAX_LOSS_PCT);
    assert!(signal.should_exit);
}

// ──────────────────────────────────────────
// JSON extraction robustness tests
// ──────────────────────────────────────────

#[test]
fn json_extraction_from_markdown_block() {
    use polymarket_agent::valuation::fair_value::extract_json;
    let response = r#"Here is my analysis:

```json
{"probability": 0.65, "confidence": 0.8}
```

That's my assessment."#;

    let json = extract_json(response);
    assert!(json.is_some());
    let parsed: serde_json::Value = serde_json::from_str(&json.unwrap()).unwrap();
    assert_eq!(parsed["probability"], 0.65);
}

#[test]
fn json_extraction_raw_json() {
    use polymarket_agent::valuation::fair_value::extract_json;
    let response = r#"{"probability": 0.5, "confidence": 0.7}"#;
    let json = extract_json(response);
    assert!(json.is_some());
}

#[test]
fn json_extraction_handles_nested_braces_in_strings() {
    use polymarket_agent::valuation::fair_value::extract_json;
    let response = r#"{"key": "value with {braces}", "num": 1}"#;
    let json = extract_json(response);
    assert!(json.is_some());
}

#[test]
fn json_extraction_returns_none_for_invalid() {
    use polymarket_agent::valuation::fair_value::extract_json;
    let response = "Just some text with no JSON at all";
    let json = extract_json(response);
    assert!(json.is_none());
}

// ──────────────────────────────────────────
// Sanitization tests
// ──────────────────────────────────────────

#[test]
fn sanitize_strips_control_chars() {
    use polymarket_agent::valuation::fair_value::sanitize_market_question;
    let input = "Will Bitcoin\x00\x01 reach $100k?";
    let sanitized = sanitize_market_question(input);
    assert!(!sanitized.contains('\x00'));
    assert!(!sanitized.contains('\x01'));
    assert!(sanitized.contains("Bitcoin"));
}

#[test]
fn sanitize_limits_length() {
    use polymarket_agent::valuation::fair_value::sanitize_market_question;
    let long_input = "A".repeat(1000);
    let sanitized = sanitize_market_question(&long_input);
    assert!(sanitized.len() <= 500);
}
