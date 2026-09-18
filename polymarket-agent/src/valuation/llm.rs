//! LLM client for fair value estimation.
//!
//! Supports two wire formats behind one interface:
//! - **Anthropic** (`/v1/messages`, `x-api-key`, system as its own field)
//! - **OpenAI-compatible** (`/v1/chat/completions`, `Authorization: Bearer`,
//!   system as the first message) — covers NVIDIA NIM, vLLM, OpenRouter,
//!   Together, and anything else speaking the chat-completions schema.
//!
//! Every call's token usage and cost is tracked in the database. Pricing is
//! configured per provider rather than hardcoded, because a self-hosted or
//! free-tier endpoint costs nothing while Claude does not.

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{info, instrument, warn};

use crate::config::{LlmProvider, ValuationConfig};
use crate::db::store::{ApiCostRecord, Store};

/// Default Claude Sonnet pricing, used when the config doesn't override it.
pub const ANTHROPIC_DEFAULT_INPUT_PRICE: Decimal = dec!(3.00);
pub const ANTHROPIC_DEFAULT_OUTPUT_PRICE: Decimal = dec!(15.00);
const MILLION: Decimal = dec!(1_000_000);

const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";
const MAX_TOKENS: u32 = 1024;

/// Typical valuation call shape, used to estimate spend before making it.
pub const TYPICAL_INPUT_TOKENS: i64 = 2000;
pub const TYPICAL_OUTPUT_TOKENS: i64 = 300;

pub struct LlmClient {
    client: reqwest::Client,
    api_key: String,
    model: String,
    provider: LlmProvider,
    base_url: String,
    input_price_per_million: Decimal,
    output_price_per_million: Decimal,
    store: Store,
}

