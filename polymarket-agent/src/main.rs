use anyhow::Result;
use chrono::Utc;
use clap::Parser;

use polymarket_agent::agent::lifecycle::Agent;
use polymarket_agent::config::{self, AgentMode, AppConfig, ExposeSecret};
use polymarket_agent::db::store::Store;
use polymarket_agent::monitoring;
use polymarket_agent::monitoring::alerts::{AlertLevel, AnomalyKind};
use polymarket_agent::monitoring::dashboard::{spawn_dashboard, DashboardState};
use polymarket_agent::monitoring::logger;

/// Exit (so the supervisor restarts us) after this many back-to-back cycle failures.
const MAX_CONSECUTIVE_CYCLE_FAILURES: u32 = 5;

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
    if let Some(mode) = args.mode.clone() {
        config.agent.mode = mode.into();
    }

    // Held for the lifetime of the process; `shutdown` flushes pending spans.
    // Initialised before the dry-run branch on purpose: validating
    // connectivity is exactly when a trace of what was attempted, and how long
    // each call took, is worth having.
    let telemetry = logger::init_logging(&config.monitoring, &config.telemetry)?;

    // One flush, on the single path every mode returns through.
    //
    // Dropping the handle is not a substitute: the SDK only shuts a provider
    // down when its *last* reference goes, and `tracing_opentelemetry`'s
    // layer holds one inside the globally-installed subscriber for the life
    // of the process. So the drop is a no-op, and any `?` that skipped an
    // explicit flush — a failed store open, a dashboard port already in use,
    // a backtest shorter than the batch interval — exported nothing at all.
    // Those are exactly the runs whose traces are worth having.
    let result = dispatch(args, config, secrets).await;
    telemetry.shutdown().await;
    result
}

