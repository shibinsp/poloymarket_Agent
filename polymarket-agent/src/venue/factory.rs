//! Building the venue registry from configuration.
//!
//! A venue is only constructed when it is enabled *and* its credentials are
//! present. A configured-but-unusable venue is skipped with a warning rather
//! than failing startup, so one missing key doesn't take the agent down — but
//! it is never silently treated as working either.

use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::config::{AgentMode, AppConfig, ExposeSecret, Secrets, VenueConfig};
use crate::market::polymarket::PolymarketClient;
use crate::venue::alpaca::{AlpacaConfig, AlpacaVenue};
use crate::venue::binance_us::{BinanceUsConfig, BinanceUsVenue};
use crate::venue::coinbase::{CoinbaseConfig, CoinbaseVenue};
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
    Ok(build_registry_reporting(config, secrets, polymarket_client).0)
}

/// Why a configured venue is not in the registry.
///
/// Returned rather than only logged, because "enabled in the config" and
/// "present in the registry" are different sets and the caller — the dry run
/// — has to be able to say *which* of three unrelated reasons applies.
/// Telling an operator their credentials are missing when they mistyped a
/// `kind` sends them to the wrong file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedVenue {
    pub id: String,
    pub reason: String,
}

/// `build_registry`, plus the skips.
pub fn build_registry_reporting(
    config: &AppConfig,
    secrets: &Secrets,
    polymarket_client: Option<Arc<PolymarketClient>>,
) -> (VenueRegistry, Vec<SkippedVenue>) {
    let mut venues: Vec<Box<dyn Venue>> = Vec::new();
    let mut skipped: Vec<SkippedVenue> = Vec::new();
    let mut skip = |id: &str, reason: String| {
        warn!(venue = %id, reason = %reason, "Venue skipped");
        skipped.push(SkippedVenue {
            id: id.to_string(),
            reason,
        });
    };

    for venue_config in config.venues.iter().filter(|v| v.enabled) {
        // Matched case-insensitively. A `kind = "Alpaca"` is a one-character
        // mistake that used to fall through to "unknown kind" and take the
        // venue with it.
        match venue_config.kind.to_ascii_lowercase().as_str() {
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
                Ok(None) => skip(
                    &venue_config.id,
                    "ALPACA_API_KEY_ID/ALPACA_API_SECRET_KEY are unset".to_string(),
                ),
                Err(e) => skip(&venue_config.id, format!("{e:#}")),
            },
            "coinbase" => match build_coinbase(venue_config, secrets, config.agent.mode) {
                Ok(Some(venue)) => {
                    info!(
                        venue = %venue_config.id,
                        symbols = venue_config.symbols.len(),
                        mode = ?config.agent.mode,
                        "Venue enabled"
                    );
                    venues.push(Box::new(venue));
                }
                Ok(None) => skip(
                    &venue_config.id,
                    "COINBASE_API_KEY_NAME/COINBASE_API_PRIVATE_KEY are unset".to_string(),
                ),
                Err(e) => skip(&venue_config.id, format!("{e:#}")),
            },
            "binance_us" | "binanceus" | "binance.us" => {
                match build_binance_us(venue_config, secrets, config.agent.mode) {
                    Ok(Some(venue)) => {
                        info!(
                            venue = %venue_config.id,
                            symbols = venue_config.symbols.len(),
                            mode = ?config.agent.mode,
                            "Venue enabled"
                        );
                        venues.push(Box::new(venue));
                    }
                    Ok(None) => skip(
                        &venue_config.id,
                        "BINANCE_US_API_KEY/BINANCE_US_SECRET_KEY are unset".to_string(),
                    ),
                    Err(e) => skip(&venue_config.id, format!("{e:#}")),
                }
            }
            "polymarket" => match &polymarket_client {
                Some(client) => {
                    let venue = PolymarketVenue::new(client.clone());
                    // Refused, not merely warned about. Every loss limit is
                    // measured against equity summed across the registry, and
                    // this venue cannot report one — its position listing is
                    // unimplemented, so `equity()` is always `None`.
                    // Enabling it therefore made the sum unknowable and halted
                    // the agent `UntilResume` on every cycle, whatever the
                    // other venues said. The legacy Polymarket loop, which has
                    // its own equity, runs when no venue is configured.
                    if !venue.reports_equity() {
                        skip(
                            &venue_config.id,
                            "Polymarket cannot report account equity, so the risk limits \
                             could not be evaluated for any venue while it is enabled — \
                             leave `[[venues]]` unset to use the legacy Polymarket loop"
                                .to_string(),
                        );
                    } else {
                        info!(venue = %venue_config.id, "Venue enabled");
                        venues.push(Box::new(venue));
                    }
                }
                None => skip(
                    &venue_config.id,
                    "no Polymarket client was supplied".to_string(),
                ),
            },
            other => skip(
                &venue_config.id,
                format!(
                    "unknown venue kind {other:?} — expected \"alpaca\", \"coinbase\", \"binance_us\" or \"polymarket\""
                ),
            ),
        }
    }

    (VenueRegistry::new(venues), skipped)
}

