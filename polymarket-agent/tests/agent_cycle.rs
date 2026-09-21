//! One whole agent cycle, against mock venues.
//!
//! Everything below this level is unit-tested: the breakers are pure, the
//! venue cycle runs against a `StubVenue`, the reconciler against stubs. What
//! had no coverage at all was the *wiring* — `Agent::new` is called from
//! exactly one place in the codebase, `main.rs`, and nothing drove a cycle.
//!
//! That is not a gap in the abstract. Every defect the Phase 3 review found
//! lived in precisely that seam: the breakers were handed the Polymarket
//! wallet instead of the venue account, the absolute live caps were computed
//! and then never passed to sizing, and halts raised outside the loop never
//! reached the code that acts on them. Each component was correct; the
//! assembly was not, and no unit test can see that.
//!
//! So these tests build a real `Agent` over a mock Alpaca and a mock model,
//! run a cycle, and assert on what ends up in the database.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;
use std::str::FromStr;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use polymarket_agent::agent::kill_switch::KillSwitch;
use polymarket_agent::agent::lifecycle::Agent;
use polymarket_agent::config::{
    AgentConfig, AgentMode, AppConfig, ContinuousSizingConfig, DatabaseConfig, ExecutionConfig,
    ExitsContinuousConfig, LlmProvider, MonitoringConfig, PolymarketConfig, RateLimitConfig,
    RiskConfig, ScanningConfig, SecretString, Secrets, TelemetryConfig, ValuationConfig,
    VenueConfig,
};
use polymarket_agent::db::store::Store;

/// Account equity the mock Alpaca reports. Deliberately unlike the paper
/// Polymarket balance (100) so a test can tell which account was measured.
const ALPACA_EQUITY: &str = "4200.00";
const ALPACA_CASH: &str = "4000.00";

/// A mock Alpaca with every endpoint one cycle touches.
async fn alpaca_server() -> MockServer {
    let server = MockServer::start().await;
    mount_common(&server).await;
    server
}

/// Every endpoint a cycle touches, plus a POST that merely acknowledges.
async fn mount_common(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v2/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "acct-1", "currency": "USD", "status": "ACTIVE",
            "cash": ALPACA_CASH, "equity": ALPACA_EQUITY,
            "buying_power": "8000.00",
            "non_marginable_buying_power": ALPACA_CASH
        })))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v2/assets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "symbol": "BTC/USD", "class": "crypto", "exchange": "CRYPTO",
            "name": "Bitcoin / US Dollar", "status": "active",
            "tradable": true, "fractionable": true,
            "min_order_size": "0.000026",
            "min_trade_increment": "0.000000001",
            "price_increment": "1"
        }])))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1beta3/crypto/us/latest/orderbooks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "orderbooks": {
                "BTC/USD": {
                    "t": "2026-09-21T10:00:00Z",
                    "b": [{"p": 60000.0, "s": 5.0}],
                    "a": [{"p": 60010.0, "s": 5.0}]
                }
            }
        })))
        .mount(server)
        .await;

    // Enough bars for the ATR window, trending gently so volatility is real
    // but modest — a flat series yields a zero ATR and no position at all.
    let bars: Vec<_> = (0..40)
        .map(|i| {
            let base = 60000.0 + (i as f64) * 25.0;
            json!({
                "t": format!("2026-09-{:02}T10:00:00Z", (i % 28) + 1),
                "o": base, "h": base + 300.0, "l": base - 300.0, "c": base + 50.0, "v": 10
            })
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/v1beta3/crypto/us/bars"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"bars": {"BTC/USD": bars}})))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v2/positions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v2/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v2/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "venue-order-1",
            "client_order_id": "will-be-overwritten",
            "symbol": "BTC/USD",
            "status": "accepted",
            "qty": "0.001",
            "filled_qty": "0",
            "side": "buy",
            "type": "limit",
            "time_in_force": "gtc"
        })))
        .mount(server)
        .await;
}

