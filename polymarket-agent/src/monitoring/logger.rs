use anyhow::Result;
use tracing::Subscriber;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::config::{MonitoringConfig, TelemetryConfig};
use crate::monitoring::telemetry::{self, Telemetry};

/// Spans from this crate at INFO, and nothing from dependencies.
///
/// A blanket `info` would pull in every reqwest and hyper span, which at a
/// ten-minute cadence is mostly noise and, on a paid ingest, mostly cost.
const DEFAULT_OTEL_FILTER: &str = "polymarket_agent=info";

/// The JSON log layer, built the same way wherever it is used.
///
/// **Span fields are deliberately kept out of the log lines.** The JSON
/// formatter defaults to writing the current span's fields *and* the whole
/// enclosing span list into every event. With the valuation span carrying the
/// prompt and the completion, that copied each of them into every log line
/// emitted inside the call — twice per line under the default formatter, and
/// once more for every reqwest DEBUG line underneath it. Prompts belong in the
/// trace, which is opt-in and access-controlled; the log file is neither.
///
/// Taking the writer as a parameter is what makes that testable.
fn json_layer<S, W>(writer: W) -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::fmt::layer()
        .json()
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_current_span(false)
        .with_span_list(false)
        .with_writer(writer)
}

/// Install the log subscriber, and the OTLP span exporter when one is
/// configured.
///
/// Returns a handle whose `shutdown` flushes pending spans. Failing to flush
/// loses the tail of the trace — which on a crash is the part worth having.
pub fn init_logging(
    config: &MonitoringConfig,
    telemetry_config: &TelemetryConfig,
) -> Result<Telemetry> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    let (otel_layer, handle) = telemetry::layer(telemetry_config);

    // The log level and the trace level are separate concerns.
    //
    // Attached to the registry, `EnvFilter` sits in the global-filter
    // position and suppresses span *creation* for every layer beneath it. A
    // deployment quieting the agent with `RUST_LOG=warn` would silently get
    // no spans at all, while startup still announced that export was on —
    // every span in this codebase is created at INFO. Filtering the fmt layer
    // alone leaves tracing free to keep its own level.
    let otel_filter = EnvFilter::try_from_env("OTEL_LOG_LEVEL")
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_OTEL_FILTER));

    tracing_subscriber::registry()
        .with(json_layer(std::io::stdout).with_filter(filter))
        .with(otel_layer.map(|l| l.with_filter(otel_filter)))
        .init();

    // Only now is there a subscriber to log into.
    handle.announce();

    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Emit one event inside a span carrying sensitive fields, and return
    /// everything the log layer wrote.
    fn log_output_for_a_span_with_secrets() -> String {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::registry().with(json_layer(captured.clone()));
        let _guard = tracing::subscriber::set_default(subscriber);

        let span = tracing::info_span!(
            "llm.generation",
            langfuse.observation.input = "PROMPT-a4f9-SENSITIVE",
            langfuse.observation.output = "COMPLETION-b7c2-SENSITIVE",
            api_key = "sk-ant-0123456789abcdef",
        );
        let _entered = span.enter();
        // The kind of line the HTTP stack emits from inside the call.
        tracing::info!(status = 200, "request complete");
        drop(_entered);
        drop(span);

        captured.text()
    }

    /// The regression that mattered: with `telemetry.enabled = false` and a
    /// stock client, running one model call wrote the full system prompt, the
    /// user prompt and the completion into the ordinary JSON logs — twice per
    /// event, and once more for every dependency line inside the request.
    /// A deployment that had never turned tracing on was writing its prompts
    /// to disk.
    #[test]
    fn span_fields_never_reach_the_log_lines() {
        let output = log_output_for_a_span_with_secrets();

        assert!(
            output.contains("request complete"),
            "the event itself must still be logged, got: {output}"
        );
        assert!(
            !output.contains("PROMPT-a4f9-SENSITIVE"),
            "a span field leaked into the logs: {output}"
        );
        assert!(
            !output.contains("COMPLETION-b7c2-SENSITIVE"),
            "a span field leaked into the logs: {output}"
        );
        assert!(
            !output.contains("sk-ant-"),
            "a credential-shaped span field leaked into the logs: {output}"
        );
    }

    /// The event's *own* fields are the ones a log line is for, and they must
    /// survive. Without this, a formatter that suppressed everything would
    /// also pass the test above.
    #[test]
    fn an_events_own_fields_are_still_logged() {
        let output = log_output_for_a_span_with_secrets();
        assert!(
            output.contains("\"status\":200"),
            "event fields must survive, got: {output}"
        );
    }
}
