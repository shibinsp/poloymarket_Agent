//! Building the venue registry from configuration.
//!
//! A venue is only constructed when it is enabled *and* its credentials are
//! present. A configured-but-unusable venue is skipped with a warning rather
//! than failing startup, so one missing key doesn't take the agent down — but
//! it is never silently treated as working either.

use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::config::{AgentMode, AppConfig, Secrets, VenueConfig};
use crate::market::polymarket::PolymarketClient;
use crate::venue::alpaca::{AlpacaConfig, AlpacaVenue};
use crate::venue::polymarket::PolymarketVenue;
use crate::venue::{Venue, VenueRegistry};

/// Build the registry described by `[[venues]]`.
///
/// `polymarket_client` is threaded in rather than constructed here because the
/// legacy loop owns it and both paths must share one client (and therefore one
/// paper balance).
pub fn build_registry(
    config: &AppConfig,
    secrets: &Secrets,
    polymarket_client: Option<Arc<PolymarketClient>>,
) -> Result<VenueRegistry> {
    let mut venues: Vec<Box<dyn Venue>> = Vec::new();

    for venue_config in config.venues.iter().filter(|v| v.enabled) {
        match venue_config.kind.as_str() {
            "alpaca" => match build_alpaca(venue_config, secrets, config.agent.mode) {
                Ok(Some(venue)) => {
                    info!(
                        venue = %venue_config.id,
                        symbols = venue_config.symbols.len(),
                        mode = ?config.agent.mode,
                        "Venue enabled"
                    );
                    venues.push(Box::new(venue));
                }
                Ok(None) => warn!(
                    venue = %venue_config.id,
                    "Alpaca is enabled but ALPACA_API_KEY_ID/ALPACA_API_SECRET_KEY are unset — skipping"
                ),
                Err(e) => {
                    warn!(venue = %venue_config.id, error = %e, "Failed to build Alpaca venue — skipping")
                }
            },
            "polymarket" => match &polymarket_client {
                Some(client) => {
                    info!(venue = %venue_config.id, "Venue enabled");
                    venues.push(Box::new(PolymarketVenue::new(client.clone())));
                }
                None => warn!(
                    venue = %venue_config.id,
                    "Polymarket is enabled but no client was supplied — skipping"
                ),
            },
            other => warn!(
                venue = %venue_config.id,
                kind = other,
                "Unknown venue kind — skipping"
            ),
        }
    }

    Ok(VenueRegistry::new(venues))
}

/// `Ok(None)` means "configured but no credentials", which is a skip, not an
/// error.
fn build_alpaca(
    venue_config: &VenueConfig,
    secrets: &Secrets,
    mode: AgentMode,
) -> Result<Option<AlpacaVenue>> {
    let (Some(key_id), Some(secret_key)) = (&secrets.alpaca_key_id, &secrets.alpaca_secret_key)
    else {
        return Ok(None);
    };

    // Paper and backtest both point at the paper host: backtest must never
    // reach the live trading API even by accident.
    let mut alpaca_config = match mode {
        AgentMode::Live => AlpacaConfig::live(key_id, secret_key),
        AgentMode::Paper | AgentMode::Backtest => AlpacaConfig::paper(key_id, secret_key),
    }
    .with_symbols(venue_config.symbols.clone());

    // An explicit base_url overrides the mode-derived host. Worth noting in
    // the log, since it is how someone would accidentally point paper mode at
    // the live API.
    if let Some(base) = &venue_config.base_url {
        let data = venue_config
            .data_url
            .clone()
            .unwrap_or_else(|| "https://data.alpaca.markets".to_string());
        if mode != AgentMode::Live && base.contains("//api.alpaca.markets") {
            warn!(
                venue = %venue_config.id,
                "base_url points at Alpaca's LIVE host while the agent is not in live mode"
            );
        }
        alpaca_config = alpaca_config.with_urls(base, data);
    }

    let venue = AlpacaVenue::new(alpaca_config).context("Failed to construct the Alpaca venue")?;
    Ok(Some(venue))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::types::VenueId;

    fn config_with(venues: Vec<VenueConfig>) -> AppConfig {
        let toml = std::fs::read_to_string("config/default.toml").unwrap();
        let mut config: AppConfig = toml::from_str(&toml).unwrap();
        config.venues = venues;
        config
    }

    fn alpaca_config(enabled: bool) -> VenueConfig {
        VenueConfig {
            id: "alpaca".to_string(),
            kind: "alpaca".to_string(),
            enabled,
            base_url: None,
            data_url: None,
            symbols: vec!["BTC/USD".to_string()],
            fee_pct: rust_decimal_macros::dec!(0.0025),
        }
    }

    fn secrets_with_alpaca(present: bool) -> Secrets {
        Secrets {
            polymarket_private_key: None,
            llm_api_key: None,
            discord_webhook_url: None,
            noaa_api_token: None,
            espn_api_key: None,
            dashboard_token: None,
            alpaca_key_id: present.then(|| "key".to_string()),
            alpaca_secret_key: present.then(|| "secret".to_string()),
        }
    }

    #[test]
    fn an_enabled_venue_with_credentials_is_built() {
        let config = config_with(vec![alpaca_config(true)]);
        let registry = build_registry(&config, &secrets_with_alpaca(true), None).unwrap();
        assert_eq!(registry.len(), 1);
        assert!(registry.get(&VenueId::new("alpaca")).is_some());
    }

    #[test]
    fn missing_credentials_skip_the_venue_rather_than_failing_startup() {
        let config = config_with(vec![alpaca_config(true)]);
        let registry = build_registry(&config, &secrets_with_alpaca(false), None).unwrap();
        assert!(
            registry.is_empty(),
            "a venue without keys must not be treated as usable"
        );
    }

    #[test]
    fn disabled_venues_are_not_built() {
        let config = config_with(vec![alpaca_config(false)]);
        let registry = build_registry(&config, &secrets_with_alpaca(true), None).unwrap();
        assert!(registry.is_empty());
    }

    #[test]
    fn unknown_venue_kinds_are_skipped_not_fatal() {
        let mut venue = alpaca_config(true);
        venue.kind = "nasdaq".to_string();
        let config = config_with(vec![venue]);
        let registry = build_registry(&config, &secrets_with_alpaca(true), None).unwrap();
        assert!(registry.is_empty());
    }

    #[test]
    fn polymarket_without_a_client_is_skipped() {
        let mut venue = alpaca_config(true);
        venue.kind = "polymarket".to_string();
        venue.id = "polymarket".to_string();
        let config = config_with(vec![venue]);
        let registry = build_registry(&config, &secrets_with_alpaca(false), None).unwrap();
        assert!(registry.is_empty());
    }

    #[test]
    fn no_configured_venues_yields_an_empty_registry() {
        let config = config_with(Vec::new());
        let registry = build_registry(&config, &secrets_with_alpaca(true), None).unwrap();
        assert!(
            registry.is_empty(),
            "legacy behaviour when venues are unset"
        );
    }
}
