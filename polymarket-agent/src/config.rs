use std::path::Path;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
pub use secrecy::{ExposeSecret, SecretString};
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
    /// Tracing export. Absent means off.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
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
    ///
    /// Default: $0.50, roughly 55 Claude calls at ~$0.009 each. Deliberately
    /// low. The first live capital here is $50–100, and a research budget
    /// that can exceed the day's realistic P&L is not a research budget, it
    /// is the largest position the agent takes. Raise it once the paper
    /// window shows what a day actually costs.
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
    rust_decimal_macros::dec!(0.50)
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

impl LlmProvider {
    /// The `gen_ai.system` value from the OpenTelemetry GenAI semantic
    /// conventions.
    ///
    /// Not the Rust variant name. `?self.provider` exported
    /// `"OpenAiCompatible"`, which no consumer recognises — Langfuse and the
    /// OTel collectors both key their model handling off the conventional
    /// spelling, so a span claiming to follow the convention while using
    /// Rust's identifier gets treated as an unknown provider.
    pub fn semconv_name(&self) -> &'static str {
        match self {
            // An OpenAI-compatible endpoint is, by definition, speaking
            // OpenAI's wire format; that is what the attribute describes.
            Self::OpenAiCompatible => "openai",
            Self::Anthropic => "anthropic",
        }
    }
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

    /// Per-side taker fee for one venue.
    ///
    /// Asked per venue, not once for all of them. The previous version
    /// returned the *maximum* fee across every enabled venue and applied it
    /// uniformly, so configuring one expensive venue charged every Alpaca
    /// edge calculation a fee it does not pay — enough to stop anything
    /// clearing `should_trade`.
    pub fn venue_fee_pct(&self, venue_id: &str) -> Decimal {
        self.venues
            .iter()
            .find(|v| v.enabled && v.id == venue_id)
            .map(|v| v.fee_pct)
            .unwrap_or(default_fee_pct())
    }

    /// The universe configured for one venue.
    pub fn venue_symbols_for(&self, venue_id: &str) -> Vec<String> {
        self.venues
            .iter()
            .find(|v| v.enabled && v.id == venue_id)
            .map(|v| v.symbols.clone())
            .unwrap_or_default()
    }

    /// Minimum probability-of-up before a directional view is tradeable.
    pub fn min_p_up(&self) -> Decimal {
        self.sizing_continuous.min_p_up
    }

    /// Cap on orders opened in a single cycle.
    pub fn max_orders_per_cycle(&self) -> usize {
        self.sizing_continuous.max_orders_per_cycle
    }
}

/// Tracing export. Off unless an endpoint is configured.
#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryConfig {
    /// Three-valued on purpose: absent, `true`, or `false`.
    ///
    /// Setting `OTEL_EXPORTER_OTLP_ENDPOINT` turns export on when the config
    /// file has not expressed an opinion — that is the convenient one-env-var
    /// path. But it must *not* override a written `enabled = false`, and with
    /// a plain `bool` there is no way to tell "the operator wrote false" from
    /// "serde defaulted it". Platforms that inject that variable
    /// cluster-wide are common, and since `export_content` defaults on, the
    /// consequence of getting this wrong is the agent's prompts being shipped
    /// to a collector its operator does not control.
    ///
    /// Read through `enabled()`, never directly.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// OTLP/HTTP base URL. A self-hosted Langfuse is
    /// `http://localhost:3000/api/public/otel`; a plain collector is
    /// `http://localhost:4318`. `/v1/traces` is appended if absent.
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
    #[serde(default = "default_service_name")]
    pub service_name: String,
    /// Whether prompts and completions are exported alongside the metadata.
    ///
    /// On by default because the destination is expected to be your own
    /// infrastructure, and a valuation trace without its prompt cannot explain
    /// why the model said what it did. Turn it off and the spans keep model,
    /// token counts, cost and latency.
    #[serde(default = "default_true")]
    pub export_content: bool,
    #[serde(default = "default_export_timeout")]
    pub export_timeout_seconds: u64,
    /// Extra OTLP headers, for collectors that want their own auth.
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    /// Langfuse credentials. Read from the environment, never the config file.
    #[serde(skip)]
    pub langfuse_public_key: Option<String>,
    #[serde(skip)]
    pub langfuse_secret_key: Option<String>,
}

