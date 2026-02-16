use anyhow::Result;

use polymarket_agent::agent::lifecycle::Agent;
use polymarket_agent::config::{self, AgentMode, AppConfig};
use polymarket_agent::db::store::Store;
use polymarket_agent::monitoring;
use polymarket_agent::monitoring::dashboard::{DashboardState, spawn_dashboard};
use polymarket_agent::monitoring::logger;

#[tokio::main]
async fn main() -> Result<()> {
    let (config, secrets) = AppConfig::load()?;

    logger::init_logging(&config.monitoring)?;

    tracing::info!(
        mode = ?config.agent.mode,
        cycle_interval_s = config.agent.cycle_interval_seconds,
        "Polymarket Agent starting"
    );

    match config.agent.mode {
        AgentMode::Backtest => run_backtest(&config),
        AgentMode::Paper | AgentMode::Live => run_agent(config, secrets).await,
    }
}

/// Run the agent in paper or live trading mode.
async fn run_agent(config: AppConfig, secrets: config::Secrets) -> Result<()> {
    // Create shared database store
    let store = Store::new(&config.database.path).await?;

    // Create health state and dashboard
    let health_state = monitoring::health::HealthState::new();
    let dashboard_store = Store::from_pool(store.pool().clone());
    let dashboard_state = DashboardState::new(
        dashboard_store,
        health_state.clone(),
        config.agent.initial_paper_balance,
    );
    let dashboard_handle = spawn_dashboard(
        dashboard_state,
        &config.monitoring.dashboard_bind,
        config.monitoring.dashboard_port,
    );

    let mut agent = Agent::new(config.clone(), secrets, store).await?;
    let interval = std::time::Duration::from_secs(config.agent.cycle_interval_seconds);

    loop {
        tokio::select! {
            result = agent.run_cycle() => {
                result?;

                // Update health state
                health_state.record_cycle(agent.cycle_number(), agent.current_state());

                if agent.is_dead() {
                    tracing::error!("Agent has died. Shutting down.");
                    break;
                }

                tokio::time::sleep(interval).await;
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received Ctrl+C — shutting down gracefully");
                break;
            }
        }
    }

    // Clean up dashboard server
    dashboard_handle.abort();
    tracing::info!("Agent shutdown complete");

    Ok(())
}

/// Run a backtest using historical or synthetic data.
fn run_backtest(config: &AppConfig) -> Result<()> {
    use polymarket_agent::backtesting::engine::{self, BacktestConfig};
    use polymarket_agent::backtesting::historical;
    use std::path::Path;

    let bt_config = BacktestConfig::from_app_config(config);

    // Check for a historical data file, otherwise use synthetic data
    let data_path = Path::new("data/backtest.csv");
    let snapshots = if data_path.exists() {
        tracing::info!(path = %data_path.display(), "Loading historical data from CSV");
        historical::load_from_csv(data_path)?
    } else {
        let count = 500;
        tracing::info!(
            count,
            "No historical data found — generating synthetic data"
        );
        historical::generate_synthetic(count)
    };

    tracing::info!(snapshots = snapshots.len(), "Starting backtest");

    let results = engine::run_backtest(&snapshots, &bt_config);

    // Print results to stdout
    println!("\n{results}");

    if results.total_trades >= 500 {
        tracing::info!("Backtest completed with 500+ trades — ready for paper trading");
    } else {
        tracing::warn!(
            trades = results.total_trades,
            "Backtest completed with fewer than 500 trades — consider more data"
        );
    }

    Ok(())
}
