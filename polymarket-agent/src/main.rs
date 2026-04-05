use anyhow::Result;
use clap::Parser;

use polymarket_agent::agent::lifecycle::Agent;
use polymarket_agent::config::{self, AgentMode, AppConfig};
use polymarket_agent::db::store::Store;
use polymarket_agent::monitoring;
use polymarket_agent::monitoring::dashboard::{spawn_dashboard, DashboardState};
use polymarket_agent::monitoring::logger;

/// Polymarket Autonomous Trading Agent
#[derive(Parser, Debug)]
#[command(
    name = "polymarket-agent",
    about = "Autonomous prediction market trading agent"
)]
struct CliArgs {
    /// Override agent mode from config file
    #[arg(long, value_enum)]
    mode: Option<AgentModeArg>,

    /// Run a quick validation check (single cycle, no trades)
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum AgentModeArg {
    Paper,
    Live,
    Backtest,
}

impl From<AgentModeArg> for AgentMode {
    fn from(arg: AgentModeArg) -> Self {
        match arg {
            AgentModeArg::Paper => AgentMode::Paper,
            AgentModeArg::Live => AgentMode::Live,
            AgentModeArg::Backtest => AgentMode::Backtest,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = CliArgs::parse();

    let (mut config, secrets) = AppConfig::load()?;

    // Override mode from CLI if provided
    if let Some(mode) = args.mode {
        config.agent.mode = mode.into();
    }

    // Dry run mode: single cycle validation
    if args.dry_run {
        return run_dry_run(&config, &secrets).await;
    }

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

/// Quick dry-run validation: tests connectivity and pipeline without placing trades.
async fn run_dry_run(config: &AppConfig, secrets: &config::Secrets) -> Result<()> {
    println!("=== Polymarket Agent — Dry Run Validation ===\n");

    // 1. Check config
    println!("1. Configuration:");
    println!("   Mode: {:?}", config.agent.mode);
    println!(
        "   Cycle interval: {}s",
        config.agent.cycle_interval_seconds
    );
    println!(
        "   Initial balance: ${}",
        config.agent.initial_paper_balance
    );
    println!("   Daily API budget: ${}", config.agent.daily_api_budget);
    println!(
        "   Min edge threshold: {}%",
        config.valuation.min_edge_threshold * rust_decimal_macros::dec!(100)
    );
    println!("   Kelly fraction: {}", config.risk.kelly_fraction);
    println!(
        "   Max position: {}%",
        config.risk.max_position_pct * rust_decimal_macros::dec!(100)
    );
    println!("   ✅ Configuration valid\n");

    // 2. Check database
    println!("2. Database:");
    let store = Store::new(&config.database.path).await?;
    let cycle_count = store.get_cycle_count().await?;
    println!("   Path: {}", config.database.path);
    println!("   Previous cycles: {}", cycle_count);
    println!("   ✅ Database connected\n");

    // 3. Check API keys
    println!("3. API Keys:");
    let anthropic_ok = secrets.anthropic_api_key.is_some();
    let poly_ok = secrets.polymarket_private_key.is_some();
    println!(
        "   Anthropic (Claude): {}",
        if anthropic_ok {
            "✅ Set"
        } else {
            "❌ Missing"
        }
    );
    println!(
        "   Polymarket Private Key: {}",
        if poly_ok {
            "✅ Set"
        } else {
            "⚠️  Missing (required for live mode)"
        }
    );
    if config.agent.mode == AgentMode::Live && !poly_ok {
        println!("   ❌ ERROR: POLYMARKET_PRIVATE_KEY required for live mode");
        return Err(anyhow::anyhow!("Missing required API key for live mode"));
    }
    println!();

    // 4. Test Polymarket connectivity
    println!("4. Polymarket Connectivity:");
    let config_arc = std::sync::Arc::new(config.clone());
    let polymarket =
        polymarket_agent::market::polymarket::PolymarketClient::new(config_arc.clone(), secrets)
            .await?;

    let filters = polymarket_agent::market::polymarket::MarketFilters {
        min_volume_24h: config.scanning.min_volume_24h,
        max_resolution_days: config.scanning.max_resolution_days,
        max_markets: 10,
        max_spread_pct: config.scanning.max_spread_pct,
    };

    let markets = polymarket.get_markets(&filters).await?;
    println!(
        "   Gamma API: ✅ Connected (found {} markets)",
        markets.len()
    );

    if let Some(first) = markets.first() {
        if let Some(first_token) = first.tokens.first() {
            let book = polymarket.get_order_book(&first_token.token_id).await?;
            println!(
                "   CLOB API: ✅ Connected (spread: {}%)",
                book.spread * rust_decimal_macros::dec!(100)
            );
        }
    }
    println!();

    // 5. Check balance
    println!("5. Balance:");
    let balance = polymarket.get_balance().await?;
    println!("   Current balance: ${}", balance);
    if balance <= rust_decimal_macros::dec!(0) && config.agent.mode != AgentMode::Backtest {
        println!("   ⚠️  Balance is zero — agent would be in Dead state");
    } else {
        println!("   ✅ Balance sufficient");
    }
    println!();

    // 6. Summary
    println!("=== Dry Run Summary ===");
    println!("All systems operational. The agent is ready to run.");
    println!();
    println!("Next steps:");
    println!("  Paper mode:  cargo run -- --mode paper");
    println!("  Live mode:   cargo run -- --mode live");
    println!("  Backtest:    cargo run -- --mode backtest");
    println!();
    println!("⚠️  Always run paper trading for 48-72h before going live.");

    Ok(())
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