/// A venue that fills on the spot: the submit acknowledgement itself reports
/// the order filled, so the reconciler never sees it.
///
/// Built by layering a higher-priority POST over the standard mock rather
/// than re-registering the endpoints in a different order — wiremock resolves
/// ties by registration order, and rebuilding the set by hand produced a
/// server whose asset listing stopped matching on the second cycle.
async fn instant_fill_alpaca_server() -> MockServer {
    let server = alpaca_server().await;
    Mock::given(method("POST"))
        .and(path("/v2/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "venue-order-1",
            "client_order_id": "ignored",
            "symbol": "BTC/USD",
            "status": "filled",
            "qty": "0.001",
            "filled_qty": "0.001",
            "filled_avg_price": "60065.005",
            "side": "buy",
            "type": "limit",
            "time_in_force": "gtc"
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    // Positions have to be staged, not static: flat before the fill and
    // holding after it.
    //
    // A venue that reports the position from the first call disagrees with an
    // empty ledger; one that never reports it disagrees with a full ledger.
    // Either way reconciliation halts the agent — correctly, and that is how
    // this fixture's missing positions were found. The first reconciliation
    // runs before the entry, so the empty answer is consumed once.
    Mock::given(method("GET"))
        .and(path("/v2/positions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/positions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "symbol": "BTCUSD",
            "asset_class": "crypto",
            "qty": "0.001",
            "avg_entry_price": "60065.005"
        }])))
        .with_priority(2)
        .mount(&server)
        .await;
    server
}

/// The same venue, but every order comes back already filled — above the mid,
/// so the execution carries measurable slippage.
async fn filling_alpaca_server() -> MockServer {
    let server = alpaca_server().await;
    // The reconciler prefers the venue id once it has one, and falls back to
    // the client id — mock both so the test does not depend on which.
    let filled = json!({
        "id": "venue-order-1",
        "client_order_id": "ignored",
        "symbol": "BTC/USD",
        "status": "filled",
        "qty": "0.001",
        "filled_qty": "0.001",
        // The mid at submission is 60005; filling at 60065.005 is +10 bps.
        "filled_avg_price": "60065.005",
        "side": "buy",
        "type": "limit",
        "time_in_force": "gtc"
    });
    Mock::given(method("GET"))
        .and(path("/v2/orders/venue-order-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(filled))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/orders:by_client_order_id"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "venue-order-1",
            "client_order_id": "ignored",
            "symbol": "BTC/USD",
            "status": "filled",
            "qty": "0.001",
            "filled_qty": "0.001",
            // The mid at submission is 60005; filling at 60065 is +10 bps.
            "filled_avg_price": "60065.005",
            "side": "buy",
            "type": "limit",
            "time_in_force": "gtc"
        })))
        .mount(&server)
        .await;
    server
}

/// A model that always returns a tradeable long view.
async fn model_server() -> MockServer {
    let server = MockServer::start().await;
    let view = json!({
        "direction": "long",
        "p_up": 0.72,
        "expected_return_pct": 0.05,
        "horizon_hours": 24,
        "confidence": 0.8,
        "invalidation_price": 57000,
        "target_price": 64000,
        "reasoning_summary": "test",
        "key_factors": ["test"],
        "data_quality": "high"
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": view.to_string()}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 50}
        })))
        .mount(&server)
        .await;
    server
}

/// A Polymarket that lists nothing, so the legacy loop is inert and the test
/// is about the venue path only.
async fn polymarket_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/markets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    server
}

