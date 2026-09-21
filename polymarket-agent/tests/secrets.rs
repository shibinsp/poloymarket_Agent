//! Credentials must not be reachable through any of the ordinary ways a value
//! ends up in a log file.
//!
//! Every struct here already has a hand-written `Debug` that redacts, and the
//! `Secrets` struct now holds `SecretString`. None of that is self-enforcing:
//! a `#[derive(Debug)]` added in a hurry, or a field added to an existing
//! redacting impl's `finish_non_exhaustive()`, reintroduces the leak silently
//! and passes review because the diff looks like housekeeping.
//!
//! So these tests are deliberately about the *observable* behaviour — format
//! the thing, search the output for the secret — rather than about the shape
//! of the code. They fail for the next person who breaks it, whatever way
//! they break it.

use polymarket_agent::config::{ExposeSecret, SecretString, Secrets};

/// Sentinels shaped like the real credentials, so a partial redaction that
/// leaves a prefix behind still trips the assertion.
const ANTHROPIC_KEY: &str = "sk-ant-api03-LEAKCANARY-0123456789abcdef";
const ALPACA_KEY_ID: &str = "APCA-LEAKCANARY-KEYID";
const ALPACA_SECRET: &str = "APCA-LEAKCANARY-SECRET";
const PRIVATE_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn assert_clean(what: &str, rendered: &str) {
    for canary in [ANTHROPIC_KEY, ALPACA_KEY_ID, ALPACA_SECRET, PRIVATE_KEY] {
        assert!(
            !rendered.contains(canary),
            "{what} leaked a credential: {rendered}"
        );
    }
    // The shapes named in the go-live checklist, independent of the exact
    // canaries above.
    assert!(
        !rendered.contains("sk-ant-"),
        "{what} leaked an Anthropic-shaped key: {rendered}"
    );
    assert!(
        !rendered.contains("APCA-"),
        "{what} leaked an Alpaca-shaped credential: {rendered}"
    );
}

#[test]
fn a_secret_string_reveals_nothing_when_formatted() {
    let secret = SecretString::from(ANTHROPIC_KEY);
    assert_clean("SecretString Debug", &format!("{secret:?}"));
    // And the escape hatch still works, or the type would be useless.
    assert_eq!(secret.expose_secret(), ANTHROPIC_KEY);
}

#[test]
fn a_secrets_struct_full_of_credentials_renders_none_of_them() {
    let secrets = Secrets {
        polymarket_private_key: Some(SecretString::from(PRIVATE_KEY)),
        llm_api_key: Some(SecretString::from(ANTHROPIC_KEY)),
        alpaca_key_id: Some(SecretString::from(ALPACA_KEY_ID)),
        alpaca_secret_key: Some(SecretString::from(ALPACA_SECRET)),
        ..Secrets::default()
    };
    // `Secrets` has no `Debug` of its own; the fields are what would carry a
    // credential into a log line, and each of them must be inert.
    for rendered in [
        format!("{:?}", secrets.polymarket_private_key),
        format!("{:?}", secrets.llm_api_key),
        format!("{:?}", secrets.alpaca_key_id),
        format!("{:?}", secrets.alpaca_secret_key),
    ] {
        assert_clean("Secrets field", &rendered);
    }
}

#[tokio::test]
async fn the_llm_client_redacts_its_api_key() {
    let store = polymarket_agent::db::store::Store::new(":memory:")
        .await
        .unwrap();
    let config = polymarket_agent::config::ValuationConfig {
        provider: polymarket_agent::config::LlmProvider::Anthropic,
        model: "claude-sonnet-4-20250514".to_string(),
        base_url: None,
        input_price_per_million: None,
        output_price_per_million: None,
        max_tokens: 1024,
        min_edge_threshold: rust_decimal_macros::dec!(0.08),
        high_confidence_edge: rust_decimal_macros::dec!(0.06),
        low_confidence_edge: rust_decimal_macros::dec!(0.10),
        cache_ttl_seconds: 300,
    };
    let client =
        polymarket_agent::valuation::llm::LlmClient::new(ANTHROPIC_KEY.to_string(), &config, store)
            .unwrap();

    assert_clean("LlmClient Debug", &format!("{client:?}"));
    // The redaction must not have swallowed the useful part.
    assert!(format!("{client:?}").contains("claude-sonnet-4"));
}

#[test]
fn the_alpaca_venue_config_redacts_both_credentials() {
    let config = polymarket_agent::venue::alpaca::AlpacaConfig::paper(ALPACA_KEY_ID, ALPACA_SECRET);
    assert_clean("AlpacaConfig Debug", &format!("{config:?}"));
    assert!(
        format!("{config:?}").contains("paper-api.alpaca.markets"),
        "the endpoint is not a secret and is the useful part"
    );
}

#[test]
fn the_alpaca_rest_client_redacts_both_credentials() {
    let rest = polymarket_agent::venue::alpaca::rest::AlpacaRest::new(
        "https://paper-api.alpaca.markets",
        "https://data.alpaca.markets",
        ALPACA_KEY_ID,
        ALPACA_SECRET,
        std::time::Duration::from_secs(10),
    )
    .unwrap();
    assert_clean("AlpacaRest Debug", &format!("{rest:?}"));
}

/// An error message is the other way a credential escapes: it is formatted,
/// logged, and often returned to a caller that logs it again.
#[test]
fn a_rejected_alpaca_credential_is_not_echoed_in_the_error() {
    let err = polymarket_agent::venue::alpaca::rest::AlpacaRest::new(
        "https://paper-api.alpaca.markets",
        "https://data.alpaca.markets",
        ALPACA_KEY_ID,
        // Empty secret: rejected, and the rejection must not quote what it
        // was given alongside it.
        "   ",
        std::time::Duration::from_secs(10),
    )
    .unwrap_err();
    assert_clean("AlpacaRest::new error", &format!("{err:?}"));
}