/// Run whichever mode was selected. Separated from `main` only so that every
/// return path passes back through the flush above.
async fn dispatch(args: CliArgs, config: AppConfig, secrets: config::Secrets) -> Result<()> {
    if args.dry_run {
        return run_dry_run(&config, &secrets).await;
    }

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
    //
    // Collected rather than returned. The shipped paper template points at
    // `/var/lib/polymarket-agent`, which a normal user cannot create — so
    // the very first `--dry-run` an operator runs died here, with SQLite's
    // "unable to open database file" and nothing about which path or what to
    // do, before reaching any of the checks they ran it for. One run should
    // surface everything that is wrong, not the first thing.
    println!("2. Database:");
    println!("   Path: {}", config.database.path);
    let mut failures: Vec<String> = Vec::new();
    match Store::new(&config.database.path).await {
        Ok(store) => {
            let cycles = store.get_cycle_count().await.unwrap_or(0);
            println!("   Previous cycles: {cycles}");
            println!("   ✅ Database connected");
        }
        Err(e) => {
            println!("   ❌ {e:#}");
            println!(
                "      create the directory and make it writable, or point \
                 database.path\n      somewhere you can write"
            );
            failures.push(format!("database {}: {e}", config.database.path));
        }
    }
    println!();

    // 3. Check API keys
    println!("3. API Keys:");
    let llm_ok = secrets.llm_api_key.is_some();
    let poly_ok = secrets.polymarket_private_key.is_some();
    println!(
        "   LLM ({:?} / {}): {}",
        config.valuation.provider,
        config.valuation.model,
        if llm_ok { "✅ Set" } else { "❌ Missing" }
    );
    // Only demanded when this deployment can actually reach Polymarket. A
    // venue-based agent never touches it — and returning early here skipped
    // the venue preflight entirely, which is the check that matters most
    // before the first real dollar.
    let poly_needed = config.uses_polymarket();
    println!(
        "   Polymarket Private Key: {}",
        match (poly_ok, poly_needed) {
            (true, _) => "✅ Set",
            (false, true) => "⚠️  Missing (required for live mode)",
            (false, false) => "— not needed (no Polymarket venue is enabled)",
        }
    );
    if config.agent.mode == AgentMode::Live && !poly_ok && poly_needed {
        println!("   ❌ ERROR: POLYMARKET_PRIVATE_KEY required for live mode");
        return Err(anyhow::anyhow!("Missing required API key for live mode"));
    }
    println!();

    // 4. Test the valuation model end to end — wrong provider/base_url/model
    // combinations otherwise only surface mid-cycle, after a scan has run.
    // Failures are recorded and reported at the summary rather than returned
    // here, so one bad setting doesn't hide the remaining checks.
    println!("4. Valuation Model:");
    match &secrets.llm_api_key {
        Some(key) => {
            // Probe against a throwaway in-memory store: the real one would
            // record this call in api_costs and eat into the daily budget.
            let probe_store = Store::new(":memory:").await?;
            match polymarket_agent::valuation::llm::LlmClient::new(
                key.expose_secret().to_string(),
                &config.valuation,
                probe_store,
            )
            .map(|c| c.with_content_export(config.telemetry.exports_content()))
            {
                Ok(client) => {
                    println!("   Provider: {:?}", config.valuation.provider);
                    println!("   Model: {}", config.valuation.model);
                    match client
                        .complete("Reply with exactly: OK", "Reply with exactly: OK", None)
                        .await
                    {
                        Ok(resp) => {
                            println!(
                                "   Reply: {:?} ({} in / {} out tokens)",
                                resp.text.trim(),
                                resp.input_tokens,
                                resp.output_tokens
                            );
                            println!("   Cost of this call: ${}", resp.cost);
                            println!(
                                "   Estimated per-valuation cost: ${}",
                                client.estimated_call_cost()
                            );
                            println!("   ✅ Valuation model reachable");
                        }
                        Err(e) => {
                            println!("   ❌ Call failed: {e}");
                            failures.push(format!("valuation model call failed: {e}"));
                        }
                    }
                }
                Err(e) => {
                    println!("   ❌ Misconfigured: {e}");
                    failures.push(format!("valuation model misconfigured: {e}"));
                }
            }
        }
        None => println!("   ⚠️  No LLM_API_KEY/ANTHROPIC_API_KEY — valuations disabled"),
    }
    println!();

    // 5. The venues the agent will actually trade
    //
    // The check the dry run existed to provide and did not: it validated
    // Polymarket and nothing else, so an Alpaca deployment — which is what
    // the safety gate is for — learned nothing here.
    println!("5. Venues:");
    // These two are reported separately below — "configured but skipped" and
    // "no venues at all" are different things to tell an operator. Whether
    // Polymarket is *needed* is `config.uses_polymarket()`, asked once and
    // shared with the key check above and with `Agent::new`: two copies of
    // one predicate is two answers waiting to disagree, and the disagreement
    // decides whether a live credential is demanded.
    let polymarket_configured = config
        .venues
        .iter()
        .any(|v| v.enabled && v.kind.eq_ignore_ascii_case("polymarket"));
    let legacy_only = config.venues.iter().all(|v| !v.enabled);

    // Built only when something needs it. Constructing it unconditionally put
    // a fatal `?` — an authenticating network call in live mode — ahead of
    // the venue section, so for a US operator with an unreachable CLOB the
    // run still died before reporting anything about Alpaca. That was the
    // bug, moved one call earlier rather than fixed.
    let polymarket = if poly_needed {
        match polymarket_agent::market::polymarket::PolymarketClient::new(
            std::sync::Arc::new(config.clone()),
            secrets,
        )
        .await
        {
            Ok(c) => Some(std::sync::Arc::new(c)),
            Err(e) => {
                println!("   Polymarket client: ❌ {e:#}");
                failures.push(format!("Polymarket client: {e:#}"));
                None
            }
        }
    } else {
        None
    };

    let (registry, skipped) = polymarket_agent::venue::factory::build_registry_reporting(
        config,
        secrets,
        polymarket.clone(),
    );

    if legacy_only {
        println!("   ⚠️  No [[venues]] configured — the agent falls back to the");
        println!("      legacy Polymarket-only loop, which the safety gate does");
        println!("      not cover. See 'Starting the paper window' in the README.");
    }

    // A skipped venue is enabled in the config but absent from the registry,
    // for one of three unrelated reasons. Reporting "missing credentials" for
    // all of them sends an operator who mistyped a `kind` to the wrong file.
    for s in &skipped {
        println!("   {}:", s.id);
        println!("      ❌ enabled but not built — {}", s.reason);
        failures.push(format!("{}: {}", s.id, s.reason));
    }

    for venue in registry.all() {
        let id = venue.id().to_string();
        let report = polymarket_agent::venue::preflight::check(
            venue,
            &config.venue_symbols_for(&id),
            // The real scan limit, not a sample size — this is what makes a
            // `max_markets` below the universe visible here rather than as a
            // fortnight of empty cycles.
            config.scanning.max_markets,
            Utc::now(),
        )
        .await;

        println!("   {}:", report.venue_id);
        for (label, check) in &report.checks {
            println!("      {label}: {} {}", check.mark(), check.message());
        }
        for failure in report.failures() {
            failures.push(format!("{}: {}", report.venue_id, failure.message()));
        }
    }
    println!();

    // 6. Polymarket, only when it is not already covered above
    //
    // Reported, never enforced: its international CLOB prohibits US persons,
    // so for many operators an unreachable Gamma is the expected state and
    // must not fail a dry run about an Alpaca deployment.
    println!("6. Polymarket:");
    match (&polymarket, polymarket_configured) {
        // Already checked as a venue; asking again is two more round trips
        // against a rate-limited API and a second failure line for one cause.
        (Some(_), true) => println!("   Checked above as a venue."),
        (Some(client), false) => {
            let filters = polymarket_agent::market::polymarket::MarketFilters {
                min_volume_24h: config.scanning.min_volume_24h,
                max_resolution_days: config.scanning.max_resolution_days,
                max_markets: 10,
                max_spread_pct: config.scanning.max_spread_pct,
            };
            match client.get_markets(&filters).await {
                Ok(markets) => println!("   Gamma API: ✅ {} markets", markets.len()),
                Err(e) => println!("   Gamma API: ⚠️  {e:#}"),
            }
            match client.get_balance().await {
                Ok(balance) => println!("   Balance: ${balance}"),
                Err(e) => println!("   Balance: ⚠️  {e:#}"),
            }
        }
        (None, _) => println!("   Skipped — not an enabled venue."),
    }
    println!();

    // 7. Summary
    println!("=== Dry Run Summary ===");
    if !failures.is_empty() {
        println!("❌ {} check(s) failed:", failures.len());
        for f in &failures {
            println!("   - {f}");
        }
        return Err(anyhow::anyhow!("Dry run failed: {}", failures.join("; ")));
    }
    println!("All systems operational. The agent is ready to run.");
    println!();
    println!("Next steps:");
    println!("  Paper mode:  cargo run -- --mode paper");
    println!("  Live mode:   cargo run -- --mode live");
    println!("  Backtest:    cargo run -- --mode backtest");
    println!();
    println!("⚠️  Run paper trading for at least two weeks before going live.");

    Ok(())
}

