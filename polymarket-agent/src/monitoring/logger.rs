use anyhow::Result;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::config::{MonitoringConfig, TelemetryConfig};
use crate::monitoring::telemetry::{self, Telemetry};

/// Install the log subscriber, and the OTLP span exporter when one is
/// configured.
///
/// Returns a handle whose `shutdown` flushes pending spans. Dropping it
/// without flushing loses the tail of the trace — which on a crash is the part
/// worth having.
pub fn init_logging(
    config: &MonitoringConfig,
    telemetry_config: &TelemetryConfig,
) -> Result<Telemetry> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_target(true)
        .with_file(true)
        .with_line_number(true);

    let (otel_layer, handle) = telemetry::layer(telemetry_config)?;

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    // Only now is there a subscriber to log into.
    handle.announce();

    Ok(handle)
}