fn config(alpaca: &MockServer, model: &MockServer, poly: &MockServer, db: &str) -> AppConfig {
    AppConfig {
        agent: AgentConfig {
            mode: AgentMode::Paper,
            cycle_interval_seconds: 600,
            death_balance_threshold: dec!(0),
            low_fuel_threshold: dec!(10),
            api_reserve: dec!(2),
            initial_paper_balance: dec!(100),
            daily_api_budget: dec!(5),
            max_sleep_seconds: 3600,
        },
        scanning: ScanningConfig {
            max_markets: 50,
            min_volume_24h: dec!(0),
            max_resolution_days: 14,
            max_spread_pct: dec!(0.05),
            categories: vec!["crypto".to_string()],
        },
        valuation: ValuationConfig {
            temperature: None,
            response_format: None,
            provider: LlmProvider::OpenAiCompatible,
            model: "test-model".to_string(),
            base_url: Some(format!("{}/v1", model.uri())),
            input_price_per_million: Some(dec!(1)),
            output_price_per_million: Some(dec!(1)),
            max_tokens: 1024,
            min_edge_threshold: dec!(0.01),
            high_confidence_edge: dec!(0.01),
            low_confidence_edge: dec!(0.02),
            cache_ttl_seconds: 0,
        },
        risk: RiskConfig {
            // Loose enough that the breakers do not fire on the first cycle;
            // individual tests tighten what they are exercising.
            max_position_pct: dec!(0.50),
            ..RiskConfig::default()
        },
        sizing_continuous: ContinuousSizingConfig::default(),
        exits_continuous: ExitsContinuousConfig::default(),
        execution: ExecutionConfig {
            order_type: "limit".to_string(),
            order_ttl_seconds: 300,
            max_slippage_pct: dec!(0.02),
            max_retries: 1,
        },
        monitoring: MonitoringConfig {
            log_level: "warn".to_string(),
            discord_enabled: false,
            daily_summary_hour: 9,
            dashboard_port: 0,
            dashboard_bind: "127.0.0.1".to_string(),
        },
        polymarket: PolymarketConfig {
            clob_base_url: poly.uri(),
            gamma_base_url: poly.uri(),
            chain_id: 137,
        },
        rate_limit: RateLimitConfig {
            requests_per_second: 100,
            burst_size: 100,
            backoff_base_ms: 1,
            backoff_max_ms: 2,
        },
        database: DatabaseConfig {
            path: db.to_string(),
            data_dir: None,
        },
        telemetry: TelemetryConfig::default(),
        venues: vec![VenueConfig {
            id: "alpaca".to_string(),
            kind: "alpaca".to_string(),
            enabled: true,
            base_url: Some(alpaca.uri()),
            data_url: Some(alpaca.uri()),
            symbols: vec!["BTC/USD".to_string()],
            fee_pct: dec!(0.0025),
        }],
    }
}

fn secrets() -> Secrets {
    Secrets {
        llm_api_key: Some(SecretString::from("test-key")),
        alpaca_key_id: Some(SecretString::from("test-id")),
        alpaca_secret_key: Some(SecretString::from("test-secret")),
        ..Secrets::default()
    }
}

struct Harness {
    agent: Agent,
    store: Store,
    kill_switch: std::sync::Arc<KillSwitch>,
    // Held so the mock servers and temp dir outlive the agent.
    _alpaca: MockServer,
    _model: MockServer,
    _poly: MockServer,
    _dir: tempfile::TempDir,
}

async fn harness(tweak: impl FnOnce(&mut AppConfig)) -> Harness {
    harness_with(alpaca_server().await, tweak).await
}

async fn harness_with(alpaca: MockServer, tweak: impl FnOnce(&mut AppConfig)) -> Harness {
    let model = model_server().await;
    let poly = polymarket_server().await;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("agent.db");

    let mut cfg = config(&alpaca, &model, &poly, db.to_str().unwrap());
    tweak(&mut cfg);

    let store = Store::new(db.to_str().unwrap()).await.unwrap();
    let kill_switch = std::sync::Arc::new(KillSwitch::new(dir.path().join("HALT")));
    let agent = Agent::new(
        cfg,
        secrets(),
        Store::from_pool(store.pool().clone()),
        kill_switch.clone(),
    )
    .await
    .expect("agent builds");

    Harness {
        agent,
        store,
        kill_switch,
        _alpaca: alpaca,
        _model: model,
        _poly: poly,
        _dir: dir,
    }
}

fn money(v: &str) -> Decimal {
    Decimal::from_str(v).unwrap()
}

/// The defect this exists for: equity was taken from `current_balance()`,
/// which is the *Polymarket* wallet. An Alpaca-only deployment — the
/// configuration the whole safety gate is for — therefore measured drawdown
/// and daily loss against an account that never moves with Alpaca P&L, and in
/// live mode with no Polymarket credentials that number is zero, so every
/// loss limit was computed from zero.
#[tokio::test]
async fn the_breakers_are_measured_against_the_venue_account() {
    let mut h = harness(|_| {}).await;
    h.agent.run_cycle().await.expect("cycle runs");

    let equity = h.store.get_daily_equity().await.unwrap();
    assert_eq!(equity.len(), 1, "one row for today");

    let recorded = money(&equity[0].starting_equity);
    assert_eq!(
        recorded,
        money(ALPACA_EQUITY),
        "equity must come from the venue account, not the {} paper Polymarket wallet",
        100
    );
    assert_ne!(
        recorded,
        dec!(100),
        "recording the Polymarket balance is the bug this test exists for"
    );
}