impl TelemetryConfig {
    /// Whether spans are exported at all.
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Whether prompts and completions may leave this process.
    ///
    /// Gated on export actually being configured, not just on the flag.
    /// `export_content` describes what rides along *with a span*; with no
    /// exporter there is no span to ride, and treating the flag as
    /// standalone is what attached multi-kilobyte prompts to the local logs
    /// of deployments that had never turned tracing on.
    pub fn exports_content(&self) -> bool {
        self.enabled()
            && self.export_content
            && self.otlp_endpoint.as_deref().is_some_and(|e| !e.is_empty())
    }
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            otlp_endpoint: None,
            service_name: default_service_name(),
            export_content: true,
            export_timeout_seconds: default_export_timeout(),
            headers: std::collections::HashMap::new(),
            langfuse_public_key: None,
            langfuse_secret_key: None,
        }
    }
}

fn default_service_name() -> String {
    "polymarket-agent".to_string()
}

fn default_true() -> bool {
    true
}

fn default_export_timeout() -> u64 {
    10
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

    // --- Circuit breakers (see `risk::circuit_breaker`) ---
    //
    // Every field below has a default, so an existing config file keeps
    // working — and keeps the breakers *on*, which is the point. A safety
    // limit that has to be opted into is a safety limit nobody has.
    //
    // Losses trip on a strict `>` and counts on a `>=`, so a limit of zero
    // means "none tolerated" rather than silently meaning "disabled". To
    // switch a check off, set it high.
    /// Fraction of the day's starting equity that may be lost before entries
    /// stop for the rest of the UTC day.
    #[serde(default = "default_max_daily_loss_pct")]
    pub max_daily_loss_pct: Decimal,
    /// Cash equivalent of the above. At micro capital the percentage is a
    /// rounding error and this is the number the operator actually agreed to.
    #[serde(default = "default_max_daily_loss_usd")]
    pub max_daily_loss_usd: Decimal,
    /// Fraction below the all-time equity high that halts until a human
    /// resumes. Does not clear at midnight: sleeping on a drawdown changes
    /// nothing about it.
    #[serde(default = "default_max_drawdown_pct")]
    pub max_drawdown_pct: Decimal,
    /// Positions opened per UTC day.
    #[serde(default = "default_max_trades_per_day")]
    pub max_trades_per_day: u32,
    /// Consecutive losing closes before entries stop for the day.
    #[serde(default = "default_max_consecutive_losses")]
    pub max_consecutive_losses: u32,
    /// Absolute ceiling on one live position, in dollars. Applies only in
    /// live mode: a percentage of a paper balance is a number nobody agreed
    /// to, and the first live run must not inherit it.
    #[serde(default = "default_max_live_notional_per_position_usd")]
    pub max_live_notional_per_position_usd: Decimal,
    /// Absolute ceiling on all live positions together, in dollars.
    #[serde(default = "default_max_live_total_notional_usd")]
    pub max_live_total_notional_usd: Decimal,
}

fn default_max_daily_loss_pct() -> Decimal {
    rust_decimal_macros::dec!(0.05)
}
fn default_max_daily_loss_usd() -> Decimal {
    rust_decimal_macros::dec!(5.0)
}
fn default_max_drawdown_pct() -> Decimal {
    rust_decimal_macros::dec!(0.15)
}
fn default_max_trades_per_day() -> u32 {
    10
}
fn default_max_consecutive_losses() -> u32 {
    4
}
fn default_max_live_notional_per_position_usd() -> Decimal {
    rust_decimal_macros::dec!(10.0)
}
fn default_max_live_total_notional_usd() -> Decimal {
    rust_decimal_macros::dec!(60.0)
}

