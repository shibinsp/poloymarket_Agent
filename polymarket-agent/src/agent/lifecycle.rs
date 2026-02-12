use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rust_decimal::Decimal;
use tracing::{error, info, warn};

use crate::agent::self_funding::{
    self, CycleCosts, enhanced_survival_check, edge_justifies_cost, log_cost_breakdown,
};
use crate::config::{AppConfig, Secrets};
use crate::data::crypto::CryptoSource;
use crate::data::news::NewsSource;
use crate::data::sports::SportsSource;
use crate::data::weather::WeatherSource;
use crate::data::{DataAggregator, DataPoint, MarketQuery};
use crate::db::store::{CycleRecord, Store};
use crate::execution::fills;
use crate::execution::order::{self, OrderStatus};
use crate::execution::wallet;
use crate::market::models::{AgentState, MarketCandidate};
use crate::market::polymarket::PolymarketClient;
use crate::market::scanner::MarketScanner;
use crate::monitoring::alerts::{AlertClient, check_milestone};
use crate::monitoring::metrics::{compute_metrics, log_metrics};
use crate::risk::kelly;
use crate::risk::limits;
use crate::risk::portfolio::{PortfolioManager, Position};
use crate::valuation::claude::ClaudeClient;
use crate::valuation::edge::{evaluate_edge, to_opportunity, EdgeResult};
use crate::valuation::fair_value::{ValuationEngine, ValuationResult};

pub struct Agent {
    config: AppConfig,
    store: Store,
    state: AgentState,
    cycle_number: u64,
    polymarket: Arc<PolymarketClient>,
    scanner: MarketScanner,
    data_aggregator: DataAggregator,
    valuation_engine: Option<ValuationEngine>,
    portfolio: PortfolioManager,
    alert_client: AlertClient,
    last_balance: Decimal,
}

impl Agent {
    pub async fn new(config: AppConfig, secrets: Secrets, store: Store) -> Result<Self> {
        let config_arc = Arc::new(config.clone());
        let polymarket = Arc::new(PolymarketClient::new(config_arc, &secrets).await?);
        let scanner = MarketScanner::new(polymarket.clone(), config.scanning.clone());

        // Phase 3: Initialize data sources
        let data_sources: Vec<Box<dyn crate::data::DataSource>> = vec![
            Box::new(WeatherSource::new()),
            Box::new(SportsSource::new()),
            Box::new(CryptoSource::new()),
            Box::new(NewsSource::new()),
        ];
        let data_aggregator = DataAggregator::new(data_sources);

        // Phase 4: Initialize valuation engine (only if API key is available)
        let valuation_engine = if let Some(ref api_key) = secrets.anthropic_api_key {
            let claude_store = Store::new(&config.database.path).await?;
            let claude_client = Arc::new(ClaudeClient::new(
                api_key.clone(),
                config.valuation.claude_model.clone(),
                claude_store,
            ));
            Some(ValuationEngine::new(
                claude_client,
                config.valuation.clone(),
            ))
        } else {
            warn!("ANTHROPIC_API_KEY not set — valuation engine disabled");
            None
        };

        // Phase 5: Initialize portfolio manager
        let portfolio = PortfolioManager::new(config.risk.clone());

        // Phase 8: Initialize alert client
        let alert_client = AlertClient::new(
            secrets.discord_webhook_url.clone(),
            config.monitoring.discord_enabled,
        );

        // Resume cycle number from last recorded cycle
        let cycle_number = match store.get_latest_cycle().await? {
            Some(cycle) => cycle.cycle_number as u64 + 1,
            None => 0,
        };

        info!(
            mode = ?config.agent.mode,
            cycle_number,
            valuation_enabled = valuation_engine.is_some(),
            alerts_enabled = alert_client.is_enabled(),
            "Agent initialized"
        );

        Ok(Self {
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
        })
    }

    fn has_valuation_engine(&self) -> bool {
        self.valuation_engine.is_some()
    }