/// The venue path has to actually place an order, or every screen and every
/// promotion criterion downstream measures nothing.
#[tokio::test]
async fn a_cycle_takes_a_view_and_records_an_order() {
    let mut h = harness(|_| {}).await;
    h.agent.run_cycle().await.expect("cycle runs");

    let orders = h.store.get_orders(10).await.unwrap();
    assert_eq!(orders.len(), 1, "one order, got {orders:#?}");
    let o = &orders[0];
    assert_eq!(o.venue_id, "alpaca");
    assert_eq!(o.symbol, "BTC/USD");
    assert_eq!(o.side, "BUY");
    assert_eq!(o.intent, "ENTRY");
    // Recorded before submission, so a timed-out request still leaves
    // something for reconciliation to resolve.
    assert!(!o.client_order_id.is_empty());
}

/// Reconciliation runs every cycle and leaves a trail, even when clean —
/// "no rows" and "never checked" must not look the same.
#[tokio::test]
async fn a_cycle_records_a_reconciliation_run() {
    let mut h = harness(|_| {}).await;
    h.agent.run_cycle().await.expect("cycle runs");

    let runs = h.store.get_reconciliation_runs(10).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].venue_id, "alpaca");
    assert!(
        runs[0].passed,
        "a fresh ledger against a flat venue is clean"
    );
}

/// A halt raised from outside the loop — the dashboard, a signal — must reach
/// the code that acts on it. It previously did not: the side effects keyed
/// off `trip()`'s return value, which only the agent's own callers ever saw.
#[tokio::test]
async fn a_halt_raised_outside_the_loop_stops_entries_and_is_persisted() {
    let mut h = harness(|_| {}).await;

    h.kill_switch
        .trip(polymarket_agent::agent::kill_switch::Halt::new(
            polymarket_agent::agent::kill_switch::HaltSource::Api,
            polymarket_agent::risk::circuit_breaker::HaltScope::UntilResume,
            "operator",
            chrono::Utc::now(),
        ));

    h.agent.run_cycle().await.expect("cycle runs");

    assert_eq!(
        h.store.get_orders(10).await.unwrap().len(),
        0,
        "a halted agent must open nothing"
    );
    let halt = h
        .store
        .active_halt()
        .await
        .unwrap()
        .expect("the halt must survive a restart");
    assert_eq!(halt.source, "api");
    assert_eq!(
        h.agent.current_state().to_string(),
        "HALTED",
        "and the state must say so"
    );
}

/// A tripped breaker stops entries on the same cycle it fires.
#[tokio::test]
async fn a_tripped_breaker_stops_entries_within_the_cycle() {
    // Zero trades allowed today: `trades_today (0) >= 0` trips on the first
    // evaluation, before anything is opened.
    let mut h = harness(|c| c.risk.max_trades_per_day = 0).await;
    h.agent.run_cycle().await.expect("cycle runs");

    assert!(h.kill_switch.is_tripped(), "the breaker must have fired");
    assert_eq!(
        h.store.get_orders(10).await.unwrap().len(),
        0,
        "no order may be placed on the cycle the breaker trips"
    );
    let halt = h.store.active_halt().await.unwrap().expect("persisted");
    assert_eq!(halt.source, "circuit_breaker");
    assert_eq!(
        halt.scope, "rest_of_day",
        "a trade-count trip clears at the UTC rollover"
    );
}

/// Exits, reconciliation and settlement keep running while halted — refusing
/// to close a position because the day went badly is how a bounded loss
/// becomes an unbounded one.
#[tokio::test]
async fn reconciliation_still_runs_while_halted() {
    let mut h = harness(|c| c.risk.max_trades_per_day = 0).await;
    h.agent.run_cycle().await.expect("cycle runs");

    assert!(h.kill_switch.is_tripped());
    assert_eq!(
        h.store.get_reconciliation_runs(10).await.unwrap().len(),
        1,
        "a halted cycle still audits its ledger against the venue"
    );
}