impl Default for RiskConfig {
    /// The documented defaults, so a test can spell out only the field it is
    /// exercising.
    ///
    /// Not a loading path: `RiskConfig` has no struct-level `serde(default)`,
    /// so a config file is still required to state the five sizing fields.
    /// Only the breaker limits below fall back to these when absent.
    fn default() -> Self {
        Self {
            kelly_fraction: rust_decimal_macros::dec!(0.5),
            max_position_pct: rust_decimal_macros::dec!(0.06),
            max_total_exposure_pct: rust_decimal_macros::dec!(0.30),
            max_positions_per_category: 3,
            min_position_usd: rust_decimal_macros::dec!(1.0),
            max_daily_loss_pct: default_max_daily_loss_pct(),
            max_daily_loss_usd: default_max_daily_loss_usd(),
            max_drawdown_pct: default_max_drawdown_pct(),
            max_trades_per_day: default_max_trades_per_day(),
            max_consecutive_losses: default_max_consecutive_losses(),
            max_live_notional_per_position_usd: default_max_live_notional_per_position_usd(),
            max_live_total_notional_usd: default_max_live_total_notional_usd(),
        }
    }
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
    /// Minimum probability-of-up before a directional view is tradeable.
    /// Config rather than a literal: it and the order cap are the two knobs an
    /// operator most needs during a micro-capital rollout, and needing a
    /// recompile to turn either one down is not a real control.
    #[serde(default = "default_min_p_up")]
    pub min_p_up: Decimal,
    /// Cap on orders opened in a single cycle.
    #[serde(default = "default_max_orders_per_cycle")]
    pub max_orders_per_cycle: usize,
}

fn default_min_p_up() -> Decimal {
    rust_decimal_macros::dec!(0.55)
}

fn default_max_orders_per_cycle() -> usize {
    2
}

