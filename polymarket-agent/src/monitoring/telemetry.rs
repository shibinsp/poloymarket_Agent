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
//!
//! **Nothing here may stop the agent from starting.** A telemetry endpoint
//! with a typo in it is a monitoring problem; a trading agent that refuses to
//! boot is an outage. Every failure in this module degrades to "export is
//! off" plus a message the operator can actually see.

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

/// The per-signal OTLP variable. Used verbatim by the exporter and given top
/// precedence over everything else, which is what makes it the only reliable
/// way to state where spans actually go. See `effective_endpoint`.
const TRACES_ENDPOINT_VAR: &str = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT";

/// Something worth telling the operator, held until a subscriber exists.
enum Note {
    Enabled(String),
    Warning(String),
}

/// Keeps the provider alive so spans can be flushed at shutdown. Dropping it
/// without flushing loses whatever has not been exported yet, which is
/// precisely the tail of a crash — the part worth having.
pub struct Telemetry {
    provider: Option<TracerProvider>,
    /// Emitted by `announce`, not at construction: `layer()` runs *before*
    /// `.init()` installs the subscriber, so anything logged from inside it
    /// goes to the no-op dispatcher and is discarded. The one line confirming
    /// export is on, and every line explaining why it is off, were all
    /// landing in that hole.
    note: Option<Note>,
}

impl Telemetry {
    pub fn disabled() -> Self {
        Self {
            provider: None,
            note: None,
        }
    }

    fn off_with_warning(message: String) -> Self {
        Self {
            provider: None,
            note: Some(Note::Warning(message)),
        }
    }

    /// Say where spans are going, or why they are not going anywhere.
    /// Called after the subscriber is installed.
    pub fn announce(&self) {
        match &self.note {
            Some(Note::Enabled(endpoint)) => tracing::info!(
                endpoint = %endpoint,
                "OpenTelemetry span export enabled"
            ),
            Some(Note::Warning(message)) => tracing::warn!(
                reason = %message,
                "OpenTelemetry span export is off"
            ),
            None => tracing::debug!("OpenTelemetry span export is off"),
        }
    }