/// Run the agent in paper or live trading mode.
async fn run_agent(config: AppConfig, secrets: config::Secrets) -> Result<()> {
    // Create shared database store
    config.database.warn_if_relative();
    let store = Store::new(&config.database.path).await?;
    // Shared pool, not a second connection: opening the same file twice means
    // two WAL writers and two migration runs.
    let agent_pool = store.pool().clone();

    // Create health state and dashboard
    let health_state = monitoring::health::HealthState::new();
    let dashboard_store = Store::from_pool(store.pool().clone());
    // One flag, shared by the dashboard's `/api/halt`, the `HALT` file poll,
    // the SIGUSR1 handler and the agent's own breakers. Built here because
    // the dashboard comes up before the agent does.
    let kill_switch = std::sync::Arc::new(polymarket_agent::agent::kill_switch::KillSwitch::new(
        config.database.halt_file(),
    ));

    let dashboard_state = DashboardState::new(
        dashboard_store,
        health_state.clone(),
        config.agent.initial_paper_balance,
        secrets
            .dashboard_token
            .as_ref()
            .map(|t| t.expose_secret().to_string()),
        kill_switch.clone(),
        config.risk.clone(),
        config.agent.mode,
    );
    let dashboard_handle = spawn_dashboard(
        dashboard_state,
        &config.monitoring.dashboard_bind,
        config.monitoring.dashboard_port,
        config.agent.mode,
    )?;

    let mut agent = Agent::new(config.clone(), secrets, store, kill_switch.clone()).await?;
    let signal_halt = spawn_signal_halt(kill_switch.clone());
    let halt_poller = spawn_halt_file_poller(kill_switch.clone());
    let mut halt_trips = kill_switch.subscribe();

    // Hourly snapshots. Skipped for an in-memory database, which has no file
    // to vacuum and no reason to want one.
    let backups = if config.database.path == ":memory:" {
        None
    } else {
        let dir = config.database.backup_dir();
        tracing::info!(dir = %dir.display(), "Hourly database snapshots enabled");
        Some(polymarket_agent::db::backup::spawn(
            Store::from_pool(agent_pool),
            dir,
        ))
    };
    let alerts = agent.alerts();
    let watchdog = spawn_cycle_watchdog(
        health_state.clone(),
        alerts.clone(),
        std::time::Duration::from_secs(config.agent.cycle_interval_seconds),
    );
    let mut shutdown = ShutdownSignals::new()?;
    let mut consecutive_failures: u32 = 0;
    let mut fatal: Option<anyhow::Error> = None;

    // Arm the watchdog before the first cycle, not after it. The loop sets a
    // due time each time it goes to sleep, which leaves the very first cycle
    // unwatched — and that is the one most likely to hang, because it is the
    // one that first touches an unreachable venue or a misconfigured endpoint.
    health_state
        .expect_cycle_by(
            Utc::now() + chrono::Duration::seconds(config.agent.cycle_interval_seconds as i64),
        )
        .await;

    loop {
        // Run the cycle to completion — it is never raced against the shutdown
        // signal. A live order placement must not be abandoned partway
        // through just because SIGTERM arrived; if a cycle genuinely hangs,
        // systemd's TimeoutStopSec (see deploy/polymarket-agent.service) is
        // the backstop that forces an exit.
        // Consumed before the cycle, so a trip raised while it runs is still
        // pending when the loop reaches its sleep.
        halt_trips.borrow_and_update();

        match agent.run_cycle().await {
            Ok(()) => {
                consecutive_failures = 0;
                health_state
                    .record_cycle(agent.cycle_number(), agent.current_state())
                    .await;
                health_state.record_anomalies(alerts.anomaly_counts()).await;

                if agent.is_dead() {
                    tracing::error!("Agent has died. Shutting down.");
                    break;
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                health_state.record_failure().await;
                health_state.record_anomalies(alerts.anomaly_counts()).await;
                tracing::error!(
                    error = %e,
                    consecutive_failures,
                    "Cycle failed — retrying after the normal interval"
                );
                let _ = alerts
                    .anomaly(
                        AlertLevel::Critical,
                        AnomalyKind::VenueUnreachable,
                        "cycle",
                        &format!("{consecutive_failures} consecutive cycle failures: {e}"),
                    )
                    .await;
                health_state
                    .record_alert_delivery(!alerts.delivery_failing())
                    .await;

                if consecutive_failures >= MAX_CONSECUTIVE_CYCLE_FAILURES {
                    fatal = Some(e.context(format!(
                        "{consecutive_failures} consecutive cycle failures — exiting so the supervisor can restart"
                    )));
                    break;
                }
            }
        }

        // Idle until there is something to do, but wake immediately on a
        // shutdown signal — this is the only point where a signal can
        // interrupt the loop. The wake is computed *after* the cycle, not
        // before it, so a cycle that ran long doesn't sleep on a stale plan.
        let now = Utc::now();
        let plan = agent.next_wake(now);
        let sleep_for = plan.sleep_from(now);
        // Tell the watchdog when this cycle is next due, so "late" is measured
        // against the schedule the agent actually chose rather than against a
        // fixed interval it is no longer following.
        health_state.expect_cycle_by(plan.at).await;
        tracing::debug!(
            reason = plan.reason.as_str(),
            wake_at = %plan.at,
            sleep_s = sleep_for.as_secs(),
            "Idling until the next wake"
        );

        // A halt raised *during* the cycle — by the file poller, a signal or
        // the dashboard — must not be marked seen here. The version was
        // consumed before `run_cycle` (below, at the top of the loop), so
        // anything that arrived since is still pending and `changed()` will
        // resolve immediately.
        //
        // Belt and braces: if the switch is holding work the loop has not
        // acted on, do not sleep at all. Marking the version seen after the
        // cycle — which is what this used to do — silently consumed a trip
        // raised in the window between the cycle's last halt check and the
        // sleep, and the agent then idled for up to `max_sleep_seconds` with
        // resting orders still working. That is exactly the latency the watch
        // channel was added to remove.
        if kill_switch.has_unhandled() {
            tracing::warn!("A halt arrived during the cycle — not sleeping on it");
            continue;
        }

        tokio::select! {
            _ = tokio::time::sleep(sleep_for) => {}
            // A halt raised while the loop is idle cuts the sleep short, so
            // the agent cancels its resting orders now rather than at the
            // next scheduled wake — which, across a closed weekend, is an
            // hour away.
            _ = halt_trips.changed() => {
                tracing::warn!("Halted while idle — waking to cancel resting orders");
            }
            signal = shutdown.recv() => {
                tracing::info!(signal, "Shutdown signal received — stopping after the current cycle");
                break;
            }
        }
    }

    // Pull everything off the book before the process goes away.
    //
    // Gate item 1: no orphaned live orders on stop or crash. Without this a
    // `systemctl stop` — a deploy, a reboot, an operator tidying up — left
    // resting orders working at the venue with nothing polling them. They can
    // still fill, and the position they open belongs to nobody until the
    // agent comes back and reconciles it.
    //
    // Runs on every exit path, including the fatal one: a run that ended in
    // five consecutive cycle failures is exactly when the book should not be
    // left unattended.
    agent.cancel_resting_orders("shutdown").await;

    // Clean up background tasks
    watchdog.abort();
    dashboard_handle.abort();
    signal_halt.abort();
    halt_poller.abort();
    if let Some(backups) = backups {
        backups.abort();
    }
    // Spans are flushed by `main`, on the one path every mode returns
    // through — including the `?` returns above this line.
    tracing::info!("Agent shutdown complete");

    match fatal {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// How far past its due time a cycle has to be before it counts as stalled.
/// Generous relative to the cadence, because a cycle legitimately takes as
/// long as its market scan and valuation calls do.
fn stall_grace(cycle_interval: std::time::Duration) -> chrono::Duration {
    let grace = (cycle_interval * 2).max(std::time::Duration::from_secs(300));
    chrono::Duration::from_std(grace).unwrap_or_else(|_| chrono::Duration::minutes(5))
}

/// Watch for a cycle that never finishes.
///
/// This has to live outside the loop. `run_cycle` is awaited to completion and
/// is never raced against anything, so a cycle that hangs simply never returns
/// — the loop cannot notice its own stall, and the health endpoint would sit
/// there reporting the last good cycle forever.
fn spawn_cycle_watchdog(
    health: monitoring::health::HealthState,
    alerts: std::sync::Arc<monitoring::alerts::AlertClient>,
    cycle_interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    let grace = stall_grace(cycle_interval);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        // The first tick completes immediately; skip it so a freshly started
        // agent is not reported late before it has run anything.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(late) = health.overdue_by(Utc::now()).await else {
                continue;
            };
            if late < grace {
                continue;
            }
            health.record_failure().await;
            let _ = alerts
                .anomaly(
                    AlertLevel::Critical,
                    AnomalyKind::StalledCycle,
                    "",
                    &format!(
                        "No cycle has completed for {}s past its scheduled wake",
                        late.num_seconds()
                    ),
                )
                .await;
            health
                .record_alert_delivery(!alerts.delivery_failing())
                .await;
        }
    })
}