impl Default for ContinuousSizingConfig {
    fn default() -> Self {
        Self {
            risk_per_trade_pct: default_risk_per_trade(),
            atr_period: default_atr_period(),
            atr_multiplier: default_atr_multiplier(),
            min_stop_pct: default_min_stop_pct(),
            max_stop_pct: default_max_stop_pct(),
            min_p_up: default_min_p_up(),
            max_orders_per_cycle: default_max_orders_per_cycle(),
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
    /// Directory for everything that is not the database file itself: the
    /// `HALT` file and the hourly backups.
    ///
    /// Defaults to the database file's own directory, so an existing config
    /// needs no new key and the operational files land next to the data they
    /// describe. Set it explicitly to put them somewhere else.
    #[serde(default)]
    pub data_dir: Option<String>,
}

impl DatabaseConfig {
    pub fn url(&self) -> String {
        format!("sqlite:{}", self.path)
    }

    /// Where the `HALT` file and the backups live.
    pub fn data_dir(&self) -> std::path::PathBuf {
        if let Some(dir) = self.data_dir.as_deref().filter(|d| !d.is_empty()) {
            return std::path::PathBuf::from(dir);
        }
        // `polymarket-agent.db` — the default — has no parent component, and
        // `Path::parent` returns an empty path for it rather than `None`.
        // Joining onto "" produces a relative path that resolves against the
        // working directory, which is what is wanted, but `PathBuf::from("")`
        // is not a directory anything can be created in.
        match Path::new(&self.path).parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => std::path::PathBuf::from("."),
        }
    }

    /// Warn if the database will land somewhere that depends on where the
    /// process happened to be started.
    ///
    /// A relative `path` resolves against the working directory, which is the
    /// repo when run by hand and `WorkingDirectory=` — or `/` — under
    /// systemd. The two are different files, and the symptom is not an error:
    /// the agent starts cleanly against an empty database and reports no open
    /// positions, while the real ledger sits untouched somewhere else. That
    /// is indistinguishable from "nothing has happened yet" right up until it
    /// places a duplicate of every position it already holds.
    pub fn warn_if_relative(&self) {
        if self.path == ":memory:" || Path::new(&self.path).is_absolute() {
            return;
        }
        let resolved = std::env::current_dir()
            .map(|cwd| cwd.join(&self.path))
            .unwrap_or_else(|_| std::path::PathBuf::from(&self.path));
        tracing::warn!(
            configured = %self.path,
            resolved = %resolved.display(),
            "database.path is relative — it resolves against the working \
             directory, so starting the agent from elsewhere silently opens a \
             different ledger. Set an absolute path."
        );
    }

    /// The file whose existence halts the agent.
    pub fn halt_file(&self) -> std::path::PathBuf {
        self.data_dir().join("HALT")
    }

    /// Directory for hourly database snapshots.
    pub fn backup_dir(&self) -> std::path::PathBuf {
        self.data_dir().join("backups")
    }
}

/// Secrets loaded exclusively from environment variables.
/// Not serializable, not stored in config files.
///
/// `Default` is "no credentials at all", which is a legitimate configuration:
/// paper mode needs none.
///
/// Every field is a `SecretString`, which is what stops these ending up in a
/// log line. The previous `String` fields relied on nobody ever deriving
/// `Debug` on this struct or on anything holding one of its values — true
/// today, and a property no reviewer can check by reading the diff in front
/// of them. `SecretString` has no `Debug` or `Display` that reveals anything,
/// so the mistake stops compiling rather than stops being noticed, and it
/// zeroes its buffer on drop.
///
/// Read one with `.expose_secret()`, which is deliberately conspicuous.
#[derive(Default)]
pub struct Secrets {
    pub polymarket_private_key: Option<SecretString>,
    /// API key for the configured valuation provider. Read from `LLM_API_KEY`,
    /// falling back to `ANTHROPIC_API_KEY`.
    pub llm_api_key: Option<SecretString>,
    /// A capability URL: anyone holding it can post to the channel.
    pub discord_webhook_url: Option<SecretString>,
    pub noaa_api_token: Option<SecretString>,
    pub espn_api_key: Option<SecretString>,
    /// Bearer token protecting the dashboard's `/api/*` routes. Required when
    /// the dashboard is bound to a non-loopback address.
    pub dashboard_token: Option<SecretString>,
    /// Alpaca trading credentials. Paper and live use different keys.
    pub alpaca_key_id: Option<SecretString>,
    pub alpaca_secret_key: Option<SecretString>,
}

/// Read an env var, treating blank/whitespace-only as unset.
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `non_empty_env` for a value that must not be logged.
fn secret_env(key: &str) -> Option<SecretString> {
    non_empty_env(key).map(SecretString::from)
}

impl Secrets {
    pub fn from_env() -> Self {
        Self {
            polymarket_private_key: secret_env("POLYMARKET_PRIVATE_KEY"),
            // An unset var and one set to "" must behave the same, or the
            // blank `LLM_API_KEY=` line in .env.example would shadow the
            // ANTHROPIC_API_KEY fallback with Some("").
            llm_api_key: secret_env("LLM_API_KEY").or_else(|| secret_env("ANTHROPIC_API_KEY")),
            discord_webhook_url: secret_env("DISCORD_WEBHOOK_URL"),
            noaa_api_token: secret_env("NOAA_API_TOKEN"),
            espn_api_key: secret_env("ESPN_API_KEY"),
            dashboard_token: secret_env("DASHBOARD_TOKEN"),
            alpaca_key_id: secret_env("ALPACA_API_KEY_ID"),
            alpaca_secret_key: secret_env("ALPACA_API_SECRET_KEY"),
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

        let mut config: AppConfig = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config file: {}", config_path.display()))?;

        // Telemetry credentials come from the environment, never the config
        // file — the same rule every other secret here follows. The endpoint
        // may also be overridden, so a deployment can point at its own
        // collector without editing a tracked file.
        config.telemetry.langfuse_public_key = non_empty_env("LANGFUSE_PUBLIC_KEY");
        config.telemetry.langfuse_secret_key = non_empty_env("LANGFUSE_SECRET_KEY");
        apply_telemetry_endpoint(
            &mut config.telemetry,
            non_empty_env("OTEL_EXPORTER_OTLP_ENDPOINT")
                .or_else(|| non_empty_env("LANGFUSE_HOST").map(|h| format!("{h}/api/public/otel"))),
        );

        let secrets = Secrets::from_env();

        Ok((config, secrets))
    }
}

/// Point telemetry at an endpoint discovered in the environment.
///
/// Extracted from `load` so the precedence can be tested without mutating
/// process-wide environment variables from a parallel test run.
///
/// The endpoint is always taken. `enabled` is only *defaulted* — a config
/// file that wrote `enabled = false` said so deliberately, and platforms
/// that inject `OTEL_EXPORTER_OTLP_ENDPOINT` cluster-wide must not be able
/// to overrule it. With `export_content` defaulting on, the cost of losing
/// that argument is the agent's prompts going somewhere its operator did not
/// choose.
fn apply_telemetry_endpoint(telemetry: &mut TelemetryConfig, endpoint: Option<String>) {
    let Some(endpoint) = endpoint else {
        return;
    };
    telemetry.otlp_endpoint = Some(endpoint);
    telemetry.enabled.get_or_insert(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Telemetry gating -------------------------------------------------

    #[test]
    fn an_ambient_endpoint_turns_export_on_when_the_config_is_silent() {
        let mut t = TelemetryConfig::default();
        assert_eq!(t.enabled, None, "silence is the default");
        apply_telemetry_endpoint(&mut t, Some("http://collector:4318".to_string()));
        assert!(t.enabled(), "one env var is meant to be enough");
        assert_eq!(t.otlp_endpoint.as_deref(), Some("http://collector:4318"));
    }

    #[test]
    fn an_ambient_endpoint_cannot_override_an_explicit_disable() {
        // The failure this prevents: a k8s platform exports
        // OTEL_EXPORTER_OTLP_ENDPOINT for every pod, and an operator who
        // wrote `enabled = false` gets export anyway — carrying the agent's
        // full prompts, because export_content defaults on.
        let mut t = TelemetryConfig {
            enabled: Some(false),
            ..TelemetryConfig::default()
        };
        apply_telemetry_endpoint(&mut t, Some("http://collector:4318".to_string()));
        assert!(
            !t.enabled(),
            "an explicit `enabled = false` must win over the environment"
        );
        assert!(
            !t.exports_content(),
            "and nothing may be exported from a disabled exporter"
        );
    }

    #[test]
    fn an_explicit_enable_survives_having_no_ambient_endpoint() {
        let mut t = TelemetryConfig {
            enabled: Some(true),
            otlp_endpoint: Some("http://configured:4318".to_string()),
            ..TelemetryConfig::default()
        };
        apply_telemetry_endpoint(&mut t, None);
        assert!(t.enabled());
        assert_eq!(t.otlp_endpoint.as_deref(), Some("http://configured:4318"));
    }

    #[test]
    fn content_is_not_exported_without_an_exporter_to_export_it_to() {
        // `export_content` describes what rides along with a span. With no
        // span leaving the process there is nothing to ride, and treating
        // the flag as standalone is what put multi-kilobyte prompts into the
        // local log files of deployments that never turned tracing on.
        let off = TelemetryConfig::default();
        assert!(off.export_content, "the flag itself defaults on");
        assert!(
            !off.exports_content(),
            "but it must not authorise anything while export is off"
        );

        let enabled_but_unaddressed = TelemetryConfig {
            enabled: Some(true),
            otlp_endpoint: None,
            ..TelemetryConfig::default()
        };
        assert!(
            !enabled_but_unaddressed.exports_content(),
            "enabled with nowhere to send it is still nowhere to send it"
        );

        let empty_endpoint = TelemetryConfig {
            enabled: Some(true),
            otlp_endpoint: Some(String::new()),
            ..TelemetryConfig::default()
        };
        assert!(
            !empty_endpoint.exports_content(),
            "an empty endpoint string is not an endpoint"
        );

        let live = TelemetryConfig {
            enabled: Some(true),
            otlp_endpoint: Some("http://collector:4318".to_string()),
            ..TelemetryConfig::default()
        };
        assert!(
            live.exports_content(),
            "fully configured: content rides along"
        );

        let opted_out = TelemetryConfig {
            export_content: false,
            ..live
        };
        assert!(
            !opted_out.exports_content(),
            "and the flag still turns it off on its own"
        );
    }

    #[test]
    fn the_provider_attribute_uses_the_semantic_convention_spelling() {
        assert_eq!(LlmProvider::Anthropic.semconv_name(), "anthropic");
        assert_eq!(LlmProvider::OpenAiCompatible.semconv_name(), "openai");
    }

    /// The paper-window template has to deserialize, not merely be valid
    /// TOML. It is the file an operator copies to start the ≥14-day window,
    /// and a typo in it surfaces as the agent refusing to boot at the exact
    /// moment they are trying to begin.
    #[test]
    fn the_paper_template_parses_and_enables_a_venue() {
        let contents =
            std::fs::read_to_string("config/paper.toml").expect("config/paper.toml should exist");
        let config: AppConfig = toml::from_str(&contents).expect("should parse");

        assert_eq!(config.agent.mode, AgentMode::Paper);
        // The whole point of the template: without this the agent falls back
        // to the legacy Polymarket-only loop and exercises none of the venue
        // path — which is how the paper window failed to start at all.
        assert_eq!(config.venues.len(), 1);
        assert!(config.venues[0].enabled);
        assert!(
            config.venues[0].symbols.iter().any(|s| s.contains('/')),
            "needs a 24/7 crypto symbol to cover weekends"
        );
        assert!(config.venue_symbols().len() >= 2);

        // Absolute, or the ledger depends on the working directory.
        assert!(
            Path::new(&config.database.path).is_absolute(),
            "the template must not ship a relative database path"
        );

        // `max_markets` is not Polymarket-only: the venue cycle passes it as
        // `ScanFilter.max_results` and the Alpaca adapter truncates to it. A
        // template that sets it below the symbol universe discovers nothing
        // and trades nothing — silently, for as long as it is left running.
        assert!(
            config.scanning.max_markets >= config.venue_symbols().len(),
            "max_markets ({}) must cover the {} configured symbols, or the \
             venue scan truncates them away",
            config.scanning.max_markets,
            config.venue_symbols().len()
        );

        // Likewise the budget: the venue path is skipped entirely once the
        // day's ledger is spent, so a budget that runs out mid-morning
        // produces the same empty window as a wrong max_markets.
        //
        // One valuation per symbol per cycle, at roughly $0.009 a call.
        let cycles_per_day =
            rust_decimal_macros::dec!(86400) / Decimal::from(config.agent.cycle_interval_seconds);
        let daily_cost = cycles_per_day
            * Decimal::from(config.venue_symbols().len())
            * rust_decimal_macros::dec!(0.009);
        assert!(
            config.agent.daily_api_budget >= daily_cost,
            "daily_api_budget ({}) is below the ~{daily_cost} a full day of \
             valuations costs for this universe and cadence",
            config.agent.daily_api_budget
        );

        // The live caps stay at their live values even in the paper file, so
        // going live changes the mode and not the risk numbers.
        assert_eq!(
            config.risk.max_live_notional_per_position_usd,
            rust_decimal_macros::dec!(10.0)
        );
        assert_eq!(
            config.risk.max_live_total_notional_usd,
            rust_decimal_macros::dec!(60.0)
        );
    }

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

        // The tracked default must leave `enabled` *unset*, not write
        // `false`. Written false is an override that beats the environment,
        // which would silently break the documented one-env-var path for
        // everyone who never edited this file.
        assert_eq!(
            config.telemetry.enabled, None,
            "config/default.toml must not pin telemetry.enabled"
        );
        assert!(!config.telemetry.enabled(), "and it is off until asked for");
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
    fn the_data_directory_defaults_to_the_database_files_own_directory() {
        let db = DatabaseConfig {
            path: "/var/lib/agent/agent.db".to_string(),
            data_dir: None,
        };
        assert_eq!(db.data_dir(), Path::new("/var/lib/agent"));
        assert_eq!(db.halt_file(), Path::new("/var/lib/agent/HALT"));
        assert_eq!(db.backup_dir(), Path::new("/var/lib/agent/backups"));
    }

    #[test]
    fn a_bare_database_filename_still_yields_a_usable_directory() {
        // `Path::parent` on "agent.db" returns an *empty* path rather than
        // None, and `PathBuf::from("")` is not somewhere a file can be
        // created — so the HALT file and the backup directory would both have
        // been unusable with the default config.
        let db = DatabaseConfig {
            path: "polymarket-agent.db".to_string(),
            data_dir: None,
        };
        assert_eq!(db.data_dir(), Path::new("."));
        assert_eq!(db.halt_file(), Path::new("./HALT"));
    }

    #[test]
    fn an_explicit_data_dir_wins_over_the_database_location() {
        let db = DatabaseConfig {
            path: "/var/lib/agent/agent.db".to_string(),
            data_dir: Some("/run/agent".to_string()),
        };
        assert_eq!(db.halt_file(), Path::new("/run/agent/HALT"));
    }

    #[test]
    fn a_blank_data_dir_is_treated_as_unset() {
        let db = DatabaseConfig {
            path: "/var/lib/agent/agent.db".to_string(),
            data_dir: Some(String::new()),
        };
        assert_eq!(db.data_dir(), Path::new("/var/lib/agent"));
    }

    #[test]
    fn test_database_url() {
        let db = DatabaseConfig {
            path: "test.db".to_string(),
            data_dir: None,
        };
        assert_eq!(db.url(), "sqlite:test.db");
    }
}