/// Hand-written so the API key can never reach a log line or panic message.
impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClient")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl LlmClient {
    /// Build a client for the configured provider.
    ///
    /// Fails fast if an OpenAI-compatible provider is selected without a
    /// `base_url` — there is no sensible default endpoint for "some
    /// OpenAI-compatible service".
    pub fn new(api_key: String, config: &ValuationConfig, store: Store) -> Result<Self> {
        let base_url = match (config.provider, config.base_url.as_deref()) {
            (_, Some(url)) => url.trim_end_matches('/').to_string(),
            (LlmProvider::Anthropic, None) => ANTHROPIC_DEFAULT_BASE_URL.to_string(),
            (LlmProvider::OpenAiCompatible, None) => bail!(
                "valuation.base_url is required when valuation.provider = \"openai_compatible\" \
                 (e.g. https://integrate.api.nvidia.com/v1 for NVIDIA NIM)"
            ),
        };

        let (input_price_per_million, output_price_per_million) = config.effective_pricing();

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("Failed to build HTTP client")?;

        info!(
            provider = ?config.provider,
            model = %config.model,
            base_url = %base_url,
            input_price_per_million = %input_price_per_million,
            output_price_per_million = %output_price_per_million,
            "Valuation LLM configured"
        );

        Ok(Self {
            client,
            api_key,
            model: config.model.clone(),
            provider: config.provider,
            base_url,
            input_price_per_million,
            output_price_per_million,
            store,
        })
    }

    /// Send a prompt to the model and return the text plus tracked cost.
    #[instrument(skip(self, system_prompt, user_prompt))]
    pub async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        cycle: Option<i64>,
    ) -> Result<LlmResponse> {
        let (text, input_tokens, output_tokens) = match self.provider {
            LlmProvider::Anthropic => self.complete_anthropic(system_prompt, user_prompt).await?,
            LlmProvider::OpenAiCompatible => {
                self.complete_openai(system_prompt, user_prompt).await?
            }
        };

        let cost = self.cost(input_tokens, output_tokens);

        info!(
            provider = ?self.provider,
            input_tokens,
            output_tokens,
            cost = %cost,
            model = %self.model,
            "LLM call completed"
        );

        if let Err(e) = self
            .track_cost(input_tokens, output_tokens, cost, cycle)
            .await
        {
            warn!(error = %e, "Failed to track API cost");
        }

        Ok(LlmResponse {
            text,
            input_tokens,
            output_tokens,
            cost,
        })
    }

    /// Returns (text, input_tokens, output_tokens).
    async fn complete_anthropic(&self, system: &str, user: &str) -> Result<(String, i64, i64)> {
        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens: MAX_TOKENS,
            system: Some(system.to_string()),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: user.to_string(),
            }],
        };

        let response = self
            .client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Anthropic API request failed")?;

        let body = Self::read_success_body(response, "Anthropic").await?;
        let parsed: AnthropicResponse =
            serde_json::from_str(&body).context("Failed to parse Anthropic API response")?;

        let text = parsed
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<&str>>()
            .join("");

        Ok((text, parsed.usage.input_tokens, parsed.usage.output_tokens))
    }

    /// Returns (text, input_tokens, output_tokens).
    async fn complete_openai(&self, system: &str, user: &str) -> Result<(String, i64, i64)> {
        // OpenAI-compatible APIs carry the system prompt as the first message
        // rather than a dedicated field.
        let request = OpenAiRequest {
            model: self.model.clone(),
            max_tokens: MAX_TOKENS,
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user.to_string(),
                },
            ],
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .context("LLM API request failed")?;

        let body = Self::read_success_body(response, "LLM").await?;
        let parsed: OpenAiResponse =
            serde_json::from_str(&body).context("Failed to parse LLM API response")?;

        let text = parsed
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();

        // usage is optional in the OpenAI schema; absent means we can't bill it.
        let (input_tokens, output_tokens) = parsed
            .usage
            .map(|u| (u.prompt_tokens, u.completion_tokens))
            .unwrap_or((0, 0));

        Ok((text, input_tokens, output_tokens))
    }

    async fn read_success_body(response: reqwest::Response, label: &str) -> Result<String> {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("{label} API error ({status}): {body}");
        }
        Ok(body)
    }

    fn cost(&self, input_tokens: i64, output_tokens: i64) -> Decimal {
        calculate_cost(
            input_tokens,
            output_tokens,
            self.input_price_per_million,
            self.output_price_per_million,
        )
    }

    /// Cost of a typical valuation call, for pre-flight budget checks.
    pub fn estimated_call_cost(&self) -> Decimal {
        self.cost(TYPICAL_INPUT_TOKENS, TYPICAL_OUTPUT_TOKENS)
    }

    async fn track_cost(
        &self,
        input_tokens: i64,
        output_tokens: i64,
        cost: Decimal,
        cycle: Option<i64>,
    ) -> Result<()> {
        let (provider, endpoint) = match self.provider {
            LlmProvider::Anthropic => ("anthropic", "messages"),
            LlmProvider::OpenAiCompatible => ("openai_compatible", "chat/completions"),
        };
        let record = ApiCostRecord {
            id: None,
            provider: provider.to_string(),
            endpoint: Some(endpoint.to_string()),
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

/// Calculate the dollar cost of an LLM call at the given per-million rates.
pub fn calculate_cost(
    input_tokens: i64,
    output_tokens: i64,
    input_price_per_million: Decimal,
    output_price_per_million: Decimal,
) -> Decimal {
    let input_cost = Decimal::from(input_tokens) * input_price_per_million / MILLION;
    let output_cost = Decimal::from(output_tokens) * output_price_per_million / MILLION;
    input_cost + output_cost
}

// --- Request/Response Types ---

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ChatMessage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
    usage: AnthropicUsage,
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
struct AnthropicUsage {
    input_tokens: i64,
    output_tokens: i64,
}

#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<ChatMessage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: i64,
    completion_tokens: i64,
}

/// Parsed response from an LLM call.
#[derive(Debug)]
pub struct LlmResponse {
    pub text: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json_schema, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn valuation_config(provider: LlmProvider, base_url: Option<String>) -> ValuationConfig {
        ValuationConfig {
            provider,
            model: "test-model".to_string(),
            base_url,
            input_price_per_million: None,
            output_price_per_million: None,
            min_edge_threshold: dec!(0.08),
            high_confidence_edge: dec!(0.06),
            low_confidence_edge: dec!(0.10),
            cache_ttl_seconds: 300,
        }
    }

    #[test]
    fn test_cost_calculation() {
        // 1000 input, 500 output at Claude Sonnet rates
        let cost = calculate_cost(
            1000,
            500,
            ANTHROPIC_DEFAULT_INPUT_PRICE,
            ANTHROPIC_DEFAULT_OUTPUT_PRICE,
        );
        // input: 1000 * 3.00 / 1_000_000 = 0.003
        // output: 500 * 15.00 / 1_000_000 = 0.0075
        assert_eq!(cost, dec!(0.0105));
    }

    #[test]
    fn test_cost_calculation_zero_tokens() {
        let cost = calculate_cost(
            0,
            0,
            ANTHROPIC_DEFAULT_INPUT_PRICE,
            ANTHROPIC_DEFAULT_OUTPUT_PRICE,
        );
        assert_eq!(cost, Decimal::ZERO);
    }

    #[test]
    fn test_cost_calculation_large_input() {
        let cost = calculate_cost(
            100_000,
            4_000,
            ANTHROPIC_DEFAULT_INPUT_PRICE,
            ANTHROPIC_DEFAULT_OUTPUT_PRICE,
        );
        assert_eq!(cost, dec!(0.36));
    }

    #[test]
    fn test_free_tier_pricing_is_zero() {
        // A free endpoint (NVIDIA's tier, a self-hosted model) costs nothing
        // no matter how many tokens it burns.
        let cost = calculate_cost(1_000_000, 1_000_000, Decimal::ZERO, Decimal::ZERO);
        assert_eq!(cost, Decimal::ZERO);
    }

    #[tokio::test]
    async fn openai_compatible_requires_base_url() {
        let store = Store::new(":memory:").await.unwrap();
        let err = LlmClient::new(
            "k".to_string(),
            &valuation_config(LlmProvider::OpenAiCompatible, None),
            store,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("base_url is required"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn anthropic_defaults_to_claude_pricing() {
        let store = Store::new(":memory:").await.unwrap();
        let client = LlmClient::new(
            "k".to_string(),
            &valuation_config(LlmProvider::Anthropic, None),
            store,
        )
        .unwrap();
        // 2000 in / 300 out at $3/$15 = 0.006 + 0.0045
        assert_eq!(client.estimated_call_cost(), dec!(0.0105));
    }

    #[tokio::test]
    async fn openai_compatible_defaults_to_free_pricing() {
        let store = Store::new(":memory:").await.unwrap();
        let client = LlmClient::new(
            "k".to_string(),
            &valuation_config(
                LlmProvider::OpenAiCompatible,
                Some("https://example.invalid/v1".to_string()),
            ),
            store,
        )
        .unwrap();
        assert_eq!(client.estimated_call_cost(), Decimal::ZERO);
    }

    #[tokio::test]
    async fn anthropic_request_shape_and_parsing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .and(header("x-api-key", "secret-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .and(body_json_schema::<serde_json::Value>)
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "hello from claude"}],
                "usage": {"input_tokens": 1000, "output_tokens": 500}
            })))
            .mount(&server)
            .await;

        let store = Store::new(":memory:").await.unwrap();
        let client = LlmClient::new(
            "secret-key".to_string(),
            &valuation_config(LlmProvider::Anthropic, Some(server.uri())),
            store,
        )
        .unwrap();

        let resp = client.complete("sys", "user", Some(1)).await.unwrap();
        assert_eq!(resp.text, "hello from claude");
        assert_eq!(resp.input_tokens, 1000);
        assert_eq!(resp.output_tokens, 500);
        assert_eq!(resp.cost, dec!(0.0105));
    }

    #[tokio::test]
    async fn openai_compatible_request_shape_and_parsing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer nvapi-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": "hello from nim"}}],
                "usage": {"prompt_tokens": 1000, "completion_tokens": 500}
            })))
            .mount(&server)
            .await;

        let store = Store::new(":memory:").await.unwrap();
        let mut config = valuation_config(LlmProvider::OpenAiCompatible, Some(server.uri()));
        config.input_price_per_million = Some(dec!(1.00));
        config.output_price_per_million = Some(dec!(2.00));
        let client = LlmClient::new("nvapi-test".to_string(), &config, store).unwrap();

        let resp = client.complete("sys", "user", Some(1)).await.unwrap();
        assert_eq!(resp.text, "hello from nim");
        assert_eq!(resp.input_tokens, 1000);
        assert_eq!(resp.output_tokens, 500);
        // 1000 * 1.00 / 1M + 500 * 2.00 / 1M
        assert_eq!(resp.cost, dec!(0.002));
    }

    #[tokio::test]
    async fn openai_compatible_tolerates_missing_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": "no usage block"}}]
            })))
            .mount(&server)
            .await;

        let store = Store::new(":memory:").await.unwrap();
        let client = LlmClient::new(
            "k".to_string(),
            &valuation_config(LlmProvider::OpenAiCompatible, Some(server.uri())),
            store,
        )
        .unwrap();

        let resp = client.complete("sys", "user", None).await.unwrap();
        assert_eq!(resp.text, "no usage block");
        assert_eq!(resp.input_tokens, 0);
        assert_eq!(resp.cost, Decimal::ZERO);
    }

    #[tokio::test]
    async fn http_error_surfaces_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
            .mount(&server)
            .await;

        let store = Store::new(":memory:").await.unwrap();
        let client = LlmClient::new(
            "bad".to_string(),
            &valuation_config(LlmProvider::OpenAiCompatible, Some(server.uri())),
            store,
        )
        .unwrap();

        let err = client.complete("sys", "user", None).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("401"), "unexpected error: {msg}");
        assert!(msg.contains("invalid api key"), "unexpected error: {msg}");
    }
}
