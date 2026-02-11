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
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScanningConfig {
    pub max_markets: usize,
    pub min_volume_24h: Decimal,
    pub max_resolution_days: u32,
    pub max_spread_pct: Decimal,
    pub categories: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValuationConfig {
    pub claude_model: String,
    pub min_edge_threshold: Decimal,
    pub high_confidence_edge: Decimal,
    pub low_confidence_edge: Decimal,
    pub cache_ttl_seconds: u64,
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
    pub anthropic_api_key: Option<String>,
    pub discord_webhook_url: Option<String>,
    pub noaa_api_token: Option<String>,
    pub espn_api_key: Option<String>,
}

impl Secrets {
    pub fn from_env() -> Self {
        Self {
            polymarket_private_key: std::env::var("POLYMARKET_PRIVATE_KEY").ok(),
            anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
            discord_webhook_url: std::env::var("DISCORD_WEBHOOK_URL").ok(),
            noaa_api_token: std::env::var("NOAA_API_TOKEN").ok(),
            espn_api_key: std::env::var("ESPN_API_KEY").ok(),
        }
    }
}

impl AppConfig {
    /// Load configuration from config/default.toml, overlaying environment variables for secrets.
    pub fn load() -> Result<(Self, Secrets)> {
        dotenvy::dotenv().ok();

        let config_path = Path::new("config/default.toml");
        let contents = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

        let config: AppConfig =
            toml::from_str(&contents).context("Failed to parse config/default.toml")?;

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
    fn test_database_url() {
        let db = DatabaseConfig {
            path: "test.db".to_string(),
        };
        assert_eq!(db.url(), "sqlite:test.db");
    }
}
