//! OpenTelemetry tracing, exported over OTLP.
//!
//! The agent already emits structured logs, which tell you *what* happened but
//! not how long it took or what it was nested inside. A cycle that takes 202
//! seconds because a venue is retrying behind a blocked DNS entry looks, in the
//! logs, like three unrelated warnings. As a trace it is one span with three
//! children and an obvious culprit.
//!
//! The exporter is off unless an endpoint is configured, so a deployment that
//! does not want this pays nothing.
//!
//! **Langfuse.** Its OTLP ingest accepts the same spans, and the valuation
//! calls carry GenAI semantic-convention attributes so it renders them as
//! generations with model, token counts and cost rather than as anonymous
//! spans. Point `otlp_endpoint` at a self-hosted Langfuse
//! (`http://localhost:3000/api/public/otel`) or at any collector
//! (`http://localhost:4318`) — both speak OTLP/HTTP protobuf.

use anyhow::{Context, Result};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::trace::TracerProvider;
use opentelemetry_sdk::Resource;
use std::collections::HashMap;
use tracing::Subscriber;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::config::TelemetryConfig;

/// Keeps the provider alive so spans can be flushed at shutdown. Dropping it
/// without flushing loses whatever has not been exported yet, which is
/// precisely the tail of a crash — the part worth having.
pub struct Telemetry {
    provider: Option<TracerProvider>,
    /// Where spans are going, for a confirmation line the operator can see.
    endpoint: Option<String>,
}

impl Telemetry {
    pub fn disabled() -> Self {
        Self {
            provider: None,
            endpoint: None,
        }
    }

    /// Log where spans are going.
    ///
    /// Called *after* the subscriber is installed. Saying it during
    /// construction — which is where it naturally belongs — emits into a
    /// subscriber that does not exist yet, so the one line confirming export
    /// is on was the one line nobody could see.
    pub fn announce(&self) {
        match &self.endpoint {
            Some(endpoint) => tracing::info!(
                endpoint = %endpoint,
                "OpenTelemetry span export enabled"
            ),
            None => tracing::debug!("OpenTelemetry span export is off"),
        }
    }

    /// Flush pending spans. Called on the shutdown path.
    pub fn shutdown(&self) {
        let Some(provider) = &self.provider else {
            return;
        };
        for result in provider.force_flush() {
            if let Err(e) = result {
                tracing::warn!(error = %e, "Failed to flush a span batch on shutdown");
            }
        }
    }
}

/// Build the OTLP layer, if telemetry is configured.
///
/// Returns `None` when disabled or when no endpoint is set, which is the
/// normal case for a local run.
pub fn layer<S>(config: &TelemetryConfig) -> Result<(Option<impl Layer<S>>, Telemetry)>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    if !config.enabled {
        return Ok((None, Telemetry::disabled()));
    }
    let Some(endpoint) = config.otlp_endpoint.as_deref().filter(|e| !e.is_empty()) else {
        tracing::warn!(
            "telemetry.enabled is true but no otlp_endpoint is set — tracing export is off"
        );
        return Ok((None, Telemetry::disabled()));
    };

    // Traces go to `<endpoint>/v1/traces` by convention; a caller who has
    // already spelled out the full path is taken at their word.
    let traces_endpoint = if endpoint.ends_with("/v1/traces") {
        endpoint.to_string()
    } else {
        format!("{}/v1/traces", endpoint.trim_end_matches('/'))
    };

    let mut headers = HashMap::new();
    // Langfuse authenticates its OTLP ingest with HTTP Basic over the public
    // and secret key pair; a bare collector usually needs nothing.
    if let (Some(public), Some(secret)) = (
        config.langfuse_public_key.as_deref(),
        config.langfuse_secret_key.as_deref(),
    ) {
        if !public.is_empty() && !secret.is_empty() {
            headers.insert("Authorization".to_string(), basic_auth(public, secret));
        }
    }
    for (k, v) in &config.headers {
        headers.insert(k.clone(), v.clone());
    }

    let exporter = SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(&traces_endpoint)
        .with_headers(headers)
        .with_timeout(std::time::Duration::from_secs(
            config.export_timeout_seconds,
        ))
        .build()
        .context("Failed to build the OTLP span exporter")?;

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_resource(Resource::new(vec![
            KeyValue::new("service.name", config.service_name.clone()),
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
        ]))
        .build();

    let tracer = provider.tracer("polymarket-agent");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    Ok((
        Some(layer),
        Telemetry {
            provider: Some(provider),
            endpoint: Some(traces_endpoint),
        },
    ))
}

/// `Basic base64(public:secret)`.
fn basic_auth(public: &str, secret: &str) -> String {
    use base64::Engine as _;
    let raw = format!("{public}:{secret}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_is_base64_of_the_key_pair() {
        // Langfuse rejects anything else, and a wrong header shows up as a
        // silent 401 in a background batch export.
        assert_eq!(
            basic_auth("pk-lf-1", "sk-lf-2"),
            "Basic cGstbGYtMTpzay1sZi0y"
        );
    }

    #[test]
    fn a_disabled_config_produces_no_exporter() {
        let cfg = TelemetryConfig {
            enabled: false,
            otlp_endpoint: Some("http://localhost:4318".to_string()),
            ..TelemetryConfig::default()
        };
        let (layer, _t) =
            layer::<tracing_subscriber::Registry>(&cfg).expect("disabled is not an error");
        assert!(layer.is_none());
    }

    /// Enabled but unconfigured must not fail startup — it warns and stays off.
    /// A trading agent refusing to boot because a telemetry endpoint is absent
    /// would be a worse outcome than losing the traces.
    #[test]
    fn enabled_without_an_endpoint_is_a_warning_not_an_error() {
        let cfg = TelemetryConfig {
            enabled: true,
            otlp_endpoint: None,
            ..TelemetryConfig::default()
        };
        let (layer, _t) = layer::<tracing_subscriber::Registry>(&cfg).expect("must not fail");
        assert!(layer.is_none());
    }
}