/// How often the `HALT` file is checked.
///
/// Short, because this is the route that works when nothing else does — no
/// dashboard token, no PID, just a file an operator can touch over SSH. It is
/// a `stat` on one path; five seconds of it costs nothing and turns the
/// worst-case latency of the kill switch from an hour into five seconds.
const HALT_FILE_POLL: std::time::Duration = std::time::Duration::from_secs(5);

/// Watch the `HALT` file.
///
/// Separate from the cycle for the same reason the cycle watchdog is: the
/// loop cannot notice anything while it is asleep, and it may legitimately
/// sleep for `max_sleep_seconds` across a closed weekend. Tripping the switch
/// here also wakes the loop, so the agent acts on the halt rather than only
/// recording it.
fn spawn_halt_file_poller(
    kill_switch: std::sync::Arc<polymarket_agent::agent::kill_switch::KillSwitch>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(HALT_FILE_POLL);
        loop {
            ticker.tick().await;
            // Both directions: creating the file halts, removing it resumes.
            // The agent loop does the rest — cancelling resting orders,
            // alerting, writing the row that survives a restart.
            match kill_switch.sync_with_file(Utc::now()) {
                polymarket_agent::agent::kill_switch::FileSync::Raised(halt) => {
                    tracing::warn!(
                        detail = %halt.detail,
                        "HALT file present — halting new positions"
                    );
                }
                // The persisted row is cleared by the agent loop's own
                // `sync_halts`, which has the store; this task only has the
                // flag. Logged here so the two are not confused.
                polymarket_agent::agent::kill_switch::FileSync::Resumed => {
                    tracing::info!("HALT file removed — resuming on the next wake");
                }
                polymarket_agent::agent::kill_switch::FileSync::Unchanged => {}
            }
        }
    })
}

