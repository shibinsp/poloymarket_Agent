use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{error, info, instrument, warn};

use crate::agent::budget::BudgetLedger;
use crate::agent::kill_switch::{FileSync, Halt, HaltSource, KillSwitch};
use crate::agent::reconcile::Reconciler;
use crate::agent::scheduler::{self, WakePlan};
use crate::agent::self_funding::{
    self, edge_justifies_cost, enhanced_survival_check, log_cost_breakdown, CycleCosts,
};
use crate::agent::venue_cycle::{CycleOutcome, VenueCycle};
use crate::agent::venue_exits::VenueExits;
use crate::config::{AppConfig, ExposeSecret, Secrets};
use crate::data::crypto::CryptoSource;
use crate::data::news::NewsSource;
use crate::data::sports::SportsSource;
use crate::data::weather::WeatherSource;
use crate::data::{DataAggregator, DataPoint, MarketQuery};
use crate::db::store::{CycleRecord, Store};
use crate::execution::fills;
use crate::execution::order::{self, OrderStatus};
use crate::execution::reconcile::{StateReconciler, Verdict};
use crate::execution::resolution;
use crate::execution::wallet;
use crate::market::models::{apply_halt, AgentState, MarketCandidate};
use crate::market::polymarket::PolymarketClient;
use crate::market::scanner::MarketScanner;
use crate::monitoring::alerts::{check_milestone, AlertClient, AlertLevel, AnomalyKind};
use crate::monitoring::metrics::{compute_metrics, log_metrics};
use crate::risk::circuit_breaker::{self, HaltScope};
use crate::risk::kelly;
use crate::risk::limits;
use crate::risk::portfolio::{PortfolioManager, Position};
use crate::valuation::calibration;
use crate::valuation::edge::{evaluate_edge, to_opportunity, EdgeResult};
use crate::valuation::fair_value::{ValuationEngine, ValuationResult};
use crate::valuation::llm::LlmClient;
use crate::venue::VenueRegistry;

pub struct Agent {
    config: AppConfig,
    store: Store,
    state: AgentState,
    cycle_number: u64,
    /// `None` when this deployment does not use Polymarket at all — a
    /// venue-based agent never touches it, and a US operator may not legally
    /// trade it. Constructing it regardless made `Agent::new` demand a
    /// credential the run had no use for, which killed the process after
    /// `--dry-run` had already reported the config as sound.
    polymarket: Option<Arc<PolymarketClient>>,
    /// `None` alongside `polymarket`: the scanner only scans Polymarket.
    scanner: Option<MarketScanner>,
    data_aggregator: DataAggregator,
    valuation_engine: Option<ValuationEngine>,
    portfolio: PortfolioManager,
    alert_client: Arc<AlertClient>,
    last_balance: Decimal,
    /// Venues from `[[venues]]`. Empty keeps the legacy Polymarket-only path.
    venues: VenueRegistry,
    /// Consecutive cycles in which no equity figure could be established.
    unknown_equity_cycles: u32,
    /// Shared with the valuation engine; the venue cycle calls it directly
    /// because continuous assets use a different prompt.
    llm: Option<Arc<LlmClient>>,
    /// Stops new positions. Shared with the dashboard's `/api/halt` and with
    /// the SIGUSR1 handler, so all three routes end at one flag with one
    /// recorded reason.
    kill_switch: Arc<KillSwitch>,
    /// The day's valuation spend. Held here as well as inside the LLM client
    /// so a batch can be sized to what is affordable before it is spawned.
    budget: Arc<BudgetLedger>,
}

/// Scan the legacy Polymarket universe, or nothing when it is not configured.
///
/// A venue-based deployment has no Polymarket scanner, and "zero candidates"
/// is the honest answer there rather than an error: the venue path does its
/// own instrument discovery.
async fn scan_legacy(
    scanner: Option<&MarketScanner>,
) -> Result<Vec<crate::market::models::MarketCandidate>> {
    match scanner {
        Some(s) => s.scan().await,
        None => Ok(Vec::new()),
    }
}

impl Agent {
    /// `kill_switch` is created by the caller and shared with the dashboard,
    /// so `POST /api/halt` and the agent loop are the same flag rather than
    /// two that agree most of the time.
    pub async fn new(
        config: AppConfig,
        secrets: Secrets,
        store: Store,
        kill_switch: Arc<KillSwitch>,
    ) -> Result<Self> {
        let config_arc = Arc::new(config.clone());
        let polymarket = if config.uses_polymarket() {
            Some(Arc::new(PolymarketClient::new(config_arc, &secrets).await?))
        } else {
            info!("No Polymarket venue is enabled — not building a Polymarket client");
            None
        };
        let scanner = polymarket
            .as_ref()
            .map(|c| MarketScanner::new(c.clone(), config.scanning.clone()));

        // Phase 3: Initialize data sources
        let data_sources: Vec<Box<dyn crate::data::DataSource>> = vec![
            Box::new(WeatherSource::new()),
            Box::new(SportsSource::new()),
            Box::new(CryptoSource::new()),
            Box::new(NewsSource::new()),
        ];
        let data_aggregator = DataAggregator::new(data_sources);

        // The day's spend, seeded from what the database already recorded.
        // A restart mid-day must not hand the agent a fresh budget — that is
        // how "spend at most $0.50 a day" becomes "$0.50 per restart".
        let spent_today = store.get_today_api_cost().await.unwrap_or_else(|e| {
            warn!(error = %e, "Could not read today's API spend — assuming none");
            Decimal::ZERO
        });
        let budget = Arc::new(BudgetLedger::new(
            config.agent.daily_api_budget,
            Utc::now().date_naive(),
            spent_today,
        ));

        // Reinstate any halt that outlived the last process.
        match store.active_halt().await {
            Ok(Some(stored)) => {
                let halt = Halt::from_stored(&stored);
                match halt {
                    Some(halt) => {
                        // Restarting is the first thing anyone does when
                        // something looks wrong. If that cleared a drawdown
                        // halt, the breaker would be decorative.
                        warn!(
                            source = halt.source.as_str(),
                            detail = %halt.detail,
                            "Reinstating a halt that was in force before this process started"
                        );
                        kill_switch.trip(halt);
                    }
                    None => warn!(
                        source = %stored.source,
                        "Ignoring an unreadable halt row — the HALT file still applies"
                    ),
                }
            }
            Ok(None) => {}
            Err(e) => warn!(error = %e, "Could not read the halt table"),
        }

        // Phase 4: Initialize valuation engine (only if API key is available)
        let mut llm_client: Option<Arc<LlmClient>> = None;
        let valuation_engine = if let Some(ref api_key) = secrets.llm_api_key {
            // Share the caller's connection pool rather than opening (and
            // migrating) two more against the same database file.
            let llm_store = store.clone_for_parallel();
            let valuation_store = store.clone_for_parallel();
            let client = Arc::new(
                LlmClient::new(
                    api_key.expose_secret().to_string(),
                    &config.valuation,
                    llm_store,
                )?
                .with_content_export(config.telemetry.exports_content())
                .with_budget(budget.clone()),
            );
            llm_client = Some(client.clone());
            Some(ValuationEngine::new(
                client,
                config.valuation.clone(),
                valuation_store,
            ))
        } else {
            warn!("LLM_API_KEY/ANTHROPIC_API_KEY not set — valuation engine disabled");
            None
        };

        // Venues declared in [[venues]]. Sharing the Polymarket client keeps
        // one paper balance across both the legacy and venue paths.
        let venues = crate::venue::factory::build_registry(&config, &secrets, polymarket.clone())?;

        // Phase 5: Initialize portfolio manager
        let portfolio = PortfolioManager::new(config.risk.clone());

        // Phase 8: Initialize alert client
        let alert_client = Arc::new(AlertClient::new(
            secrets
                .discord_webhook_url
                .as_ref()
                .map(|u| u.expose_secret().to_string()),
            config.monitoring.discord_enabled,
        ));

        // Resume cycle number from last recorded cycle
        let cycle_number = match store.get_latest_cycle().await? {
            Some(cycle) => cycle.cycle_number as u64 + 1,
            None => 0,
        };

        info!(
            mode = ?config.agent.mode,
            cycle_number,
            valuation_enabled = valuation_engine.is_some(),
            venues = venues.len(),
            alerts_enabled = alert_client.is_enabled(),
            "Agent initialized"
        );

        Ok(Self {
            unknown_equity_cycles: 0,
            config,
            store,
            state: AgentState::Alive,
            cycle_number,
            polymarket,
            scanner,
            data_aggregator,
            valuation_engine,
            portfolio,
            alert_client,
            last_balance: Decimal::ZERO,
            venues,
            llm: llm_client,
            kill_switch,
            budget,
        })
    }