/// `Ok(None)` means "configured but no credentials", which is a skip, not an
/// error.
///
/// Coinbase has no paper endpoint — its sandbox serves auth and serialization
/// only, not a matching engine — so unlike Alpaca there is no mode-derived
/// host to swap: an enabled Coinbase venue reaches the live exchange, and
/// `venue_cycle` calls `place_order` on every venue in the registry without
/// asking which mode the agent is in. There is no paper simulator on the
/// `Venue` path.
///
/// So the mode check lives here, where the venue is built. Leaving it to a
/// disabled line in the shipped config made a comment the only thing standing
/// between `mode = "paper"` and real money, and the file operators are told to
/// edit is `config/local.toml`, which that comment is not in.
fn build_coinbase(
    venue_config: &VenueConfig,
    secrets: &Secrets,
    mode: AgentMode,
) -> Result<Option<CoinbaseVenue>> {
    if mode != AgentMode::Live {
        anyhow::bail!(
            "Coinbase has no paper endpoint, so an enabled Coinbase venue trades real \
             money — refusing to build it while agent.mode is {mode:?}. Set \
             agent.mode = \"live\" if that is what you intend."
        );
    }

    let (Some(key_name), Some(private_key)) =
        (&secrets.coinbase_key_name, &secrets.coinbase_private_key)
    else {
        return Ok(None);
    };

    let mut config = CoinbaseConfig::new(key_name.expose_secret(), private_key.expose_secret())
        .with_venue_id(venue_config.id.clone())
        .with_symbols(venue_config.symbols.clone());

    if let Some(base) = &venue_config.base_url {
        config = config.with_base_url(base.clone());
    }

    Ok(Some(CoinbaseVenue::new(config)?))
}

