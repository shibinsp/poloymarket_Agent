//! Fair value estimation pipeline.
//!
//! Constructs prompts from market data + external data points,
//! sends to Claude, and parses the structured JSON response.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{info, instrument, warn};

use crate::config::ValuationConfig;
use crate::data::DataPoint;
use crate::market::models::{MarketCandidate, OrderBookSnapshot};
use crate::valuation::claude::ClaudeClient;

/// Claude's structured valuation response.
#[derive(Debug, Clone)]
pub struct ValuationResult {
    pub probability: Decimal,
    pub confidence: Decimal,
    pub reasoning_summary: String,
    pub key_factors: Vec<String>,
    pub data_quality: DataQuality,
    pub time_sensitivity: TimeSensitivity,
}

/// Raw JSON form — Claude outputs floats, but we store as Decimal.
#[derive(Debug, Deserialize)]
struct RawValuationResult {
    probability: f64,
    confidence: f64,
    reasoning_summary: String,
    key_factors: Vec<String>,
    data_quality: DataQuality,
    time_sensitivity: TimeSensitivity,
}

impl From<RawValuationResult> for ValuationResult {
    fn from(raw: RawValuationResult) -> Self {
        Self {
            probability: Decimal::try_from(raw.probability).unwrap_or(Decimal::ZERO),
            confidence: Decimal::try_from(raw.confidence).unwrap_or(Decimal::ZERO),
            reasoning_summary: raw.reasoning_summary,
            key_factors: raw.key_factors,
            data_quality: raw.data_quality,
            time_sensitivity: raw.time_sensitivity,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DataQuality {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TimeSensitivity {
    Hours,
    Days,
    Weeks,
}

/// Cached valuation entry.
struct CachedValuation {
    result: ValuationResult,
    cached_at: Instant,
}

pub struct ValuationEngine {
    claude: Arc<ClaudeClient>,
    config: ValuationConfig,
    cache: Mutex<HashMap<String, CachedValuation>>,
}

impl ValuationEngine {
    pub fn new(claude: Arc<ClaudeClient>, config: ValuationConfig) -> Self {
        Self {
            claude,
            config,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Evaluate a market candidate using Claude.
    /// Returns None if bankroll is too low for API calls.
    #[instrument(skip(self, candidate, data_points), fields(market = %candidate.market.question))]
    pub async fn evaluate(
        &self,
        candidate: &MarketCandidate,
        data_points: &[DataPoint],
        bankroll: Decimal,
        cycle: i64,
    ) -> Result<Option<ValuationResult>> {
        // Cost gate: skip if bankroll too low
        if bankroll < dec!(10) {
            warn!("Bankroll too low for valuation, skipping");
            return Ok(None);
        }

        // Check cache
        let cache_key = candidate.market.condition_id.clone();
        {
            let cache = self.cache.lock().await;
            if let Some(cached) = cache.get(&cache_key) {
                let age = cached.cached_at.elapsed();
                if age.as_secs() < self.config.cache_ttl_seconds {
                    info!(
                        cache_age_s = age.as_secs(),
                        "Using cached valuation"
                    );
                    return Ok(Some(cached.result.clone()));
                }
            }
        }

        // Build prompt
        let system_prompt = build_system_prompt();
        let user_prompt = build_user_prompt(candidate, data_points);

        // Call Claude
        let response = self
            .claude
            .complete(&system_prompt, &user_prompt, Some(cycle))
            .await
            .context("Claude valuation call failed")?;

        // Parse JSON response
        let result = parse_valuation_response(&response.text)
            .context("Failed to parse Claude valuation response")?;

        // Validate probability bounds
        if result.probability < Decimal::ZERO || result.probability > Decimal::ONE {
            bail!(
                "Invalid probability from Claude: {}",
                result.probability
            );
        }
        if result.confidence < Decimal::ZERO || result.confidence > Decimal::ONE {
            bail!(
                "Invalid confidence from Claude: {}",
                result.confidence
            );
        }

        info!(
            probability = %result.probability,
            confidence = %result.confidence,
            reasoning = %result.reasoning_summary,
            "Valuation complete"
        );

        // Update cache
        {
            let mut cache = self.cache.lock().await;
            cache.insert(
                cache_key,
                CachedValuation {
                    result: result.clone(),
                    cached_at: Instant::now(),
                },
            );
        }

        Ok(Some(result))
    }

    /// Estimate the cost of the next valuation API call.
    pub fn estimated_call_cost(&self) -> Decimal {
        // Average Claude valuation call: ~2000 input tokens, ~300 output tokens
        crate::valuation::claude::calculate_cost(2000, 300)
    }
}

/// Build the system prompt for valuation.
fn build_system_prompt() -> String {
    r#"You are a prediction market analyst. Given market data and external signals,
estimate the true probability of the outcome. You must respond with ONLY
valid JSON. No explanations outside the JSON structure.

Your response MUST follow this exact schema:
{
  "probability": <float 0.0-1.0>,
  "confidence": <float 0.0-1.0>,
  "reasoning_summary": "<1-2 sentences>",
  "key_factors": ["<factor1>", "<factor2>"],
  "data_quality": "<high|medium|low>",
  "time_sensitivity": "<hours|days|weeks>"
}"#
    .to_string()
}

/// Build the user prompt from market data and external data points.
fn build_user_prompt(candidate: &MarketCandidate, data_points: &[DataPoint]) -> String {
    let market = &candidate.market;
    let book = &candidate.order_book;

    let implied_prob = book.implied_probability * dec!(100);
    let days_to_resolution = (market.end_date - Utc::now()).num_days();

    // Format external data points
    let data_section = if data_points.is_empty() {
        "No external data available.".to_string()
    } else {
        data_points
            .iter()
            .take(10) // Limit to 10 data points to control token count
            .enumerate()
            .map(|(i, dp)| {
                format!(
                    "{}. [{}] (confidence: {}) {}",
                    i + 1,
                    dp.source,
                    dp.confidence,
                    truncate_json(&dp.payload, 200)
                )
            })
            .collect::<Vec<String>>()
            .join("\n")
    };

    let depth = format_order_book_depth(book);

    format!(
        r#"Market: {question}
Current Price: {price} (implied prob: {implied_prob:.1}%)
Resolution Date: {end_date} ({days} days away)
Category: {category:?}

External Data:
{data}

Volume (24h): ${volume}
Order Book Depth: {depth}
Spread: {spread}

Estimate the TRUE probability of YES outcome."#,
        question = market.question,
        price = book.midpoint,
        implied_prob = implied_prob,
        end_date = market.end_date.format("%Y-%m-%d"),
        days = days_to_resolution,
        category = market.category,
        data = data_section,
        volume = market.volume_24h,
        depth = depth,
        spread = book.spread,
    )
}

/// Parse Claude's JSON response into a ValuationResult.
fn parse_valuation_response(text: &str) -> Result<ValuationResult> {
    // Try to extract JSON from the response (Claude might wrap it in markdown code blocks)
    let json_str = extract_json(text);

    let raw: RawValuationResult = serde_json::from_str(&json_str)
        .with_context(|| format!("Failed to parse valuation JSON: {json_str}"))?;

    Ok(raw.into())
}

/// Extract JSON from text that might contain markdown code blocks.
fn extract_json(text: &str) -> String {
    // Try to find JSON in code blocks first
    if let Some(start) = text.find("```json") {
        let json_start = start + 7;
        if let Some(end) = text[json_start..].find("```") {
            return text[json_start..json_start + end].trim().to_string();
        }
    }
    if let Some(start) = text.find("```") {
        let json_start = start + 3;
        if let Some(end) = text[json_start..].find("```") {
            return text[json_start..json_start + end].trim().to_string();
        }
    }

    // Try to find raw JSON object
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            return text[start..=end].to_string();
        }
    }

    text.trim().to_string()
}

/// Truncate a JSON value to a maximum string length.
fn truncate_json(value: &serde_json::Value, max_len: usize) -> String {
    let s = value.to_string();
    if s.len() > max_len {
        format!("{}...", &s[..max_len])
    } else {
        s
    }
}

/// Format order book depth as a human-readable string.
fn format_order_book_depth(book: &OrderBookSnapshot) -> String {
    let bid_depth: Decimal = book.bids.iter().map(|b| b.size).sum();
    let ask_depth: Decimal = book.asks.iter().map(|a| a.size).sum();
    format!("bids: ${bid_depth}, asks: ${ask_depth}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valuation_response_clean_json() {
        let json = r#"{
            "probability": 0.72,
            "confidence": 0.85,
            "reasoning_summary": "Based on current data, the probability is high.",
            "key_factors": ["factor1", "factor2"],
            "data_quality": "high",
            "time_sensitivity": "days"
        }"#;

        let result = parse_valuation_response(json).unwrap();
        assert_eq!(result.probability, dec!(0.72));
        assert_eq!(result.confidence, dec!(0.85));
        assert_eq!(result.data_quality, DataQuality::High);
        assert_eq!(result.time_sensitivity, TimeSensitivity::Days);
        assert_eq!(result.key_factors.len(), 2);
    }

    #[test]
    fn test_parse_valuation_response_code_block() {
        let text = r#"Here is my analysis:
```json
{
    "probability": 0.55,
    "confidence": 0.60,
    "reasoning_summary": "Test reasoning.",
    "key_factors": ["a"],
    "data_quality": "medium",
    "time_sensitivity": "hours"
}
```"#;

        let result = parse_valuation_response(text).unwrap();
        assert_eq!(result.probability, dec!(0.55));
        assert_eq!(result.confidence, dec!(0.60));
    }

    #[test]
    fn test_extract_json_raw() {
        let text = r#"some text {"key": "value"} more text"#;
        assert_eq!(extract_json(text), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_extract_json_code_block() {
        let text = "```json\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json(text), "{\"key\": \"value\"}");
    }

    #[test]
    fn test_truncate_json() {
        let value = serde_json::json!({"long_key": "a".repeat(300)});
        let truncated = truncate_json(&value, 50);
        assert!(truncated.len() <= 53); // 50 + "..."
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn test_format_order_book_depth() {
        use crate::market::models::PriceLevel;

        let book = OrderBookSnapshot {
            token_id: "test".to_string(),
            bids: vec![
                PriceLevel { price: dec!(0.60), size: dec!(100) },
                PriceLevel { price: dec!(0.59), size: dec!(200) },
            ],
            asks: vec![
                PriceLevel { price: dec!(0.65), size: dec!(150) },
            ],
            spread: dec!(0.05),
            midpoint: dec!(0.625),
            implied_probability: dec!(0.625),
            timestamp: Utc::now(),
        };

        let depth = format_order_book_depth(&book);
        assert_eq!(depth, "bids: $300, asks: $150");
    }
}