/// Halt on SIGUSR1.
///
/// The third way in, and the one that needs least: no dashboard token, no
/// filesystem path, just a PID. `kill -USR1 $(pidof polymarket-agent)` stops
/// the agent opening positions while leaving it running to manage what it
/// already holds — which is the distinction that makes this worth having
/// separately from SIGTERM.
///
/// Sets the flag only. The agent loop does the rest on its next wake, so this
/// handler cannot race a cycle that is midway through placing an order.
#[cfg(unix)]
fn spawn_signal_halt(
    kill_switch: std::sync::Arc<polymarket_agent::agent::kill_switch::KillSwitch>,
) -> tokio::task::JoinHandle<()> {
    use polymarket_agent::agent::kill_switch::{Halt, HaltSource};
    use polymarket_agent::risk::circuit_breaker::HaltScope;

    tokio::spawn(async move {
        let mut stream = match tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::user_defined1(),
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "Could not listen for SIGUSR1 — halt by file or API only");
                return;
            }
        };
        loop {
            stream.recv().await;
            let newly = kill_switch.trip(Halt::new(
                HaltSource::Signal,
                HaltScope::UntilResume,
                "SIGUSR1 received",
                Utc::now(),
            ));
            tracing::warn!(
                newly,
                "SIGUSR1 — halting new positions (exits and reconciliation continue)"
            );
        }
    })
}