/// `Ok(None)` means "configured but no credentials", which is a skip, not an
/// error.
///
/// Binance.US has **no testnet**. `testnet.binance.vision` belongs to global
/// Binance, which is a different exchange with a different symbol list and one
/// that blocks US persons — so there is no host to swap and no simulator on
/// the `Venue` path. Same reasoning, and the same guard, as Coinbase.
fn build_binance_us(
    venue_config: &VenueConfig,
    secrets: &Secrets,
    mode: AgentMode,
) -> Result<Option<BinanceUsVenue>> {
    if mode != AgentMode::Live {
        anyhow::bail!(
            "Binance.US has no testnet, so an enabled Binance.US venue trades real money \
             — refusing to build it while agent.mode is {mode:?}. Set \
             agent.mode = \"live\" if that is what you intend."
        );
    }

    let (Some(api_key), Some(secret_key)) =
        (&secrets.binance_us_api_key, &secrets.binance_us_secret_key)
    else {
        return Ok(None);
    };

    let mut config = BinanceUsConfig::new(api_key.expose_secret(), secret_key.expose_secret())
        .with_venue_id(venue_config.id.clone())
        .with_symbols(venue_config.symbols.clone());

    if let Some(base) = &venue_config.base_url {
        config = config.with_base_url(base.clone());
    }

    Ok(Some(BinanceUsVenue::new(config)?))
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
        AgentMode::Live => AlpacaConfig::live(key_id.expose_secret(), secret_key.expose_secret()),
        AgentMode::Paper | AgentMode::Backtest => {
            AlpacaConfig::paper(key_id.expose_secret(), secret_key.expose_secret())
        }
    }
    // The configured id, not the adapter's hardcoded default. Every
    // per-venue lookup keys on the id the venue reports —
    // `venue_symbols_for`, `venue_fee_pct`, `registry.get` — so a venue
    // configured as anything other than "alpaca" silently traded no symbols
    // and paid the default fee.
    .with_venue_id(venue_config.id.clone())
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

    /// Coinbase is live-only, so its tests have to say so.
    fn live_config_with(venues: Vec<VenueConfig>) -> AppConfig {
        let mut config = config_with(venues);
        config.agent.mode = AgentMode::Live;
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
            alpaca_key_id: present.then(|| crate::config::SecretString::from("key")),
            coinbase_key_name: None,
            coinbase_private_key: None,
            binance_us_api_key: None,
            binance_us_secret_key: None,
            alpaca_secret_key: present.then(|| crate::config::SecretString::from("secret")),
        }
    }

    const COINBASE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgevZzL1gdAFr88hb2\n\