/// Each cycle is written down, halted or not, so a gap in the series means
/// downtime rather than an uneventful hour.
#[tokio::test]
async fn a_cycle_is_recorded_even_when_it_trades_nothing() {
    let mut h = harness(|c| c.risk.max_trades_per_day = 0).await;
    h.agent.run_cycle().await.expect("cycle runs");
    assert_eq!(h.store.get_cycle_count().await.unwrap(), 1);
}

/// Slippage and time-to-fill are two of the paper-window promotion criteria,
/// and neither was measurable: nothing in the codebase ever wrote a row to
/// `fills`. The table existed, the dashboard queried it, and it was always
/// empty — so the Fills screen showed nothing and the criteria could not be
/// evaluated at all.
#[tokio::test]
async fn a_filled_order_is_recorded_as_an_execution_with_slippage() {
    let mut h = harness_with(filling_alpaca_server().await, |_| {}).await;

    // First cycle places the order.
    h.agent.run_cycle().await.expect("cycle one");
    assert_eq!(h.store.get_orders(10).await.unwrap().len(), 1);

    // Second cycle reconciles it, finds it filled, and records the execution.
    h.agent.run_cycle().await.expect("cycle two");

    let fills = h.store.get_fills(10).await.unwrap();
    assert_eq!(fills.len(), 1, "the fill must be recorded, got {fills:#?}");
    let f = &fills[0];
    assert_eq!(f.symbol, "BTC/USD");
    assert_eq!(f.side, "BUY");

    // The mid at submission was 60005 (60000/60010); filling at 60065.005 is
    // +10 bps, and positive means it cost us.
    let bps = f
        .slippage_bps
        .as_deref()
        .expect("a fill against a recorded mid must carry slippage");
    assert_eq!(
        bps, "10.00",
        "slippage in bps against the mid at submission"
    );
    assert!(
        f.mid_at_submit.is_some(),
        "the mid has to survive from placement to fill"
    );
}

/// The order row has to carry the mid, or the fill has nothing to measure
/// against — this is the link that did not exist.
#[tokio::test]
async fn a_placed_order_records_the_mid_it_was_decided_at() {
    let mut h = harness(|_| {}).await;
    h.agent.run_cycle().await.expect("cycle runs");

    let orders = h.store.get_orders(10).await.unwrap();
    let mid = orders[0]
        .mid_at_submit
        .as_deref()
        .expect("every order must record the mid it was priced against");
    // Compared as a number: `Decimal` preserves the scale of the inputs, so
    // the mid of a 60000.0/60010.0 book serialises as "60005.0".
    assert_eq!(
        Decimal::from_str(mid).unwrap(),
        dec!(60005),
        "the mid of a 60000/60010 book"
    );
}

/// The last unmeasurable promotion criterion. Nothing on the venue path
/// wrote a forecast down, so "Brier ≤0.24 on ≥30 closed positions" could not
/// be evaluated on the only path the paper window actually exercises.
#[tokio::test]
async fn a_venue_entry_records_a_forecast_for_calibration() {
    let mut h = harness(|_| {}).await;
    h.agent.run_cycle().await.expect("cycle runs");

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT market_id, fair_value FROM confidence_calibration WHERE resolved = 0",
    )
    .fetch_all(h.store.pool())
    .await
    .unwrap();

    assert_eq!(rows.len(), 1, "one forecast per entry, got {rows:?}");
    // Keyed by trade id: one row per position, so two positions on one
    // symbol cannot resolve each other's forecast.
    assert_eq!(rows[0].0, "trade:1");
    assert_eq!(
        Decimal::from_str(&rows[0].1).unwrap(),
        dec!(0.72),
        "the forecast recorded is p_up, which is what a Brier score is over"
    );
}

