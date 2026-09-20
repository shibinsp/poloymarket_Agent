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
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::Instrument as _;
use tracing::{info, warn};

use crate::config::{LlmProvider, ValuationConfig};
use crate::db::store::{ApiCostRecord, Store};

const MILLION: Decimal = dec!(1_000_000);

const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Typical valuation call shape, used to estimate spend before making it.
pub const TYPICAL_INPUT_TOKENS: i64 = 2000;
pub const TYPICAL_OUTPUT_TOKENS: i64 = 300;

pub struct LlmClient {
    client: reqwest::Client,
    api_key: String,
    model: String,
    provider: LlmProvider,
    base_url: String,
    max_tokens: u32,
    input_price_per_million: Decimal,
    output_price_per_million: Decimal,
    store: Store,
    /// Whether prompt and completion text is attached to the trace span.
    export_content: bool,
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
    ///
    /// Content export stays **off** on a client built this way. It is a
    /// privacy switch, and a privacy switch whose unset state is "on" is one
    /// that the next call site to be written will silently bypass. Callers
    /// that have a telemetry config to consult turn it on explicitly with
    /// [`with_content_export`](Self::with_content_export).
    pub fn new(api_key: String, config: &ValuationConfig, store: Store) -> Result<Self> {
        let base_url = match (config.provider, config.base_url.as_deref()) {
            (_, Some(url)) => url.trim_end_matches('/').to_string(),
            (LlmProvider::Anthropic, None) => ANTHROPIC_DEFAULT_BASE_URL.to_string(),
            (LlmProvider::OpenAiCompatible, None) => bail!(
                "valuation.base_url is required when valuation.provider = \"openai_compatible\" \
                 (e.g. https://integrate.api.nvidia.com/v1 for NVIDIA NIM)"
            ),
        };

        // Sending Anthropic's x-api-key to an unrelated host would leak the
        // credential; this usually means someone flipped `provider` back
        // without clearing a base_url left over from another provider.
        if config.provider == LlmProvider::Anthropic && base_url != ANTHROPIC_DEFAULT_BASE_URL {
            warn!(
                base_url = %base_url,
                "provider is \"anthropic\" but base_url is not {ANTHROPIC_DEFAULT_BASE_URL} — \
                 the Anthropic API key will be sent to this host"
            );
        }

        let (input_price_per_million, output_price_per_million) = config.effective_pricing();
        if input_price_per_million.is_zero() && output_price_per_million.is_zero() {
            warn!(
                "Valuation pricing is $0 — the daily API budget cap, the edge-justifies-cost \
                 gate and the self-funding survival check will all treat calls as free. Set \
                 valuation.input_price_per_million/output_price_per_million if this endpoint bills you."
            );
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("Failed to build HTTP client")?;

        info!(
            provider = ?config.provider,
            model = %config.model,
            base_url = %base_url,
            max_tokens = config.max_tokens,
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
            max_tokens: config.max_tokens,
            input_price_per_million,
            output_price_per_million,
            store,
            // Fails closed. See the doc comment on `new`.
            export_content: false,
        })
    }

    /// Whether prompts and completions ride along on the trace span.
    ///
    /// Separate from construction because it is a telemetry setting, not a
    /// model one, and the client is built in places that do not know about
    /// telemetry. Pass `config.telemetry.exports_content()`, which is false
    /// whenever there is no exporter to send them to.
    pub fn with_content_export(mut self, export: bool) -> Self {
        self.export_content = export;
        self
    }