    /// The halt flag, for the dashboard and the signal handler.
    pub fn kill_switch(&self) -> Arc<KillSwitch> {
        self.kill_switch.clone()
    }

    /// Raise a halt.
    ///
    /// Only sets the flag. The side effects run in `apply_pending_halt`,
    /// which is also where halts raised from outside the loop — the
    /// dashboard, SIGUSR1, the `HALT` file — get theirs. Doing the work here
    /// would mean an `/api/halt` press cancelled nothing, which is what it
    /// used to mean.
    fn raise_halt(&self, halt: Halt) {
        self.kill_switch.trip(halt);
    }

    /// Run the one-time work for a halt, whoever raised it.
    ///
    /// Cancelling resting orders, alerting, and writing the row that survives
    /// a restart. Exactly once per halt: `take_unhandled` hands back a given
    /// halt only the first time.
    async fn apply_pending_halt(&self) {
        let Some(halt) = self.kill_switch.take_unhandled() else {
            return;
        };

        error!(
            source = halt.source.as_str(),
            scope = ?halt.scope,
            detail = %halt.detail,
            "HALTED — no new positions until this is cleared"
        );

        // Resting orders first. A halt that leaves working orders on the book
        // has not stopped anything: they can still fill, and the agent has
        // just stopped watching them closely.
        self.cancel_resting_orders("halting").await;

        if let Err(e) = self
            .store
            .insert_halt(
                halt.source.as_str(),
                match halt.scope {
                    HaltScope::RestOfDay => "rest_of_day",
                    HaltScope::UntilResume => "until_resume",
                },
                &halt.detail,
                halt.at,
            )
            .await
        {
            warn!(error = %e, "Could not persist the halt — a restart would lift it");
        }

        let _ = self
            .alert_client
            .anomaly(
                AlertLevel::Critical,
                AnomalyKind::Halted,
                halt.source.as_str(),
                &halt.detail,
            )
            .await;
    }

    /// Pull every resting order off every venue.
    ///
    /// Used on three paths that share one requirement: after this returns,
    /// nothing the agent placed may still be working at a venue that nobody
    /// is watching. Halting, dying, and being stopped all qualify — an order
    /// left resting through a `systemctl stop` can fill during a deploy, and
    /// the position it opens belongs to nobody until the process comes back.
    ///
    /// Best-effort by necessity: a venue that will not answer cannot be made
    /// to cancel. Failures are logged loudly rather than propagated, because
    /// one unreachable venue must not stop the others being cleared.
    pub async fn cancel_resting_orders(&self, why: &str) {
        self.venues.cancel_all_resting(why).await;
    }

    /// Reconcile the halt flag with the `HALT` file and the calendar.
    async fn sync_halts(&self, now: DateTime<Utc>) {
        if self.kill_switch.expire_if_day_rolled(now.date_naive()) {
            info!("A rest-of-day halt expired with the UTC rollover");
            if let Err(e) = self.store.clear_halts("day_rollover", now).await {
                warn!(error = %e, "Could not clear the expired halt row");
            }
        }
        // The background poller usually gets here first; this covers the gap
        // between process start and its first tick — and, either way, this is
        // where the persisted row is cleared when the file goes away. Without
        // that, deleting the file resumed the agent in memory and left the
        // row behind, so the next restart reinstated a lifted halt, cancelled
        // every resting order and fired a Critical alert for it.
        if self.kill_switch.sync_with_file(now) == FileSync::Resumed {
            info!("HALT file removed — resuming");
            if let Err(e) = self.store.clear_halts("halt_file_removed", now).await {
                warn!(error = %e, "Could not clear the halt row — a restart would halt again");
            }
        }
        // Anything raised since the last cycle — by the poller, the
        // dashboard, or a signal — gets its side effects now.
        self.apply_pending_halt().await;
    }

