//! Claude API client for fair value estimation.
//!
//! Sends structured prompts to Claude and tracks every API call cost.

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{info, instrument, warn};

use crate::db::store::{ApiCostRecord, Store};

/// Claude API pricing (per token, as of 2025 for claude-sonnet-4-20250514).
const INPUT_PRICE_PER_MILLION: Decimal = dec!(3.00);
const OUTPUT_PRICE_PER_MILLION: Decimal = dec!(15.00);
const MILLION: Decimal = dec!(1_000_000);

pub struct ClaudeClient {
    client: reqwest::Client,
    api_key: String,
    model: String,
    store: Store,
}

impl ClaudeClient {
    pub fn new(api_key: String, model: String, store: Store) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            api_key,
            model,
            store,
        }
    }

    /// Send a message to Claude and return the parsed response with cost tracking.
    #[instrument(skip(self, system_prompt, user_prompt))]
    pub async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        cycle: Option<i64>,
    ) -> Result<ClaudeResponse> {
        let request = ClaudeRequest {
            model: self.model.clone(),
            max_tokens: 1024,
            system: Some(system_prompt.to_string()),
            messages: vec![ClaudeMessage {
                role: "user".to_string(),
                content: user_prompt.to_string(),
            }],
        };

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Claude API request failed")?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            bail!("Claude API error ({}): {}", status, error_body);
        }

        let api_response: ClaudeApiResponse = response
            .json()
            .await
            .context("Failed to parse Claude API response")?;

        // Extract text content
        let text = api_response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<&str>>()
            .join("");

        // Calculate and track cost
        let input_tokens = api_response.usage.input_tokens;
        let output_tokens = api_response.usage.output_tokens;
        let cost = calculate_cost(input_tokens, output_tokens);

        info!(
            input_tokens,
            output_tokens,
            cost = %cost,
            model = %self.model,
            "Claude API call completed"
        );

        // Store cost in DB
        if let Err(e) = self.track_cost(input_tokens, output_tokens, cost, cycle).await {
            warn!(error = %e, "Failed to track API cost");
        }

        Ok(ClaudeResponse {
            text,
            input_tokens,
            output_tokens,
            cost,
        })
    }

    async fn track_cost(
        &self,
        input_tokens: i64,
        output_tokens: i64,
        cost: Decimal,
        cycle: Option<i64>,
    ) -> Result<()> {
        let record = ApiCostRecord {
            id: None,
            provider: "anthropic".to_string(),
            endpoint: Some("messages".to_string()),
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
            cost: cost.to_string(),
            cycle,
            created_at: None,
        };
        self.store.insert_api_cost(&record).await?;
        Ok(())
    }

    /// Get total API cost across all cycles.
    pub async fn total_cost(&self) -> Result<Decimal> {
        self.store.get_total_api_cost().await
    }
}

/// Calculate the dollar cost of a Claude API call.
pub fn calculate_cost(input_tokens: i64, output_tokens: i64) -> Decimal {
    let input_cost = Decimal::from(input_tokens) * INPUT_PRICE_PER_MILLION / MILLION;
    let output_cost = Decimal::from(output_tokens) * OUTPUT_PRICE_PER_MILLION / MILLION;
    input_cost + output_cost
}

// --- Request/Response Types ---

#[derive(Debug, Serialize)]
struct ClaudeRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ClaudeMessage>,
}

#[derive(Debug, Serialize)]
struct ClaudeMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ClaudeApiResponse {
    content: Vec<ContentBlock>,
    usage: Usage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct Usage {
    input_tokens: i64,
    output_tokens: i64,
}

/// Parsed response from a Claude API call.
pub struct ClaudeResponse {
    pub text: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cost_calculation() {
        // 1000 input tokens, 500 output tokens
        let cost = calculate_cost(1000, 500);
        // input: 1000 * 3.00 / 1_000_000 = 0.003
        // output: 500 * 15.00 / 1_000_000 = 0.0075
        // total: 0.0105
        assert_eq!(cost, dec!(0.0105));
    }

    #[test]
    fn test_cost_calculation_zero_tokens() {
        let cost = calculate_cost(0, 0);
        assert_eq!(cost, Decimal::ZERO);
    }

    #[test]
    fn test_cost_calculation_large_input() {
        // 100k input, 4k output (typical Claude call)
        let cost = calculate_cost(100_000, 4_000);
        // input: 100_000 * 3.00 / 1_000_000 = 0.30
        // output: 4_000 * 15.00 / 1_000_000 = 0.06
        // total: 0.36
        assert_eq!(cost, dec!(0.36));
    }
}