    /// Make one model call.
    ///
    /// The span carries GenAI semantic-convention attributes so an OTLP
    /// consumer renders it as a generation — model, token counts, cost —
    /// rather than an anonymous span.
    ///
    /// The span is built by hand rather than with `#[instrument]` so the
    /// prompt can be attached at creation when content export is on, and
    /// simply absent when it is off. Recording it afterwards would leave the
    /// privacy-relevant branch untestable.
    pub async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        cycle: Option<i64>,
    ) -> Result<LlmResponse> {
        // The two arms differ in exactly one thing: whether the prompt is
        // attached at creation. `langfuse.observation.output` is declared in
        // both, so the `if self.export_content` guard around recording it
        // later is load-bearing — remove the guard and the completion really
        // does get exported, which is what makes that guard testable. (It was
        // previously declared only in the content arm, so `record` on the
        // other one was a silent no-op and the guard could be deleted with
        // every test still passing.)
        let span = if self.export_content {
            tracing::info_span!(
                "llm.generation",
                otel.name = "llm.generation",
                gen_ai.operation.name = "chat",
                gen_ai.system = %self.provider.semconv_name(),
                gen_ai.request.model = %self.model,
                gen_ai.request.max_tokens = self.max_tokens,
                langfuse.observation.type = "generation",
                cycle = cycle,
                gen_ai.usage.input_tokens = tracing::field::Empty,
                gen_ai.usage.output_tokens = tracing::field::Empty,
                gen_ai.usage.cost = tracing::field::Empty,
                langfuse.observation.input =
                    %format!("[system]\n{system_prompt}\n\n[user]\n{user_prompt}"),
                langfuse.observation.output = tracing::field::Empty,
            )
        } else {
            tracing::info_span!(
                "llm.generation",
                otel.name = "llm.generation",
                gen_ai.operation.name = "chat",
                gen_ai.system = %self.provider.semconv_name(),
                gen_ai.request.model = %self.model,
                gen_ai.request.max_tokens = self.max_tokens,
                langfuse.observation.type = "generation",
                cycle = cycle,
                gen_ai.usage.input_tokens = tracing::field::Empty,
                gen_ai.usage.output_tokens = tracing::field::Empty,
                gen_ai.usage.cost = tracing::field::Empty,
                langfuse.observation.output = tracing::field::Empty,
            )
        };

        self.complete_inner(system_prompt, user_prompt, cycle)
            .instrument(span)
            .await
    }

    async fn complete_inner(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        cycle: Option<i64>,
    ) -> Result<LlmResponse> {
        let span = tracing::Span::current();

        let (text, input_tokens, output_tokens) = match self.provider {
            LlmProvider::Anthropic => self.complete_anthropic(system_prompt, user_prompt).await?,
            LlmProvider::OpenAiCompatible => {
                self.complete_openai(system_prompt, user_prompt).await?
            }
        };

        let cost = self.cost(input_tokens, output_tokens);

        span.record("gen_ai.usage.input_tokens", input_tokens);
        span.record("gen_ai.usage.output_tokens", output_tokens);
        // As a number, not a string. `display(&Decimal)` produced a String
        // OTLP attribute, and Langfuse's cost mapping ignores anything that
        // is not numeric — so the one figure this span exists to carry was
        // the one it dropped. f64 loses precision that Decimal has, which is
        // why the authoritative record stays in `api_costs`; this is a
        // display value.
        span.record("gen_ai.usage.cost", cost.to_f64().unwrap_or(f64::NAN));
        if self.export_content {
            span.record(
                "langfuse.observation.output",
                tracing::field::display(&text),
            );
        }

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
            max_tokens: self.max_tokens,
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
            max_tokens: self.max_tokens,
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

        let Some(choice) = parsed.choices.first() else {
            bail!("LLM returned no choices (content filter or provider error)");
        };

        // A truncated completion is otherwise indistinguishable from a
        // malformed one downstream, where the JSON parse simply fails.
        if choice.finish_reason.as_deref() == Some("length") {
            bail!(
                "LLM response truncated at max_tokens ({}) — raise valuation.max_tokens; \
                 reasoning models spend this budget before emitting the JSON schema",
                self.max_tokens
            );
        }

        let text = choice.message.content.clone().unwrap_or_default();

        // usage is optional in the OpenAI schema; absent means we can't bill it.
        let (input_tokens, output_tokens) = parsed
            .usage
            .map(|u| (u.prompt_tokens, u.completion_tokens))
            .unwrap_or((0, 0));

        Ok((text, input_tokens, output_tokens))
    }

    async fn read_success_body(response: reqwest::Response, label: &str) -> Result<String> {
        let status = response.status();
        if !status.is_success() {
            // Best-effort on the error path: a body we can't read shouldn't
            // mask the status code we already have.
            let body = response.text().await.unwrap_or_default();
            bail!("{label} API error ({status}): {body}");
        }
        response
            .text()
            .await
            .with_context(|| format!("Failed to read {label} API response body"))
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
    /// "stop", "length", "content_filter", … Absent on some providers.
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    /// `Option` rather than `#[serde(default)] String`: reasoning models and
    /// refusals return an explicit `"content": null`, which would otherwise
    /// fail deserialization outright.
    #[serde(default)]
    content: Option<String>,
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
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn valuation_config(provider: LlmProvider, base_url: Option<String>) -> ValuationConfig {
        ValuationConfig {
            provider,
            model: "test-model".to_string(),
            base_url,
            input_price_per_million: None,
            output_price_per_million: None,
            max_tokens: 1024,
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
            crate::config::ANTHROPIC_DEFAULT_INPUT_PRICE,
            crate::config::ANTHROPIC_DEFAULT_OUTPUT_PRICE,
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
            crate::config::ANTHROPIC_DEFAULT_INPUT_PRICE,
            crate::config::ANTHROPIC_DEFAULT_OUTPUT_PRICE,
        );
        assert_eq!(cost, Decimal::ZERO);
    }

    #[test]
    fn test_cost_calculation_large_input() {
        let cost = calculate_cost(
            100_000,
            4_000,
            crate::config::ANTHROPIC_DEFAULT_INPUT_PRICE,
            crate::config::ANTHROPIC_DEFAULT_OUTPUT_PRICE,
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

    /// Capture the fields recorded on spans, so the GenAI attributes can be
    /// asserted without an OTLP collector in the loop.
    #[derive(Clone, Default)]
    struct FieldCapture(std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>);

    impl<S> tracing_subscriber::Layer<S> for FieldCapture
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            attrs.record(&mut Visitor(self.0.clone()));
        }
        fn on_record(
            &self,
            _id: &tracing::Id,
            values: &tracing::span::Record<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            values.record(&mut Visitor(self.0.clone()));
        }
    }

    struct Visitor(std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>);

    impl tracing::field::Visit for Visitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0
                .lock()
                .unwrap()
                .push((field.name().to_string(), format!("{value:?}")));
        }
        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.0
                .lock()
                .unwrap()
                .push((field.name().to_string(), value.to_string()));
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0
                .lock()
                .unwrap()
                .push((field.name().to_string(), value.to_string()));
        }
        // The cost is an f64 so that OTLP carries it as a number. Without
        // this arm it would fall through to `record_debug` and the assertion
        // below would be comparing against a `Debug` rendering instead.
        fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
            self.0
                .lock()
                .unwrap()
                .push((field.name().to_string(), value.to_string()));
        }
    }

    async fn call_and_capture(export_content: bool) -> Vec<(String, String)> {
        use tracing_subscriber::layer::SubscriberExt;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": "the answer"}}],
                "usage": {"prompt_tokens": 11, "completion_tokens": 7}
            })))
            .mount(&server)
            .await;

        let store = Store::new(":memory:").await.unwrap();
        let client = LlmClient::new(
            "k".to_string(),
            &valuation_config(
                LlmProvider::OpenAiCompatible,
                Some(format!("{}/v1", server.uri())),
            ),
            store,
        )
        .unwrap()
        .with_content_export(export_content);

        let capture = FieldCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        // `set_default` rather than `with_default`: the latter takes a closure,
        // which would mean blocking on the future from inside an async test.
        let guard = tracing::subscriber::set_default(subscriber);
        client
            .complete("SYSTEM-PROMPT", "USER-PROMPT", Some(3))
            .await
            .expect("call succeeds");
        drop(guard);

        let out = capture.0.lock().unwrap().clone();
        out
    }

    /// Langfuse renders a span as a *generation* — with model, tokens and cost
    /// — only when it carries the GenAI semantic-convention attributes. Without
    /// them it is an anonymous span and the whole integration is pointless.
    #[tokio::test]
    async fn an_llm_call_carries_genai_attributes() {
        let fields = call_and_capture(true).await;
        let get = |k: &str| {
            fields
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("missing field {k}: have {fields:?}"))
        };

        assert_eq!(get("otel.name"), "llm.generation");
        assert_eq!(get("gen_ai.operation.name"), "chat");
        assert_eq!(get("gen_ai.request.model"), "test-model");
        assert_eq!(get("langfuse.observation.type"), "generation");
        // `gen_ai.system` must be the semantic-convention spelling, not the
        // Rust variant name: a consumer keying off "OpenAiCompatible" does
        // not exist.
        assert_eq!(get("gen_ai.system"), "openai");

        // The post-call values. These are recorded after the future returns,
        // via `Span::record`, which the capture layer sees through
        // `on_record` — so there is no reason not to assert them here, and
        // every reason to: they are the numbers Langfuse bills and charts.
        assert_eq!(get("gen_ai.usage.input_tokens"), "11");
        assert_eq!(get("gen_ai.usage.output_tokens"), "7");
        // Recorded as a number. As a string, Langfuse's cost mapping drops
        // it silently — the span arrives, the cost column stays empty.
        let cost = get("gen_ai.usage.cost");
        cost.parse::<f64>()
            .unwrap_or_else(|_| panic!("cost must be numeric, got {cost:?}"));
    }

    #[tokio::test]
    async fn the_prompt_rides_along_when_content_export_is_on() {
        let fields = call_and_capture(true).await;
        let joined: String = fields.iter().map(|(_, v)| v.as_str()).collect();
        assert!(joined.contains("SYSTEM-PROMPT"), "system prompt missing");
        assert!(joined.contains("USER-PROMPT"), "user prompt missing");
    }

    /// With content export off the span keeps its metadata and drops the text,
    /// so cost and latency remain visible without the prompt leaving the box.
    #[tokio::test]
    async fn content_is_withheld_when_export_is_off() {
        let fields = call_and_capture(false).await;
        let joined: String = fields.iter().map(|(_, v)| v.as_str()).collect();
        assert!(!joined.contains("SYSTEM-PROMPT"), "prompt leaked: {joined}");
        assert!(!joined.contains("USER-PROMPT"), "prompt leaked");
        assert!(!joined.contains("the answer"), "completion leaked");
        // The span itself is still there, carrying its metadata.
        assert!(fields
            .iter()
            .any(|(n, v)| n == "otel.name" && v == "llm.generation"));
        assert!(fields.iter().any(|(n, _)| n == "gen_ai.request.model"));
    }

    #[tokio::test]
    async fn anthropic_request_shape_and_parsing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .and(header("x-api-key", "secret-key"))
            .and(header("anthropic-version", "2023-06-01"))
            // Assert the actual Anthropic shape: system is its own top-level
            // field and messages carries only the user turn. Without this the
            // test passes even if the two request builders are swapped.
            .and(body_partial_json(serde_json::json!({
                "model": "test-model",
                "max_tokens": 1024,
                "system": "sys",
                "messages": [{"role": "user", "content": "user"}]
            })))
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
            // OpenAI carries the system prompt as messages[0], not a field.
            .and(body_partial_json(serde_json::json!({
                "model": "test-model",
                "max_tokens": 1024,
                "messages": [
                    {"role": "system", "content": "sys"},
                    {"role": "user", "content": "user"}
                ]
            })))
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
    async fn openai_compatible_tolerates_null_content() {
        // Reasoning models and refusals return an explicit content: null.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {"role": "assistant", "content": null},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 0}
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
        assert_eq!(resp.text, "");
    }

    #[tokio::test]
    async fn truncated_completion_is_reported_clearly() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {"role": "assistant", "content": "{\"probability\": 0."},
                    "finish_reason": "length"
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 1024}
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

        let err = client.complete("sys", "user", None).await.unwrap_err();
        assert!(
            err.to_string().contains("truncated at max_tokens"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn empty_choices_is_an_error_not_an_empty_string() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [],
                "usage": {"prompt_tokens": 10, "completion_tokens": 0}
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

        let err = client.complete("sys", "user", None).await.unwrap_err();
        assert!(
            err.to_string().contains("no choices"),
            "unexpected error: {err}"
        );
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
