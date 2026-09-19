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
    #[serde(default)]
    pub sizing_continuous: ContinuousSizingConfig,
    #[serde(default)]
    pub exits_continuous: ExitsContinuousConfig,
    pub execution: ExecutionConfig,
    pub monitoring: MonitoringConfig,
    pub polymarket: PolymarketConfig,
    pub rate_limit: RateLimitConfig,
    pub database: DatabaseConfig,
    /// Trading venues. Empty keeps the legacy Polymarket-only behaviour.
    #[serde(default)]
    pub venues: Vec<VenueConfig>,
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
    /// Longest the agent will sleep when every venue is closed. Bounds only
    /// the closed-market sleep, never the trading cadence: a weekend is two
    /// days, and positions still need marking and orders reconciling in the
    /// middle of it. Default: 1 hour.
    #[serde(default = "default_max_sleep_seconds")]
    pub max_sleep_seconds: u64,
}

fn default_daily_api_budget() -> Decimal {
    rust_decimal_macros::dec!(5.0)
}

fn default_max_sleep_seconds() -> u64 {
    3600
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScanningConfig {
    pub max_markets: usize,
    pub min_volume_24h: Decimal,
    pub max_resolution_days: u32,
    pub max_spread_pct: Decimal,
    pub categories: Vec<String>,
}

/// Claude Sonnet list pricing, used when an Anthropic config doesn't override it.
pub const ANTHROPIC_DEFAULT_INPUT_PRICE: Decimal = rust_decimal_macros::dec!(3.00);
pub const ANTHROPIC_DEFAULT_OUTPUT_PRICE: Decimal = rust_decimal_macros::dec!(15.00);

fn default_max_tokens() -> u32 {
    1024
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
    /// Response token budget. Reasoning models spend this on chain-of-thought
    /// before emitting the JSON schema, so they need considerably more.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    pub min_edge_threshold: Decimal,
    pub high_confidence_edge: Decimal,
    pub low_confidence_edge: Decimal,
    pub cache_ttl_seconds: u64,
}

impl AppConfig {
    /// Symbol universe the venue loop trades. Continuous assets need an
    /// explicit universe — unlike prediction markets, they aren't discovered.
    pub fn venue_symbols(&self) -> Vec<String> {
        self.venues
            .iter()
            .filter(|v| v.enabled)
            .flat_map(|v| v.symbols.clone())
            .collect()
    }

    /// Per-side taker fee assumed when netting edge against costs.
    pub fn venue_fee_pct(&self) -> Decimal {
        self.venues
            .iter()
            .filter(|v| v.enabled)
            .map(|v| v.fee_pct)
            .max()
            .unwrap_or(rust_decimal_macros::dec!(0.0025))
    }

    /// Minimum probability-of-up before a directional view is tradeable.
    pub fn min_p_up(&self) -> Decimal {
        rust_decimal_macros::dec!(0.55)
    }

    /// Cap on orders opened in a single cycle.
    pub fn max_orders_per_cycle(&self) -> usize {
        2
    }
}

/// One configured trading venue.
#[derive(Debug, Clone, Deserialize)]
pub struct VenueConfig {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub data_url: Option<String>,
    /// Explicit universe for continuous assets.
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Per-side taker fee as a fraction.
    #[serde(default = "default_fee_pct")]
    pub fee_pct: Decimal,
}

fn default_fee_pct() -> Decimal {
    rust_decimal_macros::dec!(0.0025)
}

impl ValuationConfig {
    /// Per-million input/output rates, falling back to provider defaults.
    pub fn effective_pricing(&self) -> (Decimal, Decimal) {
        let (default_in, default_out) = match self.provider {
            LlmProvider::Anthropic => (
                ANTHROPIC_DEFAULT_INPUT_PRICE,
                ANTHROPIC_DEFAULT_OUTPUT_PRICE,
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

/// Volatility-targeted sizing for continuous assets (crypto, equities).
/// Prediction markets keep using Kelly and ignore this.
#[derive(Debug, Clone, Deserialize)]
pub struct ContinuousSizingConfig {
    /// Fraction of bankroll risked per trade if the stop is hit.
    #[serde(default = "default_risk_per_trade")]
    pub risk_per_trade_pct: Decimal,
    #[serde(default = "default_atr_period")]
    pub atr_period: usize,
    /// Stop distance as a multiple of ATR.
    #[serde(default = "default_atr_multiplier")]
    pub atr_multiplier: Decimal,
    /// Floor and ceiling on the stop, so a quiet market can't imply a huge
    /// position and a wild one can't imply a meaningless stop.
    #[serde(default = "default_min_stop_pct")]
    pub min_stop_pct: Decimal,
    #[serde(default = "default_max_stop_pct")]
    pub max_stop_pct: Decimal,
}

impl Default for ContinuousSizingConfig {
    fn default() -> Self {
        Self {
            risk_per_trade_pct: default_risk_per_trade(),
            atr_period: default_atr_period(),
            atr_multiplier: default_atr_multiplier(),
            min_stop_pct: default_min_stop_pct(),
            max_stop_pct: default_max_stop_pct(),
        }
    }
}

fn default_risk_per_trade() -> Decimal {
    rust_decimal_macros::dec!(0.0075)
}
fn default_atr_period() -> usize {
    14
}
fn default_atr_multiplier() -> Decimal {
    rust_decimal_macros::dec!(2.0)
}
fn default_min_stop_pct() -> Decimal {
    rust_decimal_macros::dec!(0.03)
}
fn default_max_stop_pct() -> Decimal {
    rust_decimal_macros::dec!(0.12)
}

/// Exit rules for continuous assets, which never settle themselves.
#[derive(Debug, Clone, Deserialize)]
pub struct ExitsContinuousConfig {
    /// Fraction above entry at which to take profit.
    #[serde(default = "default_take_profit_pct")]
    pub take_profit_pct: Decimal,
    /// Close regardless after this long, so a position that goes nowhere does
    /// not tie up capital indefinitely.
    #[serde(default = "default_max_hold_hours")]
    pub max_hold_hours: i64,
}

impl Default for ExitsContinuousConfig {
    fn default() -> Self {
        Self {
            take_profit_pct: default_take_profit_pct(),
            max_hold_hours: default_max_hold_hours(),
        }
    }
}

fn default_take_profit_pct() -> Decimal {
    rust_decimal_macros::dec!(0.06)
}
fn default_max_hold_hours() -> i64 {
    72
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
    /// Alpaca trading credentials. Paper and live use different keys.
    pub alpaca_key_id: Option<String>,
    pub alpaca_secret_key: Option<String>,
}

/// Read an env var, treating blank/whitespace-only as unset.
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl Secrets {
    pub fn from_env() -> Self {
        Self {
            polymarket_private_key: non_empty_env("POLYMARKET_PRIVATE_KEY"),
            // An unset var and one set to "" must behave the same, or the
            // blank `LLM_API_KEY=` line in .env.example would shadow the
            // ANTHROPIC_API_KEY fallback with Some("").
            llm_api_key: non_empty_env("LLM_API_KEY")
                .or_else(|| non_empty_env("ANTHROPIC_API_KEY")),
            discord_webhook_url: non_empty_env("DISCORD_WEBHOOK_URL"),
            noaa_api_token: non_empty_env("NOAA_API_TOKEN"),
            espn_api_key: non_empty_env("ESPN_API_KEY"),
            dashboard_token: non_empty_env("DASHBOARD_TOKEN"),
            alpaca_key_id: non_empty_env("ALPACA_API_KEY_ID"),
            alpaca_secret_key: non_empty_env("ALPACA_API_SECRET_KEY"),
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
        assert_eq!(config.agent.max_sleep_seconds, 3600);
    }

    /// Configs written before the scheduler have no `max_sleep_seconds`, and
    /// one of them is the untracked local.toml `CONFIG_PATH` points at on a
    /// live box. A new key that fails to parse there takes the agent down on
    /// restart, so the default has to hold.
    #[test]
    fn agent_config_without_max_sleep_still_parses() {
        let legacy = r#"
            mode = "paper"
            cycle_interval_seconds = 600
            death_balance_threshold = 0.0
            low_fuel_threshold = 10.0
            api_reserve = 2.0
            initial_paper_balance = 100.0
        "#;
        let agent: AgentConfig = toml::from_str(legacy).expect("should parse");
        assert_eq!(agent.max_sleep_seconds, 3600);
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
                ANTHROPIC_DEFAULT_INPUT_PRICE,
                ANTHROPIC_DEFAULT_OUTPUT_PRICE
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
    fn blank_env_var_is_treated_as_unset() {
        // .env.example ships a blank `LLM_API_KEY=`; if that shadowed the
        // ANTHROPIC_API_KEY fallback, the agent would start "enabled" and 401
        // on every call.
        std::env::set_var("BLANK_ENV_TEST", "");
        assert_eq!(non_empty_env("BLANK_ENV_TEST"), None);
        std::env::set_var("BLANK_ENV_TEST", "   ");
        assert_eq!(non_empty_env("BLANK_ENV_TEST"), None);
        std::env::set_var("BLANK_ENV_TEST", " value ");
        assert_eq!(non_empty_env("BLANK_ENV_TEST"), Some("value".to_string()));
        std::env::remove_var("BLANK_ENV_TEST");
    }

    #[test]
    fn max_tokens_defaults_but_is_overridable() {
        let base = r#"
            model = "m"
            min_edge_threshold = 0.08
            high_confidence_edge = 0.06
            low_confidence_edge = 0.10
            cache_ttl_seconds = 300
        "#;
        let cfg: ValuationConfig = toml::from_str(base).unwrap();
        assert_eq!(cfg.max_tokens, 1024);

        let cfg: ValuationConfig = toml::from_str(&format!("max_tokens = 8192\n{base}")).unwrap();
        assert_eq!(cfg.max_tokens, 8192);
    }

    #[test]
    fn test_database_url() {
        let db = DatabaseConfig {
            path: "test.db".to_string(),
        };
        assert_eq!(db.url(), "sqlite:test.db");
    }
}