    /// Ask every venue whether the ledger still describes reality.
    ///
    /// Returns the total account equity the venues reported, which is what
    /// the breakers must be measured against — the reconciler already asks
    /// each venue for it, and fetching it twice a cycle would be two calls
    /// for one number.
    ///
    /// `None` means no venue would say. That is not zero, and the caller must
    /// not treat it as a quiet day.
    async fn audit_venue_state(&self, now: DateTime<Utc>) -> Option<Decimal> {
        if self.venues.is_empty() {
            return None;
        }
        let reconciler = StateReconciler {
            registry: &self.venues,
            store: &self.store,
        };
        let reports = match reconciler.run(now, self.cycle_number as i64).await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "State reconciliation could not run");
                return None;
            }
        };

        // All the venues or none of them — see `combined_equity`.
        let equity: Option<Decimal> = crate::execution::reconcile::combined_equity(&reports);

        for report in reports {
            match report.verdict {
                Verdict::Clean => {}
                // Not a halt. A venue that cannot answer also cannot accept
                // orders, so trading there has already stopped — halting the
                // *other* venues over it would turn one venue's outage into a
                // whole-agent outage.
                Verdict::Unverified => {
                    warn!(
                        venue = %report.venue_id,
                        detail = %report.detail,
                        "Venue state could not be verified"
                    );
                    let _ = self
                        .alert_client
                        .anomaly(
                            AlertLevel::Warning,
                            AnomalyKind::ReconciliationMismatch,
                            &report.venue_id,
                            &format!("State unverified: {}", report.detail),
                        )
                        .await;
                }
                Verdict::Mismatch => {
                    self.raise_halt(Halt::new(
                        HaltSource::Reconciliation,
                        // Tomorrow will not make the books agree.
                        HaltScope::UntilResume,
                        format!("{}: {}", report.venue_id, report.detail),
                        now,
                    ));
                }
            }
        }

        equity
    }

    /// Account value the circuit breakers are measured against.
    ///
    /// Two things this gets right that the obvious version does not.
    ///
    /// **It is marked, not cost basis.** Buying $10 of something takes $10
    /// out of cash and puts $10 of exposure on; if it then halves, neither
    /// number moves. An equity built from cash plus cost basis never falls
    /// while a position bleeds, so the daily-loss and drawdown breakers would
    /// only ever see *realised* losses — they would watch the book go to zero
    /// without objecting once.
    ///
    /// **It is the account that holds the positions.** The legacy wallet
    /// balance is a Polygon USDC figure that does not move with Alpaca P&L,
    /// so measuring an Alpaca deployment against it measures nothing. When
    /// venues are configured, their own equity is authoritative.
    ///
    /// `None` means the number could not be established. The caller halts:
    /// trading on when the one input to every loss limit is unavailable is
    /// the wrong direction on the control that bounds the worst case.
    async fn account_equity(&self, venue_equity: Option<Decimal>) -> Option<Decimal> {
        if !self.venues.is_empty() {
            // A venue that would not report is already flagged Unverified by
            // the reconciler; if *none* would, there is no equity to speak of.
            return venue_equity;
        }

        // Legacy Polymarket-only path.
        let cash = self.current_balance().await;
        let cost_basis = fills::unrealized_exposure(&self.store)
            .await
            .unwrap_or(Decimal::ZERO);
        let marked = match self.store.total_unrealized_pnl().await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "Could not read unrealized P&L — equity would be overstated");
                return None;
            }
        };
        Some(cash + cost_basis + marked)
    }

    /// Mark the day's equity and test it against the risk limits.
    ///
    /// `equity` is account value including the *marked* value of open
    /// positions, not free cash and not cost basis. See `account_equity`.
    ///
    /// `None` halts. Every loss limit is measured against this number, so not
    /// having it means none of them can be evaluated — and an agent that goes
    /// on trading while its only risk input is unavailable has no risk
    /// controls at all, however many are configured.
    /// Cycles in a row that equity could not be established before halting.
    ///
    /// An `UntilResume` halt needs a human, so raising one on the first
    /// failure turns a single 500 from a book endpoint into an indefinite
    /// outage — and an operator who meets that weekly learns to resume without
    /// reading, which is worse than the gap it was guarding. One cycle of
    /// grace absorbs a transient; a second in a row is a real problem and
    /// halts. The window is bounded and the agent says where it is in it.
    const UNKNOWN_EQUITY_GRACE_CYCLES: u32 = 1;

    /// Consecutive cycles in which no equity figure could be established.
    ///
    /// Zero in normal operation. Worth reading rather than inferring: it is
    /// the difference between "one book call failed" and "the risk limits
    /// have not been evaluated since this morning".
    pub fn unknown_equity_cycles(&self) -> u32 {
        self.unknown_equity_cycles
    }

    async fn check_breakers(&mut self, now: DateTime<Utc>, equity: Option<Decimal>) {
        let Some(equity) = equity else {
            self.unknown_equity_cycles += 1;
            if self.unknown_equity_cycles <= Self::UNKNOWN_EQUITY_GRACE_CYCLES {
                // Deliberately not a halt *yet*, and deliberately still an
                // alert: for this one cycle the loss limits are unevaluated,
                // which is a real exposure and not a silent one.
                warn!(
                    cycles = self.unknown_equity_cycles,
                    "No venue would report account equity — the risk limits cannot be \
                     evaluated this cycle; halting if it happens again"
                );
                let _ = self
                    .alert_client
                    .anomaly(
                        AlertLevel::Warning,
                        AnomalyKind::ReconciliationMismatch,
                        "risk",
                        "Account equity unavailable — risk limits unevaluated for one cycle",
                    )
                    .await;
                return;
            }
            self.raise_halt(Halt::new(
                HaltSource::CircuitBreaker,
                HaltScope::UntilResume,
                format!(
                    "No venue would report account equity for {} cycles, so no risk limit \
                     can be evaluated",
                    self.unknown_equity_cycles
                ),
                now,
            ));
            return;
        };
        // A figure again: the streak is over.
        self.unknown_equity_cycles = 0;

        let today = now.date_naive();
        let marks = match self.store.record_equity(today, equity).await {
            Ok(m) => m,
            Err(e) => {
                // Without marks there is no way to know whether a limit has
                // been breached. Trading on when the answer is unavailable is
                // the wrong direction on the one control that bounds the
                // worst case.
                self.raise_halt(Halt::new(
                    HaltSource::CircuitBreaker,
                    HaltScope::UntilResume,
                    format!("Could not read or record equity marks, so no risk limit can be evaluated: {e}"),
                    now,
                ));
                return;
            }
        };

        let trades_today = self
            .store
            .count_trades_opened_on(today)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "Could not count today's trades — treating as zero");
                0
            });
        let losses = self.store.consecutive_losses().await.unwrap_or_else(|e| {
            warn!(error = %e, "Could not read the losing streak — treating as zero");
            0
        });

        if let Some(trip) =
            circuit_breaker::evaluate(equity, &marks, trades_today, losses, &self.config.risk)
        {
            self.raise_halt(Halt::new(
                HaltSource::CircuitBreaker,
                trip.scope(),
                format!("{trip}"),
                now,
            ));
        }
    }

    fn has_valuation_engine(&self) -> bool {
        self.valuation_engine.is_some()
    }

    /// Whether the current state allows opening new positions. Exits and
    /// settlement still run in every state.
    fn opens_positions(&self) -> bool {
        matches!(self.state, AgentState::Alive | AgentState::LowFuel)
    }

    /// One pass of the venue-based loop over continuous assets.
    async fn run_venue_cycle(&self) -> Result<CycleOutcome> {
        let cycle = VenueCycle {
            registry: &self.venues,
            llm: self.llm.as_deref(),
            store: &self.store,
            config: &self.config,
        };
        cycle
            .run(chrono::Utc::now(), self.state, self.cycle_number as i64)
            .await
    }

    #[instrument(
        skip(self),
        fields(
            otel.name = "agent.cycle",
            cycle = self.cycle_number,
            agent.state = %self.state,
            markets_scanned = tracing::field::Empty,
            trades_placed = tracing::field::Empty,
        ),
        // `err` emits an ERROR-level event on the failure path, which
        // tracing-opentelemetry turns into an Error span status. Without it a
        // cycle that blew up renders in the trace UI as a perfectly ordinary
        // green span, and the operator has to go back to the logs — which is
        // the problem tracing was added to solve.
        err
    )]
    pub async fn run_cycle(&mut self) -> Result<()> {
        let start = Instant::now();
        info!(cycle = self.cycle_number, state = %self.state, "Starting cycle");

        let now = Utc::now();

        // 0. Halt housekeeping, before anything else looks at the state.
        //
        // Both directions: a `HALT` file that appeared since the last wake
        // stops the agent, and a rest-of-day halt whose day has passed lifts
        // itself.
        self.sync_halts(now).await;

        // 1. Enhanced survival check (Phase 7)
        let old_state = self.state;
        let balance = self.current_balance().await;
        let unrealized = fills::unrealized_exposure(&self.store)
            .await
            .unwrap_or(Decimal::ZERO);
        let next_cycle_cost = self_funding::estimate_next_cycle_cost(&self.store, 20).await;

        let survival = enhanced_survival_check(
            balance,
            unrealized,
            next_cycle_cost,
            self.config.agent.death_balance_threshold,
            self.config.agent.api_reserve,
            self.config.agent.low_fuel_threshold,
        );

        // 2. Does the ledger still match the venues, and is the account
        // inside its risk limits? Both can halt, so both run before the state
        // is settled — deciding to trade and *then* discovering the books
        // disagree is the ordering this exists to prevent.
        let venue_equity = self.audit_venue_state(now).await;
        let equity = self.account_equity(venue_equity).await;
        self.check_breakers(now, equity).await;
        // Whatever either of those raised gets its orders cancelled, its
        // alert sent and its row written — here, once, in one place.
        self.apply_pending_halt().await;

        // A halt outranks the survival ladder, except for death. See
        // `market::models::apply_halt`.
        self.state = apply_halt(survival, self.kill_switch.is_tripped());

        // Alert on state changes (Phase 8)
        if self.state != old_state {
            if let Err(e) = self
                .alert_client
                .state_change(old_state, self.state, balance)
                .await
            {
                warn!(error = %e, "Failed to send state change alert");
            }
        }

        // Check bankroll milestones (Phase 8)
        if self.last_balance > Decimal::ZERO {
            if let Some(milestone) = check_milestone(self.last_balance, balance) {
                if let Err(e) = self
                    .alert_client
                    .bankroll_milestone(balance, milestone)
                    .await
                {
                    warn!(error = %e, "Failed to send milestone alert");
                }
            }
        }
        self.last_balance = balance;

        let mut markets_scanned: i64 = 0;
        let mut opportunities_found: i64 = 0;
        let mut trades_placed: i64 = 0;
        let mut cycle_api_cost = Decimal::ZERO;

        // Re-evaluate open positions for exit signals (RISK-01).
        // Always run, even in Dead state — positions need cleanup (TRD-06).
        self.evaluate_open_positions().await;

        // Continuous-asset positions never settle themselves, so they need an
        // explicit exit pass. Like the legacy one above this runs in every
        // state: being unable to open new positions must never mean being
        // unable to close existing ones.
        if !self.venues.is_empty() {
            // Resolve outstanding orders before anything else looks at
            // positions. An exit that filled since the last cycle must be on
            // the books before the exit pass runs, or it re-sells a position
            // that is already gone; an entry that filled must be visible
            // before the cycle decides what to buy. This also releases the
            // per-symbol block that unresolved orders hold.
            let reconciler = Reconciler {
                registry: &self.venues,
                store: &self.store,
                order_ttl: chrono::Duration::seconds(
                    self.config.execution.order_ttl_seconds as i64,
                ),
            };
            match reconciler.run(chrono::Utc::now()).await {
                // An order the venue will not talk about keeps blocking its
                // symbol, which is the safe direction but not a free one: the
                // agent stops trading that symbol entirely until someone
                // looks. Silence here is indistinguishable from working.
                Ok(report) if report.unqueryable > 0 => {
                    let _ = self
                        .alert_client
                        .anomaly(
                            AlertLevel::Warning,
                            AnomalyKind::OrderStateUnknown,
                            "reconcile",
                            &format!(
                                "{} of {} orders could not be resolved — those symbols stay blocked",
                                report.unqueryable, report.checked
                            ),
                        )
                        .await;
                }
                Ok(_) => {}
                // The pass itself failing is worse than any single finding:
                // nothing is checking whether the ledger still matches the
                // venue, so every later decision runs on unverified state.
                Err(e) => {
                    let _ = self
                        .alert_client
                        .anomaly(
                            AlertLevel::Critical,
                            AnomalyKind::ReconciliationMismatch,
                            "reconcile",
                            &format!("Reconciliation pass failed — ledger is unverified: {e}"),
                        )
                        .await;
                }
            }

            let exits = VenueExits {
                registry: &self.venues,
                store: &self.store,
                max_hold_hours: self.config.exits_continuous.max_hold_hours,
                order_ttl_seconds: self.config.execution.order_ttl_seconds as i64,
            };
            match exits
                .run(chrono::Utc::now(), self.cycle_number as i64)
                .await
            {
                Ok(0) => {}
                Ok(closed) => info!(closed, "Closed continuous positions this cycle"),
                Err(e) => warn!(error = %e, "Venue exit pass failed"),
            }
        }

        // Check for resolved markets and settle trades.
        // Always run, even in Dead state — must settle P&L for final accounting (TRD-06).
        {
            // Only meaningful on the Polymarket path: nothing else resolves.
            let settlement = match &self.polymarket {
                Some(client) => {
                    resolution::check_and_settle(
                        &self.store,
                        client.http_client(),
                        client.gamma_base_url(),
                    )
                    .await
                }
                None => Ok(Vec::new()),
            };
            match settlement {
                Ok(settled) if !settled.is_empty() => {
                    let pnl: Decimal = settled.iter().map(|r| r.pnl).sum();
                    info!(
                        settled = settled.len(),
                        pnl = %pnl,
                        "Resolved trades this cycle"
                    );
                }
                Err(e) => warn!(error = %e, "Resolution check failed"),
                _ => {}
            }
        }

        // Daily API budget. Asked of the ledger, not of the database.
        //
        // The old check read the day's total from SQLite and then spawned a
        // batch of valuations against that one number, so ten concurrent
        // calls could each see "$0.40 of $0.50 spent, fine" and collectively
        // spend $1.30. The ledger holds reservations, so what is left here
        // already accounts for calls that are in flight.
        let snapshot = self.budget.snapshot(now.date_naive());
        let budget_available = self.budget.remaining(now.date_naive()) > Decimal::ZERO;
        if !budget_available {
            // Worth alerting rather than only logging: from here the agent
            // still runs cycles and still looks healthy, but it forms no new
            // views, so it quietly stops doing the thing it exists to do
            // until the day rolls over.
            let _ = self
                .alert_client
                .anomaly(
                    AlertLevel::Warning,
                    AnomalyKind::BudgetExhausted,
                    "",
                    &format!(
                        "Spent ${} of ${} — no new valuations until the UTC day rolls over",
                        snapshot.spent, snapshot.budget
                    ),
                )
                .await;
        }

        match self.state {
            AgentState::Dead => {
                self.shutdown().await?;
                return Ok(());
            }
            AgentState::Halted => {
                // Everything above this point has already run: the exit
                // pass, order reconciliation, settlement and the state
                // audit. Only entries stop here.
                warn!(
                    cycle = self.cycle_number,
                    reason = %self
                        .kill_switch
                        .current()
                        .map(|h| h.detail)
                        .unwrap_or_else(|| "unknown".to_string()),
                    "Halted — exits and reconciliation continue, no new positions"
                );
            }
            AgentState::CriticalSurvival => {
                warn!(
                    cycle = self.cycle_number,
                    "Critical survival mode — monitoring only"
                );
            }
            AgentState::LowFuel => {
                warn!(
                    cycle = self.cycle_number,
                    "Low fuel mode — reduced operations"
                );
                match scan_legacy(self.scanner.as_ref()).await {
                    Ok(candidates) => {
                        markets_scanned = candidates.len() as i64;
                        if self.has_valuation_engine() && budget_available {
                            let bankroll = self.effective_bankroll().await;
                            let result = self.evaluate_and_trade(&candidates, bankroll, 1).await;
                            opportunities_found = result.opportunities as i64;
                            trades_placed = result.trades as i64;
                            cycle_api_cost = result.api_cost;
                        }
                    }
                    Err(e) => {
                        // A scan that cannot reach the venue means the agent
                        // sees no markets at all — it goes on cycling, logging
                        // "Cycle complete", looking entirely healthy, and
                        // trading nothing. Observed in a real run against a
                        // DNS-blocked host: every cycle scanned 0 markets and
                        // nothing said why.
                        let _ = self
                            .alert_client
                            .anomaly(
                                AlertLevel::Critical,
                                AnomalyKind::VenueUnreachable,
                                "polymarket",
                                &format!("Market scan failed — no markets are visible: {e}"),
                            )
                            .await;
                    }
                }
            }
            AgentState::Alive => {
                info!(cycle = self.cycle_number, "Normal operation");
                match scan_legacy(self.scanner.as_ref()).await {
                    Ok(candidates) => {
                        markets_scanned = candidates.len() as i64;
                        info!(
                            candidates = candidates.len(),
                            "Scan complete — candidates found"
                        );

                        if self.has_valuation_engine() && budget_available {
                            let bankroll = self.effective_bankroll().await;
                            let result = self.evaluate_and_trade(&candidates, bankroll, 10).await;
                            opportunities_found = result.opportunities as i64;
                            trades_placed = result.trades as i64;
                            cycle_api_cost = result.api_cost;
                        } else {
                            opportunities_found = candidates.len() as i64;
                        }
                    }
                    Err(e) => {
                        // A scan that cannot reach the venue means the agent
                        // sees no markets at all — it goes on cycling, logging
                        // "Cycle complete", looking entirely healthy, and
                        // trading nothing. Observed in a real run against a
                        // DNS-blocked host: every cycle scanned 0 markets and
                        // nothing said why.
                        let _ = self
                            .alert_client
                            .anomaly(
                                AlertLevel::Critical,
                                AnomalyKind::VenueUnreachable,
                                "polymarket",
                                &format!("Market scan failed — no markets are visible: {e}"),
                            )
                            .await;
                    }
                }
            }
        }

        // Venue path: continuous assets (crypto, equities). Runs alongside the
        // legacy Polymarket loop above, and only when the agent state permits
        // new positions — the same gate the legacy path applies.
        if !self.venues.is_empty() && budget_available && self.opens_positions() {
            match self.run_venue_cycle().await {
                Ok(outcome) => {
                    markets_scanned += outcome.instruments_scanned as i64;
                    opportunities_found += outcome.views_taken as i64;
                    trades_placed += outcome.orders_placed as i64;
                    cycle_api_cost += outcome.api_cost;
                }
                Err(e) => warn!(error = %e, "Venue cycle failed"),
            }
        }

        // Fill in the two fields the span declared as `Empty` at creation.
        //
        // Declaring them and never recording them left every exported
        // `agent.cycle` span with both blank — so a trace could show that a
        // cycle took 202 seconds but not whether it had scanned anything or
        // traded anything, which is the first question anyone asks of it.
        let span = tracing::Span::current();
        span.record("markets_scanned", markets_scanned);
        span.record("trades_placed", trades_placed);

        // Phase 7: Log cost breakdown
        let cumulative_api_cost = self
            .store
            .get_total_api_cost()
            .await
            .unwrap_or(Decimal::ZERO);
        let costs = CycleCosts::new(cycle_api_cost);
        log_cost_breakdown(self.cycle_number, &costs, cumulative_api_cost);

        // Log cycle results. The error is reported to the caller (so the
        // consecutive-failure counter and health status in main.rs still
        // work) but `cycle_number` advances below regardless, so the next
        // call starts a genuinely new cycle instead of replaying this one's
        // scanning, paid valuations and trades just to redo a bookkeeping
        // write.
        let duration = start.elapsed();
        let log_result = self
            .log_cycle(
                duration,
                markets_scanned,
                opportunities_found,
                trades_placed,
                cycle_api_cost,
            )
            .await;
        if let Err(ref e) = log_result {
            let _ = self
                .alert_client
                .anomaly(
                    AlertLevel::Warning,
                    AnomalyKind::DbWriteFailed,
                    "cycles",
                    &format!("Could not record the cycle summary: {e}"),
                )
                .await;
        }

        // Phase 8: Periodic metrics summary (every 10 cycles)
        if self.cycle_number > 0 && self.cycle_number % 10 == 0 {
            match compute_metrics(&self.store, self.config.agent.initial_paper_balance).await {
                Ok(m) => {
                    log_metrics(&m);
                    if let Err(e) = self.alert_client.daily_summary(&m).await {
                        warn!(error = %e, "Failed to send metrics alert");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to compute metrics"),
            }
        }

        self.cycle_number += 1;

        log_result
    }

    /// Full pipeline: evaluate candidates → size with Kelly → check constraints → execute.
    /// Uses parallel evaluation with JoinSet for higher throughput.
    async fn evaluate_and_trade(
        &mut self,
        candidates: &[MarketCandidate],
        bankroll: Decimal,
        max_evaluations: usize,
    ) -> CycleResult {
        let engine = self.valuation_engine.as_ref().unwrap();
        let mut result = CycleResult::default();

        // Build market queries for data aggregation
        let queries: Vec<MarketQuery> = candidates
            .iter()
            .map(|c| MarketQuery {
                condition_id: c.market.condition_id.clone(),
                question: c.market.question.clone(),
                category: c.market.category.clone(),
            })
            .collect();

        // Phase 3: Fetch external data for all candidates
        let all_data = self.data_aggregator.fetch_all(&queries).await;
        info!(data_points = all_data.len(), "External data collected");

        // Phase 4+5+6: Evaluate → Size → Execute
        // Parallel evaluation with JoinSet for higher throughput
        let mut join_set = tokio::task::JoinSet::new();
        let engine_arc = self.valuation_engine.as_ref().unwrap().clone_for_parallel();
        let config_valuation = self.config.valuation.clone();

        // Start only as many valuations as the day's budget can pay for.
        //
        // Each spawned call reserves its own estimate before it runs, so an
        // over-large batch is refused rather than overspent — but it is
        // refused *after* the data aggregation above has already been done
        // for every candidate. Sizing the batch up front means the work that
        // gets started is work that can be paid for.
        let affordable = self
            .llm
            .as_ref()
            .and_then(|c| c.affordable_calls(Utc::now().date_naive()))
            .unwrap_or(usize::MAX);
        let max_evaluations = max_evaluations.min(affordable);
        if affordable < candidates.len() {
            info!(
                affordable,
                candidates = candidates.len(),
                "Valuation batch trimmed to what the day's budget allows"
            );
        }

        // Spawn parallel valuation tasks
        for candidate in candidates.iter().take(max_evaluations) {
            let estimated_cost = engine.estimated_call_cost();
            if estimated_cost > bankroll - result.api_cost {
                warn!(
                    estimated_cost = %estimated_cost,
                    remaining = %(bankroll - result.api_cost),
                    "Stopping evaluations — insufficient bankroll for API cost"
                );
                break;
            }

            let candidate = candidate.clone();
            let relevant_data: Vec<DataPoint> = all_data
                .iter()
                .filter(|dp| dp.relevance_to.contains(&candidate.market.condition_id))
                .cloned()
                .collect();
            let engine = engine_arc.clone();
            let config = config_valuation.clone();
            let cycle_num = self.cycle_number as i64;
            let remaining_budget = bankroll - result.api_cost;

            join_set.spawn(async move {
                // Phase 4: Get valuation from Claude
                let valuation = match engine
                    .evaluate(&candidate, &relevant_data, remaining_budget, cycle_num)
                    .await
                {
                    Ok(Some(v)) => v,
                    Ok(None) => return None,
                    Err(_) => return None,
                };

                let edge = match evaluate_edge(&candidate, &valuation, &config) {
                    Some(e) => e,
                    None => return None,
                };

                Some((candidate, valuation, edge))
            });
        }

        // Collect results from parallel tasks
        let mut eval_results = Vec::new();
        while let Some(result_opt) = join_set.join_next().await {
            if let Ok(Some((candidate, valuation, edge))) = result_opt {
                eval_results.push((candidate, valuation, edge));
            }
        }

        info!(
            evaluations = eval_results.len(),
            "Parallel evaluations complete"
        );

        // Process results sequentially for trade execution
        for (candidate, valuation, edge) in eval_results {
            let estimated_cost = engine.estimated_call_cost();
            result.api_cost += estimated_cost;
            result.opportunities += 1;
            self.log_opportunity(&candidate, &valuation, &edge);

            // Apply calibration discount to confidence (HAL-01)
            let calibrated_confidence = match crate::valuation::calibration::compute_discount(
                self.store.pool(),
                200, // Look back 200 resolved trades
            )
            .await
            {
                Ok(discount) => {
                    let calibrated = valuation.confidence * discount;
                    if discount < dec!(1.0) {
                        info!(
                            original_confidence = %valuation.confidence,
                            discount = %discount,
                            calibrated_confidence = %calibrated,
                            "Calibration discount applied"
                        );
                    }
                    calibrated
                }
                Err(e) => {
                    warn!(error = %e, "Failed to compute calibration discount — using raw confidence");
                    valuation.confidence
                }
            };

            // Phase 5: Kelly sizing with calibrated confidence
            let kelly_result = kelly::kelly_size(
                valuation.probability,
                edge.trade_price,
                calibrated_confidence,
                bankroll - result.api_cost,
                self.state,
                &self.config.risk,
            );

            if !kelly_result.should_trade() {
                info!(
                    market = %candidate.market.question,
                    kelly_raw = %kelly_result.kelly_raw,
                    "Kelly says no trade"
                );
                continue;
            }

            // Phase 7: Check if projected profit justifies the API cost
            if !edge_justifies_cost(kelly_result.position_usd, edge.raw_edge, estimated_cost) {
                info!(
                    market = %candidate.market.question,
                    position_usd = %kelly_result.position_usd,
                    edge = %edge.raw_edge,
                    api_cost = %estimated_cost,
                    "Edge doesn't justify API cost — skipping"
                );
                continue;
            }

            // Build opportunity with kelly size
            let opportunity =
                to_opportunity(&candidate, &valuation, &edge, kelly_result.position_usd);

            // Portfolio constraint check
            let constraint_check = self.portfolio.check_constraints(&opportunity, bankroll);
            if !constraint_check.passed() {
                info!(
                    market = %candidate.market.question,
                    "Portfolio constraint check failed"
                );
                continue;
            }

            // Adjust size for remaining portfolio capacity
            let adjusted_size = self
                .portfolio
                .adjust_size(kelly_result.position_usd, bankroll);
            if adjusted_size <= Decimal::ZERO {
                continue;
            }

            // Liquidity check — use the order-book side that will actually be
            // traded. Passing ask depth unconditionally here would pair
            // YES-side liquidity with a NO-side reference price, understating
            // risk for NO trades now that liquidity_adjusted_size actually
            // uses best_price's magnitude to scale its caps.
            let depth = limits::depth_at_best(
                &candidate
                    .order_book
                    .levels_for_side(edge.side)
                    .iter()
                    .map(|l| (l.price, l.size))
                    .collect::<Vec<_>>(),
            );
            let liquidity_size = limits::liquidity_adjusted_size(
                adjusted_size,
                edge.trade_price,
                depth,
                self.config.execution.max_slippage_pct,
            );
            if liquidity_size < self.config.risk.min_position_usd {
                info!(
                    market = %candidate.market.question,
                    liquidity_size = %liquidity_size,
                    "Insufficient liquidity"
                );
                continue;
            }

            // Update opportunity with final adjusted size
            let mut final_opportunity = opportunity;
            final_opportunity.kelly_size = liquidity_size;

            // Phase 6: Prepare and execute order
            let prepared = match order::prepare_order(
                &final_opportunity,
                kelly_result.kelly_raw,
                kelly_result.kelly_adjusted,
                &self.config.execution,
            ) {
                Ok(p) => p,
                Err(e) => {
                    warn!(market = %candidate.market.question, error = %e, "Order preparation failed");
                    continue;
                }
            };

            info!(
                market = %prepared.market_question,
                side = %prepared.side,
                price = %prepared.price,
                size = %prepared.size,
                kelly_raw = %kelly_result.kelly_raw,
                kelly_adjusted = %kelly_result.kelly_adjusted,
                edge = %edge.raw_edge,
                "Executing trade"
            );

            let Some(polymarket) = self.polymarket.as_ref() else {
                // Unreachable in practice: candidates only come from the
                // Polymarket scanner, which does not exist without a client.
                // Stated rather than unwrapped, because an order is the one
                // place a wrong assumption costs money.
                warn!("Refusing to execute a Polymarket trade without a Polymarket client");
                continue;
            };
            let execution = order::execute_order(polymarket, &prepared).await;

            // Record trade in database
            if let Err(e) =
                fills::record_trade(&self.store, &prepared, &execution, self.cycle_number).await
            {
                // The order went to the venue and the ledger does not know.
                // Every later decision — exposure, exits, reconciliation —
                // now reasons from a position that is missing, so this is the
                // most expensive write in the cycle to lose.
                let _ = self
                    .alert_client
                    .anomaly(
                        AlertLevel::Critical,
                        AnomalyKind::DbWriteFailed,
                        "trades",
                        &format!(
                            "Order was placed but not recorded — the ledger is behind the venue: {e}"
                        ),
                    )
                    .await;
            }

            if execution.status == OrderStatus::Filled {
                result.trades += 1;

                // Record prediction for confidence calibration (HAL-01)
                if let Err(e) = calibration::record_prediction(
                    self.store.pool(),
                    &prepared.market_id,
                    valuation.confidence,
                    valuation.probability,
                    prepared.price,
                )
                .await
                {
                    warn!(error = %e, "Failed to record calibration prediction");
                }

                // Update portfolio tracker
                self.portfolio.add_position(Position {
                    market_id: prepared.market_id.clone(),
                    token_id: prepared.token_id.clone(),
                    category: candidate.market.category.clone(),
                    side: prepared.side,
                    size_usd: liquidity_size,
                    entry_price: prepared.price,
                });

                // Phase 8: Send trade alert
                if let Err(e) = self
                    .alert_client
                    .trade_placed(
                        &prepared.market_question,
                        prepared.side,
                        liquidity_size,
                        prepared.price,
                        edge.raw_edge,
                    )
                    .await
                {
                    warn!(error = %e, "Failed to send trade alert");
                }

                info!(
                    market = %prepared.market_question,
                    side = %prepared.side,
                    size_usd = %liquidity_size,
                    total_exposure = %self.portfolio.total_exposure(),
                    positions = self.portfolio.position_count(),
                    "Position added to portfolio"
                );
            }
        }

        result
    }

    /// Re-evaluate open positions for stop-loss exit signals (RISK-01).
    /// Fetches current YES price from Gamma and evaluates against max loss threshold.
    /// In paper mode, marks positions as CANCELLED. In live mode, places sell orders.
    async fn evaluate_open_positions(&self) {
        use crate::risk::exit::{evaluate_exit, DEFAULT_MAX_LOSS_PCT};

        let open_trades = match self.store.get_open_trades().await {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "Failed to fetch open trades for exit evaluation");
                return;
            }
        };

        for trade in &open_trades {
            let trade_id = match trade.id {
                Some(id) => id,
                None => continue,
            };
            let entry_price: Decimal = match trade.entry_price.parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let side = match trade.direction.as_str() {
                "YES" => crate::market::models::Side::Yes,
                "NO" => crate::market::models::Side::No,
                _ => continue,
            };
            let size: Decimal = match trade.size.parse() {
                Ok(s) => s,
                Err(_) => continue,
            };

            // Fetch current YES price from Gamma API
            let Some(polymarket) = self.polymarket.as_ref() else {
                // Legacy Polymarket exits only; the venue path has its own.
                break;
            };
            let current_yes_price = match polymarket.get_current_yes_price(&trade.market_id).await {
                Ok(p) => p,
                Err(e) => {
                    warn!(
                        market_id = %trade.market_id,
                        error = %e,
                        "Failed to fetch current price for exit evaluation"
                    );
                    continue;
                }
            };

            let signal = evaluate_exit(
                &trade.market_id,
                entry_price,
                side,
                current_yes_price,
                DEFAULT_MAX_LOSS_PCT,
            );

            if signal.should_exit {
                warn!(
                    market_id = %trade.market_id,
                    entry_price = %entry_price,
                    current_price = %current_yes_price,
                    pnl_pct = %signal.pnl_pct,
                    reason = %signal.reason,
                    "EXIT SIGNAL triggered"
                );

                // In live mode, place an actual sell order to exit
                if self.config.agent.mode == crate::config::AgentMode::Live {
                    // Find the token_id for this trade
                    let token_id = match self.find_token_id_for_trade(&trade.market_id, side).await
                    {
                        Some(tid) => tid,
                        None => {
                            warn!(
                                market_id = %trade.market_id,
                                "Could not find token_id for exit order"
                            );
                            continue;
                        }
                    };

                    // Exit at current market price
                    let exit_price = match side {
                        crate::market::models::Side::Yes => current_yes_price,
                        crate::market::models::Side::No => Decimal::ONE - current_yes_price,
                    };

                    match polymarket
                        .exit_position(&token_id, side, exit_price, size)
                        .await
                    {
                        Ok(order_id) => {
                            info!(
                                order_id = %order_id,
                                market_id = %trade.market_id,
                                "Live exit order placed"
                            );
                        }
                        Err(e) => {
                            warn!(
                                error = %e,
                                market_id = %trade.market_id,
                                "Failed to place live exit order"
                            );
                        }
                    }
                }

                // Mark trade as cancelled/exited in database
                let pnl = signal.pnl_pct * entry_price * size;
                if let Err(e) = self
                    .store
                    .update_trade_status(trade_id, "CANCELLED", Some(pnl), Some(chrono::Utc::now()))
                    .await
                {
                    warn!(error = %e, "Failed to update trade status for exit");
                }
            } else {
                info!(
                    market_id = %trade.market_id,
                    pnl_pct = %signal.pnl_pct,
                    "Position healthy"
                );
            }
        }
    }

    /// Find the token_id for a given market and side.
    /// Used for constructing exit orders in live mode.
    async fn find_token_id_for_trade(
        &self,
        market_id: &str,
        side: crate::market::models::Side,
    ) -> Option<String> {
        // Query the Gamma API to get market tokens
        let polymarket = self.polymarket.as_ref()?;
        let url = format!("{}/markets", polymarket.gamma_base_url());
        let markets: Vec<serde_json::Value> = match polymarket
            .http_client()
            .get(&url)
            .query(&[("condition_id", market_id)])
            .send()
            .await
        {
            Ok(resp) => match resp.json().await {
                Ok(m) => m,
                Err(_) => return None,
            },
            Err(_) => return None,
        };

        let market = markets.first()?;
        let token_ids = market
            .get("clobTokenIds")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())?;

        let outcomes = market
            .get("outcomes")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())?;

        // Find the token index matching the side
        let target_outcome = match side {
            crate::market::models::Side::Yes => "Yes",
            crate::market::models::Side::No => "No",
        };

        for (i, outcome) in outcomes.iter().enumerate() {
            if outcome.eq_ignore_ascii_case(target_outcome) {
                return token_ids.get(i).cloned();
            }
        }

        // Fallback to first token
        token_ids.first().cloned()
    }

    fn log_opportunity(
        &self,
        candidate: &MarketCandidate,
        valuation: &ValuationResult,
        edge: &EdgeResult,
    ) {
        info!(
            market = %candidate.market.question,
            fair_prob = %valuation.probability,
            market_prob = %edge.market_probability,
            edge = %edge.raw_edge,
            side = %edge.side,
            confidence = %valuation.confidence,
            "OPPORTUNITY FOUND"
        );
    }

    /// Spendable cash, from whichever account actually holds it.
    ///
    /// The Polygon USDC wallet when this is a Polymarket deployment; the sum
    /// of the venues' free cash when it is not. Returning zero for a venue
    /// deployment — which is what dropping the wallet naively would do — makes
    /// the survival ladder declare a fully funded agent dead on its first
    /// cycle and stop it trading.
    ///
    /// Venue cash is summed across venues, which assumes they quote in the
    /// same currency. Every venue here reports US dollars; a venue that did
    /// not would need this to become per-currency rather than a total.
    async fn current_balance(&self) -> Decimal {
        if let Some(polymarket) = self.polymarket.as_ref() {
            return match polymarket.get_balance().await {
                Ok(balance) => balance,
                Err(e) => {
                    warn!(error = %e, "Failed to get balance, using zero");
                    Decimal::ZERO
                }
            };
        }

        let mut total = Decimal::ZERO;
        for venue in self.venues.all() {
            match venue.balance().await {
                Ok(balance) => total += balance.available,
                Err(e) => warn!(
                    venue = %venue.id(),
                    error = %format!("{e:#}"),
                    "Could not read venue cash — it is missing from the balance"
                ),
            }
        }
        total
    }

    /// Calculate effective bankroll: wallet balance minus reserve and unrealized exposure.
    async fn effective_bankroll(&self) -> Decimal {
        let balance = self.current_balance().await;
        let unrealized = fills::unrealized_exposure(&self.store)
            .await
            .unwrap_or(Decimal::ZERO);
        wallet::effective_bankroll(balance, self.config.agent.api_reserve, unrealized)
    }

    async fn shutdown(&self) -> Result<()> {
        let balance = self.current_balance().await;
        error!(
            cycle = self.cycle_number,
            balance = %balance,
            "AGENT DEATH — balance depleted, shutting down"
        );

        // A dead agent with orders still on the book is the same problem as a
        // halted one, minus anybody left to notice.
        self.cancel_resting_orders("agent death").await;

        // Phase 8: Send death alert
        if let Err(e) = self
            .alert_client
            .agent_death(self.cycle_number, balance)
            .await
        {
            warn!(error = %e, "Failed to send death alert");
        }

        Ok(())
    }

    async fn log_cycle(
        &self,
        duration: std::time::Duration,
        markets_scanned: i64,
        opportunities_found: i64,
        trades_placed: i64,
        api_cost: Decimal,
    ) -> Result<()> {
        let balance = self.current_balance().await;
        let unrealized = fills::unrealized_exposure(&self.store)
            .await
            .unwrap_or(Decimal::ZERO);

        let cycle = CycleRecord {
            id: None,
            cycle_number: self.cycle_number as i64,
            markets_scanned: Some(markets_scanned),
            opportunities_found: Some(opportunities_found),
            trades_placed: Some(trades_placed),
            api_cost: Some(api_cost.to_string()),
            bankroll: Some(balance.to_string()),
            unrealized_pnl: Some(unrealized.to_string()),
            agent_state: self.state.to_string(),
            duration_ms: Some(duration.as_millis() as i64),
            created_at: None,
        };

        self.store.insert_cycle(&cycle).await?;

        info!(
            cycle = self.cycle_number,
            duration_ms = duration.as_millis(),
            state = %self.state,
            bankroll = %balance,
            markets_scanned,
            opportunities_found,
            trades_placed,
            api_cost = %api_cost,
            unrealized_exposure = %unrealized,
            "Cycle complete"
        );

        Ok(())
    }

    pub fn is_dead(&self) -> bool {
        self.state == AgentState::Dead
    }

    pub fn cycle_number(&self) -> u64 {
        self.cycle_number
    }

    pub fn current_state(&self) -> AgentState {
        self.state
    }

    /// Shared so a watchdog outside the loop can report on it. A cycle that
    /// hangs never returns to the loop, so the loop cannot notice its own
    /// stall — something else has to hold the same client.
    pub fn alerts(&self) -> Arc<AlertClient> {
        self.alert_client.clone()
    }

    /// When the loop should run the next cycle. Kept on `Agent` so the venue
    /// registry stays private — `main` schedules without knowing what a
    /// session is.
    pub fn next_wake(&self, now: DateTime<Utc>) -> WakePlan {
        scheduler::next_wake(
            &self.venues,
            now,
            Duration::seconds(self.config.agent.cycle_interval_seconds as i64),
            Duration::seconds(self.config.agent.max_sleep_seconds as i64),
        )
    }
}

/// Aggregated results from a single cycle's evaluate+trade pipeline.
#[derive(Default)]
struct CycleResult {
    opportunities: usize,
    trades: usize,
    api_cost: Decimal,
}