    /// Flush pending spans. Called on every shutdown path.
    ///
    /// Async, and the flush itself runs on a blocking thread, because
    /// `force_flush` is `futures_executor::block_on` underneath: it parks the
    /// calling thread until the batch processor's task completes. Called
    /// directly from async code it holds a Tokio worker hostage for up to the
    /// export timeout, and on a single-threaded runtime it deadlocks outright
    /// — the batch task it is waiting for can never be scheduled.
    pub async fn shutdown(&self) {
        let Some(provider) = self.provider.clone() else {
            return;
        };
        let results = tokio::task::spawn_blocking(move || provider.force_flush()).await;
        match results {
            Ok(results) => {
                for result in results {
                    if let Err(e) = result {
                        tracing::warn!(error = %e, "Failed to flush a span batch on shutdown");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "Span flush task failed"),
        }
    }
}

/// Where spans will *actually* be sent, given the exporter's own precedence.
///
/// `resolve_http_endpoint` in opentelemetry-otlp checks the per-signal
/// variable first, then `OTEL_EXPORTER_OTLP_ENDPOINT` (appending the signal
/// path), and only then the value passed to `.with_endpoint()`. So the
/// builder argument is the *last* thing consulted, and an ambient
/// `OTEL_EXPORTER_OTLP_ENDPOINT` — which cluster operators set as a matter of
/// course — silently wins. Worse, it appends `/v1/traces` unconditionally, so
/// an endpoint already spelled out in full becomes `…/v1/traces/v1/traces`
/// and every batch 404s.
///
/// Resolving the path here and pinning it to the per-signal variable makes
/// the builder argument, the announced endpoint and the real destination the
/// same string.
fn traces_path(endpoint: &str) -> String {
    if endpoint.ends_with("/v1/traces") {
        endpoint.to_string()
    } else {
        format!("{}/v1/traces", endpoint.trim_end_matches('/'))
    }
}

/// Build the OTLP layer, if telemetry is configured.
///
/// Never returns an error: a misconfigured exporter turns export off and says
/// so, rather than taking the agent down with it.
pub fn layer<S>(config: &TelemetryConfig) -> (Option<impl Layer<S>>, Telemetry)
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    if !config.enabled() {
        return (None, Telemetry::disabled());
    }
    let Some(endpoint) = config.otlp_endpoint.as_deref().filter(|e| !e.is_empty()) else {
        return (
            None,
            Telemetry::off_with_warning(
                "telemetry.enabled is true but no otlp_endpoint is set".to_string(),
            ),
        );
    };

    let traces_endpoint = traces_path(endpoint);
    // Pin it, so the exporter cannot re-derive a different one from the
    // ambient environment. See `traces_path`.
    std::env::set_var(TRACES_ENDPOINT_VAR, &traces_endpoint);

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
        .build();

    let exporter = match exporter {
        Ok(e) => e,
        Err(e) => {
            // The common way in is a `LANGFUSE_HOST` with the scheme left
            // off, which fails to parse as a URI. That must not be fatal.
            return (
                None,
                Telemetry::off_with_warning(format!(
                    "could not build the OTLP exporter for {traces_endpoint}: {e}"
                )),
            );
        }
    };

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_resource(Resource::new(vec![
            KeyValue::new("service.name", config.service_name.clone()),
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
        ]))
        .build();

    let tracer = provider.tracer("polymarket-agent");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    (
        Some(layer),
        Telemetry {
            provider: Some(provider),
            note: Some(Note::Enabled(traces_endpoint)),
        },
    )
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
    fn a_bare_collector_endpoint_gains_the_traces_path() {
        assert_eq!(
            traces_path("http://localhost:4318"),
            "http://localhost:4318/v1/traces"
        );
    }

    #[test]
    fn a_trailing_slash_does_not_produce_a_double_slash() {
        assert_eq!(
            traces_path("http://localhost:3000/api/public/otel/"),
            "http://localhost:3000/api/public/otel/v1/traces"
        );
    }

    #[test]
    fn an_endpoint_already_spelled_out_in_full_is_left_alone() {
        // The case that produced `…/v1/traces/v1/traces` and a 404 on every
        // batch. Appending is not idempotent, so it must not be repeated.
        assert_eq!(
            traces_path("http://collector:4318/v1/traces"),
            "http://collector:4318/v1/traces"
        );
    }

    #[test]
    fn a_disabled_config_produces_no_exporter() {
        let cfg = TelemetryConfig {
            enabled: Some(false),
            otlp_endpoint: Some("http://localhost:4318".to_string()),
            ..TelemetryConfig::default()
        };
        let (layer, t) = layer::<tracing_subscriber::Registry>(&cfg);
        assert!(layer.is_none());
        assert!(t.note.is_none(), "off by request is not worth a warning");
    }

    /// Enabled but unconfigured must not fail startup — it warns and stays off.
    /// A trading agent refusing to boot because a telemetry endpoint is absent
    /// would be a worse outcome than losing the traces.
    #[test]
    fn enabled_without_an_endpoint_is_a_warning_not_an_error() {
        let cfg = TelemetryConfig {
            enabled: Some(true),
            otlp_endpoint: None,
            ..TelemetryConfig::default()
        };
        let (layer, t) = layer::<tracing_subscriber::Registry>(&cfg);
        assert!(layer.is_none());
        assert!(
            matches!(t.note, Some(Note::Warning(_))),
            "the operator must be told why export is off"
        );
    }

    /// The same promise, for the case that actually reached `main` as an
    /// `Err`: a `LANGFUSE_HOST` with the scheme left off.
    #[test]
    fn an_unparseable_endpoint_is_a_warning_not_an_error() {
        let cfg = TelemetryConfig {
            enabled: Some(true),
            // No scheme. `Uri` parsing rejects it and the exporter build fails.
            otlp_endpoint: Some("localhost:3000/api/public/otel".to_string()),
            ..TelemetryConfig::default()
        };
        let (layer, t) = layer::<tracing_subscriber::Registry>(&cfg);
        assert!(
            layer.is_none(),
            "a malformed endpoint must not produce an exporter"
        );
        match t.note {
            Some(Note::Warning(m)) => assert!(
                m.contains("localhost:3000"),
                "the warning should name the endpoint it could not use, got {m:?}"
            ),
            other => panic!("expected a warning, got {}", describe(&other)),
        }
    }

    #[cfg(test)]
    fn describe(note: &Option<Note>) -> &'static str {
        match note {
            Some(Note::Enabled(_)) => "Enabled",
            Some(Note::Warning(_)) => "Warning",
            None => "None",
        }
    }
}