#[cfg(not(unix))]
fn spawn_signal_halt(
    _kill_switch: std::sync::Arc<polymarket_agent::agent::kill_switch::KillSwitch>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {})
}

/// Persistent OS signal streams so a SIGINT/SIGTERM is never missed, even while
/// the loop is idle between cycles. `systemctl stop` sends SIGTERM, which the
/// previous Ctrl+C-only handler ignored.
struct ShutdownSignals {
    // SIGINT is covered by the cross-platform tokio::signal::ctrl_c(); only
    // SIGTERM needs a unix-specific stream.
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl ShutdownSignals {
    fn new() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Ok(Self {
                terminate: signal(SignalKind::terminate())?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    /// Resolves with the name of the signal that was received.
    async fn recv(&mut self) -> &'static str {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => "SIGINT",
                _ = self.terminate.recv() => "SIGTERM",
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            "Ctrl+C"
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stall_grace_scales_with_the_cadence_but_has_a_floor() {
        // A short cadence must not make the watchdog trigger-happy: a single
        // cycle can legitimately outlast several intervals while it waits on
        // market data and valuation calls.
        assert_eq!(
            stall_grace(std::time::Duration::from_secs(60)),
            chrono::Duration::minutes(5),
            "the floor applies at a one-minute cadence"
        );
        assert_eq!(
            stall_grace(std::time::Duration::from_secs(600)),
            chrono::Duration::minutes(20),
            "a ten-minute cadence gets twice the cadence"
        );
        assert_eq!(
            stall_grace(std::time::Duration::ZERO),
            chrono::Duration::minutes(5),
            "a zero cadence still gets the floor, not zero"
        );
    }
}