OF/2NxApJCzGCEDdfSp6VQO30hyhRANCAAQRWz+jn65BtOMvdyHKcvjBeBSDZH2r\n\
1RTwjmYSi9R/zpBnuQ4EiMnCqfMPWiZqB4QdbAd0E7oH50VpuZ1P087G\n\
-----END PRIVATE KEY-----";

    fn coinbase_config(enabled: bool) -> VenueConfig {
        VenueConfig {
            id: "coinbase".to_string(),
            kind: "coinbase".to_string(),
            enabled,
            base_url: None,
            data_url: None,
            symbols: vec!["BTC/USD".to_string()],
            fee_pct: rust_decimal_macros::dec!(0.012),
        }
    }

    fn secrets_with_coinbase(present: bool) -> Secrets {
        Secrets {
            coinbase_key_name: present
                .then(|| crate::config::SecretString::from("organizations/o/apiKeys/k")),
            coinbase_private_key: present.then(|| crate::config::SecretString::from(COINBASE_PEM)),
            ..secrets_with_alpaca(false)
        }
    }

    #[test]
    fn a_coinbase_venue_with_credentials_is_built() {
        let registry = build_registry(
            &live_config_with(vec![coinbase_config(true)]),
            &secrets_with_coinbase(true),
            None,
        )
        .unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.all().next().unwrap().id().as_str(), "coinbase");
    }

    /// Coinbase has **no paper endpoint** — its sandbox serves auth and
    /// serialization only, with no matching engine — and there is no paper
    /// simulator on the `Venue` path: `venue_cycle` calls `place_order` on
    /// every registry venue whatever the mode. So an enabled Coinbase venue in
    /// a "paper window" spends real money, and the only thing that can stop it
    /// is refusing to build it.
    #[test]
    fn coinbase_is_refused_outright_in_paper_mode() {
        let mut config = config_with(vec![coinbase_config(true)]);
        config.agent.mode = AgentMode::Paper;
        let (registry, skipped) =
            build_registry_reporting(&config, &secrets_with_coinbase(true), None);

        assert!(
            registry.is_empty(),
            "a paper window must not be able to place a real Coinbase order"
        );
        assert_eq!(skipped.len(), 1);
        assert!(
            skipped[0].reason.contains("real money") && skipped[0].reason.contains("Paper"),
            "the reason must say why, and name the mode: {}",
            skipped[0].reason
        );
    }

    #[test]
    fn coinbase_is_refused_in_backtest_mode_too() {
        let mut config = config_with(vec![coinbase_config(true)]);
        config.agent.mode = AgentMode::Backtest;
        let (registry, _) = build_registry_reporting(&config, &secrets_with_coinbase(true), None);
        assert!(
            registry.is_empty(),
            "a backtest reaching the live exchange is worse than a paper one"
        );
    }

    /// The guard is in the builder, not in a comment in a file operators are
    /// told to copy — but the shipped template should still not enable it.
    #[test]
    fn the_shipped_paper_template_does_not_enable_coinbase() {
        let toml = std::fs::read_to_string("config/paper.toml").unwrap();
        let config: AppConfig = toml::from_str(&toml).unwrap();
        assert!(
            !config
                .venues
                .iter()
                .any(|v| v.enabled && v.kind.eq_ignore_ascii_case("coinbase")),
            "Coinbase has no paper mode"
        );
    }

    #[test]
    fn coinbase_without_credentials_is_skipped_with_a_reason_naming_them() {
        let (registry, skipped) = build_registry_reporting(
            &live_config_with(vec![coinbase_config(true)]),
            &secrets_with_coinbase(false),
            None,
        );
        assert!(registry.is_empty());
        assert_eq!(skipped.len(), 1);
        assert!(
            skipped[0].reason.contains("COINBASE_API_KEY_NAME"),
            "the reason must name the variables to set: {}",
            skipped[0].reason
        );
    }

    /// A malformed key is a construction error, not a missing credential —
    /// and reporting it as the latter sends the operator to check env vars
    /// that are already set.
    #[test]
    fn a_malformed_coinbase_key_is_reported_as_itself() {
        let secrets = Secrets {
            coinbase_key_name: Some(crate::config::SecretString::from(
                "organizations/o/apiKeys/k",
            )),
            coinbase_private_key: Some(crate::config::SecretString::from("not a pem")),
            ..secrets_with_alpaca(false)
        };
        let (registry, skipped) = build_registry_reporting(
            &live_config_with(vec![coinbase_config(true)]),
            &secrets,
            None,
        );
        assert!(registry.is_empty());
        assert!(
            skipped[0].reason.contains("P-256"),
            "the reason should name what was expected: {}",
            skipped[0].reason
        );
        assert!(
            !skipped[0].reason.contains("unset"),
            "and must not claim the credentials are missing: {}",
            skipped[0].reason
        );
    }

    /// The venue id comes from the config, not the adapter's default — every
    /// per-venue lookup keys on the id the venue reports.
    #[test]
    fn a_coinbase_venue_reports_its_configured_id() {
        let mut cfg = coinbase_config(true);
        cfg.id = "coinbase-main".to_string();
        let registry = build_registry(
            &live_config_with(vec![cfg]),
            &secrets_with_coinbase(true),
            None,
        )
        .unwrap();
        assert_eq!(
            registry.all().next().unwrap().id().as_str(),
            "coinbase-main"
        );
    }

    fn binance_config(enabled: bool) -> VenueConfig {
        VenueConfig {
            id: "binance_us".to_string(),
            kind: "binance_us".to_string(),
            enabled,
            base_url: None,
            data_url: None,
            symbols: vec!["BTC/USD".to_string()],
            fee_pct: rust_decimal_macros::dec!(0.006),
        }
    }

    fn secrets_with_binance(present: bool) -> Secrets {
        Secrets {
            binance_us_api_key: present.then(|| crate::config::SecretString::from("key")),
            binance_us_secret_key: present.then(|| crate::config::SecretString::from("secret")),
            ..secrets_with_alpaca(false)
        }
    }

    #[test]
    fn a_binance_us_venue_with_credentials_is_built() {
        let registry = build_registry(
            &live_config_with(vec![binance_config(true)]),
            &secrets_with_binance(true),
            None,
        )
        .unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.all().next().unwrap().id().as_str(), "binance_us");
    }

    /// Binance.US has **no testnet** — `testnet.binance.vision` belongs to
    /// global Binance, a different exchange that blocks US persons. So there
    /// is no host to swap and, as with Coinbase, refusing to build it is the
    /// only thing between a paper window and real money.
    #[test]
    fn binance_us_is_refused_outright_in_paper_mode() {
        let mut config = config_with(vec![binance_config(true)]);
        config.agent.mode = AgentMode::Paper;
        let (registry, skipped) =
            build_registry_reporting(&config, &secrets_with_binance(true), None);

        assert!(registry.is_empty());
        assert_eq!(skipped.len(), 1);
        assert!(
            skipped[0].reason.contains("real money") && skipped[0].reason.contains("Paper"),
            "the reason must say why, and name the mode: {}",
            skipped[0].reason
        );
    }

    #[test]
    fn binance_us_is_refused_in_backtest_mode_too() {
        let mut config = config_with(vec![binance_config(true)]);
        config.agent.mode = AgentMode::Backtest;
        let (registry, _) = build_registry_reporting(&config, &secrets_with_binance(true), None);
        assert!(registry.is_empty());
    }

    #[test]
    fn binance_us_without_credentials_is_skipped_with_a_reason_naming_them() {
        let (registry, skipped) = build_registry_reporting(
            &live_config_with(vec![binance_config(true)]),
            &secrets_with_binance(false),
            None,
        );
        assert!(registry.is_empty());
        assert!(
            skipped[0].reason.contains("BINANCE_US_API_KEY"),
            "the reason must name the variables to set: {}",
            skipped[0].reason
        );
    }

    /// Every per-venue lookup keys on the id the venue reports, so a venue
    /// configured as anything else silently trades no symbols and pays the
    /// default fee.
    #[test]
    fn a_binance_us_venue_reports_its_configured_id() {
        let mut cfg = binance_config(true);
        cfg.id = "binance-main".to_string();
        let registry = build_registry(
            &live_config_with(vec![cfg]),
            &secrets_with_binance(true),
            None,
        )
        .unwrap();
        assert_eq!(registry.all().next().unwrap().id().as_str(), "binance-main");
    }

    /// A `kind` is a one-character mistake away from taking the venue with it.
    #[test]
    fn the_binance_us_kind_is_matched_in_the_spellings_operators_write() {
        for kind in ["binance_us", "BINANCE_US", "binanceus", "binance.us"] {
            let mut cfg = binance_config(true);
            cfg.kind = kind.to_string();
            let registry = build_registry(
                &live_config_with(vec![cfg]),
                &secrets_with_binance(true),
                None,
            )
            .unwrap();
            assert_eq!(registry.len(), 1, "kind {kind:?} should build");
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

    /// Every loss limit is measured against equity summed across the
    /// registry, and this venue cannot report one — so enabling it made that
    /// sum unknowable and halted the agent `UntilResume` on every cycle,
    /// whatever the other venues said.
    #[tokio::test]
    async fn a_polymarket_venue_is_refused_because_it_cannot_report_equity() {
        let mut venue = alpaca_config(true);
        venue.kind = "polymarket".to_string();
        venue.id = "polymarket".to_string();

        let config = config_with(vec![venue]);
        let client = std::sync::Arc::new(
            crate::market::polymarket::PolymarketClient::new(
                std::sync::Arc::new(config.clone()),
                &Secrets::default(),
            )
            .await
            .expect("a paper client needs no key"),
        );
        let (registry, skipped) =
            build_registry_reporting(&config, &secrets_with_alpaca(false), Some(client));

        assert!(registry.is_empty(), "it would halt the agent every cycle");
        assert_eq!(skipped.len(), 1);
        assert!(
            skipped[0].reason.contains("equity"),
            "the reason must name why: {}",
            skipped[0].reason
        );
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
