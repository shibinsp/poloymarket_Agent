use anyhow::Result;
use tracing_subscriber::EnvFilter;

use crate::config::MonitoringConfig;

pub fn init_logging(config: &MonitoringConfig) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .init();

    Ok(())
}
