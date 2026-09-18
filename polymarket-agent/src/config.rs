use std::path::Path;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub agent: AgentConfig,
    pub scanning: ScanningConfig,
    pub valuation: ValuationConfig,
    pub risk: RiskConfig,
    pub execution: ExecutionConfig,
    pub monitoring: MonitoringConfig,
    pub polymarket: PolymarketConfig,
    pub rate_limit: RateLimitConfig,
    pub database: DatabaseConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    Paper,
    Live,
    Backtest,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub mode: AgentMode,
    pub cycle_interval_seconds: u64,
    pub death_balance_threshold: Decimal,
    pub low_fuel_threshold: Decimal,
    pub api_reserve: Decimal,
    pub initial_paper_balance: Decimal,
    /// Maximum API spend per calendar day (UTC). Stops new evaluations once hit.
    /// Default: $5.00 — sufficient for ~550 Claude calls at ~$0.009 each.
    #[serde(default = "default_daily_api_budget")]
    pub daily_api_budget: Decimal,
}

fn default_daily_api_budget() -> Decimal {
    rust_decimal_macros::dec!(5.0)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScanningConfig {
    pub max_markets: usize,
    pub min_volume_24h: Decimal,
    pub max_resolution_days: u32,
    pub max_spread_pct: Decimal,
    pub categories: Vec<String>,
}

/// Wire format the valuation LLM speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProvider {
    /// Anthropic Messages API (`/v1/messages`, `x-api-key`).
    #[default]
    Anthropic,
    /// Any OpenAI-compatible chat-completions endpoint — NVIDIA NIM, vLLM,
    /// OpenRouter, Together. Requires `base_url`.
    // Pinned explicitly: snake_case would derive "open_ai_compatible".
    #[serde(rename = "openai_compatible", alias = "open_ai_compatible")]
    OpenAiCompatible,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValuationConfig {
    #[serde(default)]
    pub provider: LlmProvider,
    /// Model identifier as the provider names it.
    #[serde(alias = "claude_model")]
    pub model: String,
    /// API root (no trailing `/chat/completions` or `/messages`). Required for
    /// `openai_compatible`; defaults to Anthropic's endpoint otherwise.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Cost tracking rates. Default to Claude Sonnet pricing for Anthropic and
    /// to zero for `openai_compatible` (free tiers and self-hosted models) —
    /// set these if your endpoint actually bills you, or the daily API budget
    /// and the edge-justifies-cost gate will both think calls are free.
    #[serde(default)]
    pub input_price_per_million: Option<Decimal>,
    #[serde(default)]
    pub output_price_per_million: Option<Decimal>,
    pub min_edge_threshold: Decimal,
    pub high_confidence_edge: Decimal,
    pub low_confidence_edge: Decimal,
    pub cache_ttl_seconds: u64,
}

impl ValuationConfig {
    /// Per-million input/output rates, falling back to provider defaults.
    pub fn effective_pricing(&self) -> (Decimal, Decimal) {
        let (default_in, default_out) = match self.provider {
            LlmProvider::Anthropic => (
                crate::valuation::llm::ANTHROPIC_DEFAULT_INPUT_PRICE,
                crate::valuation::llm::ANTHROPIC_DEFAULT_OUTPUT_PRICE,
            ),
            LlmProvider::OpenAiCompatible => (Decimal::ZERO, Decimal::ZERO),
        };
        (
            self.input_price_per_million.unwrap_or(default_in),
            self.output_price_per_million.unwrap_or(default_out),
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    pub kelly_fraction: Decimal,
    pub max_position_pct: Decimal,
    pub max_total_exposure_pct: Decimal,
    pub max_positions_per_category: u32,
    pub min_position_usd: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionConfig {
    pub order_type: String,
    pub order_ttl_seconds: u64,
    pub max_slippage_pct: Decimal,
    pub max_retries: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MonitoringConfig {
    pub log_level: String,
    pub discord_enabled: bool,
    pub daily_summary_hour: u32,
    #[serde(default = "default_dashboard_port")]
    pub dashboard_port: u16,
    #[serde(default = "default_dashboard_bind")]
    pub dashboard_bind: String,
}

fn default_dashboard_port() -> u16 {
    8080
}

fn default_dashboard_bind() -> String {
    "127.0.0.1".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolymarketConfig {
    pub clob_base_url: String,
    pub gamma_base_url: String,
    pub chain_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    pub requests_per_second: u32,
    pub burst_size: u32,
    pub backoff_base_ms: u64,
    pub backoff_max_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub path: String,
}

impl DatabaseConfig {
    pub fn url(&self) -> String {
        format!("sqlite:{}", self.path)
    }
}

/// Secrets loaded exclusively from environment variables.
/// Not serializable, not stored in config files.
pub struct Secrets {
    pub polymarket_private_key: Option<String>,
    /// API key for the configured valuation provider. Read from `LLM_API_KEY`,
    /// falling back to `ANTHROPIC_API_KEY`.
    pub llm_api_key: Option<String>,
    pub discord_webhook_url: Option<String>,
    pub noaa_api_token: Option<String>,
    pub espn_api_key: Option<String>,
    /// Bearer token protecting the dashboard's `/api/*` routes. Required when
    /// the dashboard is bound to a non-loopback address.
    pub dashboard_token: Option<String>,
}

impl Secrets {
    pub fn from_env() -> Self {
        Self {
            polymarket_private_key: std::env::var("POLYMARKET_PRIVATE_KEY").ok(),
            llm_api_key: std::env::var("LLM_API_KEY")
                .ok()
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok()),
            discord_webhook_url: std::env::var("DISCORD_WEBHOOK_URL").ok(),
            noaa_api_token: std::env::var("NOAA_API_TOKEN").ok(),
            espn_api_key: std::env::var("ESPN_API_KEY").ok(),
            dashboard_token: std::env::var("DASHBOARD_TOKEN").ok(),
        }
    }
}

/// Where to read the TOML config from when `CONFIG_PATH` is unset.
const DEFAULT_CONFIG_PATH: &str = "config/default.toml";

impl AppConfig {
    /// Load configuration from `$CONFIG_PATH` (default `config/default.toml`),
    /// overlaying environment variables for secrets.
    ///
    /// The override lets a deployment keep its tuned config outside the git
    /// checkout (see deploy/setup.sh), so redeploying never has to edit — or
    /// revert — a tracked file.
    pub fn load() -> Result<(Self, Secrets)> {
        dotenvy::dotenv().ok();

        let config_path =
            std::env::var("CONFIG_PATH").unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());
        let config_path = Path::new(&config_path);
        let contents = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

        let config: AppConfig = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config file: {}", config_path.display()))?;

        let secrets = Secrets::from_env();

        Ok((config, secrets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_default_config() {
        let contents = std::fs::read_to_string("config/default.toml")
            .expect("config/default.toml should exist");
        let config: AppConfig = toml::from_str(&contents).expect("should parse");
        assert_eq!(config.agent.mode, AgentMode::Paper);
        assert_eq!(config.agent.cycle_interval_seconds, 600);
        assert_eq!(config.scanning.max_markets, 1000);
        assert_eq!(config.polymarket.chain_id, 137);
    }

    #[test]
    fn legacy_claude_model_key_still_parses() {
        // Configs written before the provider abstraction only had
        // `claude_model` and no `provider`; they must keep working.
        let legacy = r#"
            claude_model = "claude-sonnet-4-20250514"
            min_edge_threshold = 0.08
            high_confidence_edge = 0.06
            low_confidence_edge = 0.10
            cache_ttl_seconds = 300
        "#;
        let cfg: ValuationConfig = toml::from_str(legacy).expect("legacy config should parse");
        assert_eq!(cfg.provider, LlmProvider::Anthropic);
        assert_eq!(cfg.model, "claude-sonnet-4-20250514");
        assert_eq!(cfg.base_url, None);
        // Falls back to Claude pricing so cost tracking is unchanged.
        assert_eq!(
            cfg.effective_pricing(),
            (
                crate::valuation::llm::ANTHROPIC_DEFAULT_INPUT_PRICE,
                crate::valuation::llm::ANTHROPIC_DEFAULT_OUTPUT_PRICE
            )
        );
    }

    #[test]
    fn openai_compatible_provider_parses_and_defaults_to_free() {
        let cfg: ValuationConfig = toml::from_str(
            r#"
            provider = "openai_compatible"
            model = "deepseek-ai/deepseek-v4-flash-0731"
            base_url = "https://integrate.api.nvidia.com/v1"
            min_edge_threshold = 0.08
            high_confidence_edge = 0.06
            low_confidence_edge = 0.10
            cache_ttl_seconds = 300
        "#,
        )
        .expect("should parse");
        assert_eq!(cfg.provider, LlmProvider::OpenAiCompatible);
        assert_eq!(cfg.effective_pricing(), (Decimal::ZERO, Decimal::ZERO));
    }

    #[test]
    fn explicit_pricing_overrides_provider_defaults() {
        let cfg: ValuationConfig = toml::from_str(
            r#"
            provider = "openai_compatible"
            model = "m"
            base_url = "https://example.invalid/v1"
            input_price_per_million = 0.5
            output_price_per_million = 1.5
            min_edge_threshold = 0.08
            high_confidence_edge = 0.06
            low_confidence_edge = 0.10
            cache_ttl_seconds = 300
        "#,
        )
        .expect("should parse");
        assert_eq!(
            cfg.effective_pricing(),
            (
                rust_decimal_macros::dec!(0.5),
                rust_decimal_macros::dec!(1.5)
            )
        );
    }

    #[test]
    fn test_database_url() {
        let db = DatabaseConfig {
            path: "test.db".to_string(),
        };
        assert_eq!(db.url(), "sqlite:test.db");
    }
}