/// A marketable order comes back filled in its own acknowledgement. Terminal
/// states are excluded from `get_unresolved_orders`, so the reconciler never
/// sees it — and before this was handled, nothing recorded the execution at
/// all. Slippage was then measured over only the fills slow enough to need
/// reconciling, which is the biased tail of the distribution being measured.
#[tokio::test]
async fn an_order_filled_at_submission_is_still_recorded_as_an_execution() {
    let mut h = harness_with(instant_fill_alpaca_server().await, |_| {}).await;
    h.agent.run_cycle().await.expect("cycle runs");

    let fills = h.store.get_fills(10).await.unwrap();
    assert_eq!(
        fills.len(),
        1,
        "an inline fill must be recorded, got {fills:#?}"
    );
    assert_eq!(fills[0].slippage_bps.as_deref(), Some("10.00"));
}

/// The other half: an inline fill must not *also* be recorded by the next
/// reconciliation pass. The increment guard is what prevents it, and it works
/// because the order row already carries the filled quantity by then.
#[tokio::test]
async fn an_inline_fill_is_not_recorded_twice_on_the_next_cycle() {
    let mut h = harness_with(instant_fill_alpaca_server().await, |_| {}).await;
    h.agent.run_cycle().await.expect("cycle one");
    assert_eq!(h.store.get_fills(10).await.unwrap().len(), 1);

    h.agent.run_cycle().await.expect("cycle two");

    // Cycle two opens a second position, which also fills inline — so two
    // orders and two executions. What must not happen is a *restatement*:
    // the venue reports cumulative quantities, and recording those verbatim
    // would give the first order a second row for the same 0.001.
    let orders = h.store.get_orders(10).await.unwrap();
    let fills = h.store.get_fills(10).await.unwrap();
    assert_eq!(
        fills.len(),
        orders.len(),
        "one execution per filled order, got {} orders and {fills:#?}",
        orders.len()
    );
    assert!(
        fills
            .iter()
            .all(|f| Decimal::from_str(&f.qty).unwrap() == dec!(0.001)),
        "each execution records its own increment, not a running total: {fills:#?}"
    );
}

/// A position the venue does not report halts the agent.
///
/// The base fixture's `/v2/positions` is always empty, so once an entry
/// fills, the ledger holds something the venue denies. That is the single
/// most expensive disagreement there is — it means the agent is sizing,
/// marking and stopping against a position picture that is wrong — and it
/// must stop trading rather than carry on.
///
/// Found by accident: a test that expected a second position got none, and
/// the reason was this halt firing exactly as designed.
#[tokio::test]
async fn a_position_the_venue_does_not_report_halts_the_agent() {
    let mut h = harness_with(filling_alpaca_server().await, |_| {}).await;

    h.agent
        .run_cycle()
        .await
        .expect("cycle one places an order");
    h.agent.run_cycle().await.expect("cycle two fills it");
    // Cycle three: the ledger now holds a position the venue denies.
    h.agent.run_cycle().await.expect("cycle three reconciles");

    assert!(
        h.kill_switch.is_tripped(),
        "a ledger the venue contradicts must stop trading"
    );
    let halt = h.store.active_halt().await.unwrap().expect("persisted");
    assert_eq!(halt.source, "reconciliation");
    assert_eq!(
        halt.scope, "until_resume",
        "tomorrow will not make the books agree"
    );
    assert!(
        halt.detail
            .as_deref()
            .unwrap_or_default()
            .contains("BTC/USD"),
        "the halt must name what disagreed: {halt:?}"
    );
}

/// A venue-based deployment must not need a Polymarket credential.
///
/// `--dry-run` was changed to stop demanding `POLYMARKET_PRIVATE_KEY` when no
/// Polymarket venue is enabled — but `Agent::new` still built a
/// `PolymarketClient` unconditionally, so the dry run reported a config as
/// sound and the agent then died at startup on the very key the dry run had
/// just said was not needed. The user this repo targets is a US resident who
/// may not legally trade Polymarket at all, so that is the normal case here,
/// not an edge one.
#[tokio::test]
async fn an_agent_with_venues_builds_without_a_polymarket_key() {
    let alpaca = alpaca_server().await;
    let model = model_server().await;
    let poly = polymarket_server().await;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("agent.db");

    let mut cfg = config(&alpaca, &model, &poly, db.to_str().unwrap());
    // Live is the mode that demanded the key.
    cfg.agent.mode = AgentMode::Live;

    let store = Store::new(db.to_str().unwrap()).await.unwrap();
    let kill_switch = std::sync::Arc::new(KillSwitch::new(dir.path().join("HALT")));

    let without_polymarket = Secrets {
        polymarket_private_key: None,
        ..secrets()
    };

    Agent::new(cfg, without_polymarket, store, kill_switch)
        .await
        .expect("a venue-based agent has no use for a Polymarket key");
}