    pub async fn run_cycle(&mut self) -> Result<()> {
        let start = Instant::now();
        info!(cycle = self.cycle_number, state = %self.state, "Starting cycle");

        // 1. Enhanced survival check (Phase 7)
        let old_state = self.state;
        let balance = self.current_balance().await;
        let unrealized = fills::unrealized_exposure(&self.store)
            .await
            .unwrap_or(Decimal::ZERO);
        let next_cycle_cost =
            self_funding::estimate_next_cycle_cost(&self.store, 20).await;

        self.state = enhanced_survival_check(
            balance,
            unrealized,
            next_cycle_cost,
            self.config.agent.death_balance_threshold,
            self.config.agent.api_reserve,
            self.config.agent.low_fuel_threshold,
        );

        // Alert on state changes (Phase 8)
        if self.state != old_state
            && let Err(e) = self
                .alert_client
                .state_change(old_state, self.state, balance)
                .await
        {
            warn!(error = %e, "Failed to send state change alert");
        }

        // Check bankroll milestones (Phase 8)
        if self.last_balance > Decimal::ZERO
            && let Some(milestone) = check_milestone(self.last_balance, balance)
            && let Err(e) = self
                .alert_client
                .bankroll_milestone(balance, milestone)
                .await
        {
            warn!(error = %e, "Failed to send milestone alert");
        }
        self.last_balance = balance;

        let mut markets_scanned: i64 = 0;
        let mut opportunities_found: i64 = 0;
        let mut trades_placed: i64 = 0;
        let mut cycle_api_cost = Decimal::ZERO;

        match self.state {
            AgentState::Dead => {
                self.shutdown().await?;
                return Ok(());
            }
            AgentState::CriticalSurvival => {
                warn!(cycle = self.cycle_number, "Critical survival mode — monitoring only");
            }
            AgentState::LowFuel => {
                warn!(cycle = self.cycle_number, "Low fuel mode — reduced operations");
                match self.scanner.scan().await {
                    Ok(candidates) => {
                        markets_scanned = candidates.len() as i64;
                        if self.has_valuation_engine() {
                            let bankroll = self.effective_bankroll().await;
                            let result = self
                                .evaluate_and_trade(&candidates, bankroll, 1)
                                .await;
                            opportunities_found = result.opportunities as i64;
                            trades_placed = result.trades as i64;
                            cycle_api_cost = result.api_cost;
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Market scan failed");
                    }
                }
            }
            AgentState::Alive => {
                info!(cycle = self.cycle_number, "Normal operation");
                match self.scanner.scan().await {
                    Ok(candidates) => {
                        markets_scanned = candidates.len() as i64;
                        info!(
                            candidates = candidates.len(),
                            "Scan complete — candidates found"
                        );

                        if self.has_valuation_engine() {
                            let bankroll = self.effective_bankroll().await;
                            let result = self
                                .evaluate_and_trade(&candidates, bankroll, 10)
                                .await;
                            opportunities_found = result.opportunities as i64;
                            trades_placed = result.trades as i64;
                            cycle_api_cost = result.api_cost;
                        } else {
                            opportunities_found = candidates.len() as i64;
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Market scan failed");
                    }
                }
            }
        }

        // Phase 7: Log cost breakdown
        let cumulative_api_cost = self
            .store
            .get_total_api_cost()
            .await
            .unwrap_or(Decimal::ZERO);
        let costs = CycleCosts::new(cycle_api_cost);
        log_cost_breakdown(self.cycle_number, &costs, cumulative_api_cost);

        // Log cycle results
        let duration = start.elapsed();
        self.log_cycle(
            duration,
            markets_scanned,
            opportunities_found,
            trades_placed,
            cycle_api_cost,
        )
        .await?;

        // Phase 8: Periodic metrics summary (every 10 cycles)
        if self.cycle_number > 0 && self.cycle_number.is_multiple_of(10) {
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

        Ok(())
    }

    /// Full pipeline: evaluate candidates → size with Kelly → check constraints → execute.
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
        for candidate in candidates.iter().take(max_evaluations) {
            // Check if API cost exceeds remaining bankroll
            let estimated_cost = engine.estimated_call_cost();
            if estimated_cost > bankroll - result.api_cost {
                warn!(
                    estimated_cost = %estimated_cost,
                    remaining = %(bankroll - result.api_cost),
                    "Stopping evaluations — insufficient bankroll for API cost"
                );
                break;
            }

            // Find data points relevant to this market
            let relevant_data: Vec<DataPoint> = all_data
                .iter()
                .filter(|dp| dp.relevance_to.contains(&candidate.market.condition_id))
                .cloned()
                .collect();

            // Phase 4: Get valuation from Claude
            let (valuation, edge) = match engine
                .evaluate(
                    candidate,
                    &relevant_data,
                    bankroll - result.api_cost,
                    self.cycle_number as i64,
                )
                .await
            {
                Ok(Some(valuation)) => {
                    result.api_cost += estimated_cost;
                    match evaluate_edge(candidate, &valuation, &self.config.valuation) {
                        Some(edge) => (valuation, edge),
                        None => continue,
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    warn!(market = %candidate.market.question, error = %e, "Valuation failed");
                    continue;
                }
            };

            result.opportunities += 1;
            self.log_opportunity(candidate, &valuation, &edge);

            // Phase 5: Kelly sizing
            let kelly_result = kelly::kelly_size(
                valuation.probability,
                edge.trade_price,
                valuation.confidence,
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
            if !edge_justifies_cost(
                kelly_result.position_usd,
                edge.raw_edge,
                estimated_cost,
            ) {
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
                to_opportunity(candidate, &valuation, &edge, kelly_result.position_usd);

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

            // Liquidity check
            let depth = limits::depth_at_best(
                &candidate
                    .order_book
                    .asks
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

            let execution = order::execute_order(&self.polymarket, &prepared).await;

            // Record trade in database
            if let Err(e) =
                fills::record_trade(&self.store, &prepared, &execution, self.cycle_number).await
            {
                warn!(error = %e, "Failed to record trade");
            }

            if execution.status == OrderStatus::Filled {
                result.trades += 1;

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

    async fn current_balance(&self) -> Decimal {
        match self.polymarket.get_balance().await {
            Ok(balance) => balance,
            Err(e) => {
                warn!(error = %e, "Failed to get balance, using zero");
                Decimal::ZERO
            }
        }
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
}

/// Aggregated results from a single cycle's evaluate+trade pipeline.
#[derive(Default)]
struct CycleResult {
    opportunities: usize,
    trades: usize,
    api_cost: Decimal,
}