/// A venue whose `/v2/account` fails, so no equity figure can be established.
///
/// Every other endpoint answers normally: the point is that one call failing
/// must not read as "this deployment has no risk controls, halt it".
async fn no_equity_alpaca_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/account"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream is down"))
        .mount(&server)
        .await;
    mount_common(&server).await;
    server
}

/// An `UntilResume` halt needs a human. Raising one the first time a book
/// endpoint 500s turns a thirty-second hiccup into an indefinite outage — and
/// an operator who meets that weekly learns to resume without reading, which
/// is worse than the gap it was guarding.
#[tokio::test]
async fn one_cycle_without_an_equity_figure_warns_rather_than_halts() {
    let mut h = harness_with(no_equity_alpaca_server().await, |_| {}).await;
    h.agent.run_cycle().await.unwrap();

    assert!(
        !h.kill_switch.is_tripped(),
        "a single unreadable equity must not need a human to clear"
    );
}

/// A second consecutive failure is not a hiccup. Trading on when the one input
/// to every loss limit is unavailable has no risk controls at all, however
/// many are configured.
#[tokio::test]
async fn a_second_cycle_without_an_equity_figure_halts() {
    let mut h = harness_with(no_equity_alpaca_server().await, |_| {}).await;
    h.agent.run_cycle().await.unwrap();
    h.agent.run_cycle().await.unwrap();

    assert!(
        h.kill_switch.is_tripped(),
        "a persistent gap is a real problem and must stop entries"
    );
}

/// The grace is for a *transient*, so the streak has to reset when equity
/// comes back. Without the reset, two hiccups an hour apart would halt the
/// agent as if they had been consecutive.
#[tokio::test]
async fn a_recovered_equity_figure_clears_the_streak() {
    let alpaca = MockServer::start().await;
    // Registered first, so it wins over the healthy mock beneath it — and
    // scoped, so dropping the guard heals the endpoint mid-test. That is what
    // lets one agent go unknown and then known, which is the only way to see
    // the counter reset.
    let broken = alpaca
        .register_as_scoped(
            Mock::given(method("GET"))
                .and(path("/v2/account"))
                .respond_with(ResponseTemplate::new(500).set_body_string("down")),
        )
        .await;
    mount_common(&alpaca).await;

    let mut h = harness_with(alpaca, |_| {}).await;
    h.agent.run_cycle().await.unwrap();
    assert_eq!(
        h.agent.unknown_equity_cycles(),
        1,
        "one cycle without a figure"
    );
    assert!(!h.kill_switch.is_tripped(), "and no halt for a single one");

    drop(broken);
    h.agent.run_cycle().await.unwrap();
    assert_eq!(
        h.agent.unknown_equity_cycles(),
        0,
        "equity came back, so the next failure is a first failure again"
    );
}

/// The split made cash and equity two calls where there had been one, and
/// several callers ask for cash each cycle — the bankroll, the survival
/// ladder, `shutdown`. An account snapshot shared across the cycle is what
/// keeps that from multiplying.
///
/// The exact number is the contract here, not a bound: an earlier version of
/// this test asserted `<= 8`, which passed identically on the code before the
/// change and therefore guarded nothing. Five is what both the pre-split code
/// and the uncached split produce; one is what this is for.
#[tokio::test]
async fn a_cycle_reads_the_account_once() {
    let alpaca = alpaca_server().await;
    let mut h = harness_with(alpaca, |_| {}).await;
    h.agent.run_cycle().await.unwrap();

    let reqs = h._alpaca.received_requests().await.unwrap();
    let account_calls = reqs
        .iter()
        .filter(|r| r.url.path() == "/v2/account")
        .count();

    assert_eq!(
        account_calls, 1,
        "cash and equity are projections of one payload and must share a snapshot — \
         five here means the cache stopped working, and they can also disagree"
    );
}
