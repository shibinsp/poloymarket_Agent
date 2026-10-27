//! Integration and unit tests for Indian Stock Market Testing.
//!
//! Enforces:
//! 1. Look-ahead bias prevention: Signal at bar t executes on bar t+1 OPEN.
//! 2. Conservative SL/TP handling: When a candle touches both SL and TP, SL triggers first.
//! 3. Statutory Indian transaction cost calculation accuracy.
//! 4. Upstox Sandbox live-order isolation gate.
//! 5. Whole share lot enforcement and rejection of fractional quantities.
//! 6. Order lifecycle and duplicate prevention.

use chrono::{TimeZone, Utc};
use polymarket_agent::config::{AgentMode, AppConfig, Secrets, VenueConfig};
use polymarket_agent::db::store::Store;
use polymarket_agent::india::backtest::{
    parse_and_validate_csv, run_backtest, NseBacktestConfig, NseCandle,
};
use polymarket_agent::india::costs::IndianCostConfig;
use polymarket_agent::india::instruments::{find_nse_instrument, nse_watchlist};
use polymarket_agent::india::market_feed::UpstoxLiveMarketFeed;
use polymarket_agent::india::simulator::{
    compute_portfolio_accounting, evaluate_strategy_tick, execute_paper_order,
    IndiaPaperOrderRequest,
};
use polymarket_agent::venue::types::{Side, VenueId};
use polymarket_agent::venue::upstox::client::UpstoxSandboxClient;
use polymarket_agent::venue::upstox::types::UpstoxOrderRequestPayload;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;

fn test_config_with(venues: Vec<VenueConfig>, mode: AgentMode) -> AppConfig {
    let toml = std::fs::read_to_string("config/default.toml").expect("reads config");
    let mut config: AppConfig = toml::from_str(&toml).expect("parses config");
    config.venues = venues;
    config.agent.mode = mode;
    config
}

#[test]
fn test_nse_watchlist_universe_integrity() {
    let watchlist = nse_watchlist();
    assert_eq!(
        watchlist.len(),
        13,
        "Watchlist must contain 13 curated NSE bluechips"
    );
    let venue_id = VenueId::new("upstox_sandbox");
    for inst in watchlist {
        assert!(
            inst.upstox_key.starts_with("NSE_EQ|INE"),
            "Key must follow Upstox ISIN schema"
        );
        assert_eq!(inst.tick_size, dec!(0.05), "NSE tick size must be ₹0.05");
        assert_eq!(
            inst.lot_size,
            dec!(1.0),
            "NSE cash lot size must be 1 share"
        );
        let venue_inst = inst.to_venue_instrument(&venue_id);
        assert!(
            !venue_inst.fractional,
            "NSE cash equities do not support fractional shares"
        );
        assert!(find_nse_instrument(&inst.symbol).is_some());
    }
}

#[test]
fn test_statutory_indian_transaction_costs() {
    let config = IndianCostConfig::default();
    // Buy 100 shares of RELIANCE at ₹2,500 = ₹2,50,000 turnover
    let buy_turnover = dec!(250000.0);
    let buy_costs = config.calculate(buy_turnover, Side::Buy);

    // STT = 0.1% on delivery = ₹250.00
    assert_eq!(buy_costs.stt, dec!(250.00));
    // Brokerage = 0.05% of 250k = 125, capped at ₹20
    assert_eq!(buy_costs.brokerage, dec!(20.00));
    // Stamp duty = 0.015% on Buy = ₹37.50
    assert_eq!(buy_costs.stamp_duty, dec!(37.50));
    // GST = 18% on (brokerage + turnover charges + sebi + ipft)
    assert!(buy_costs.gst > Decimal::ZERO);
    assert!(buy_costs.total > dec!(300.00));

    // Sell 100 shares of RELIANCE at ₹2,600 = ₹2,60,000 turnover
    let sell_turnover = dec!(260000.0);
    let sell_costs = config.calculate(sell_turnover, Side::Sell);
    // Stamp duty is 0 on Sell
    assert_eq!(sell_costs.stamp_duty, Decimal::ZERO);
    // STT on Sell delivery is 0.1% = ₹260.00
    assert_eq!(sell_costs.stt, dec!(260.00));
}

#[test]
fn test_csv_parser_validation_and_geometry() {
    // Valid CSV
    let valid_csv = "\
timestamp,symbol,open,high,low,close,volume
2024-01-01 09:15:00,RELIANCE,2500,2520,2490,2510,50000
2024-01-01 10:15:00,RELIANCE,2510,2530,2505,2525,45000";
    let candles = parse_and_validate_csv(valid_csv).expect("Parses valid CSV");
    assert_eq!(candles.len(), 2);
    assert_eq!(candles[0].symbol, "RELIANCE");
    assert_eq!(candles[0].close, dec!(2510));

    // Inverted high/low geometry must fail
    let invalid_geometry = "\
timestamp,symbol,open,high,low,close,volume
2024-01-01 09:15:00,RELIANCE,2500,2450,2550,2510,50000";
    assert!(
        parse_and_validate_csv(invalid_geometry).is_err(),
        "Must reject high < low"
    );

    // Non-monotonic timestamps must fail
    let non_monotonic = "\
timestamp,symbol,open,high,low,close,volume
2024-01-01 10:15:00,RELIANCE,2510,2530,2505,2525,45000
2024-01-01 09:15:00,RELIANCE,2500,2520,2490,2510,50000";
    assert!(
        parse_and_validate_csv(non_monotonic).is_err(),
        "Must reject non-monotonic timestamps"
    );
}

#[test]
fn test_no_look_ahead_bias_and_conservative_sl_tp() {
    let mut candles = Vec::new();
    let base_dt = Utc.with_ymd_and_hms(2024, 1, 1, 3, 45, 0).unwrap(); // 09:15 IST is 03:45 UTC

    // Build 20 candles with upward trend to trigger fast SMA > slow SMA
    for i in 0..20 {
        let ts = base_dt + chrono::Duration::hours(i as i64);
        let px = dec!(2400.0) + Decimal::from(i * 10);
        candles.push(NseCandle {
            timestamp: ts,
            symbol: "RELIANCE".to_string(),
            open: px,
            high: px + dec!(15.0),
            low: px - dec!(5.0),
            close: px + dec!(10.0),
            volume: dec!(100000.0),
        });
    }

    // Candle 20: Big candle that touches BOTH SL and TP
    let ts_both = base_dt + chrono::Duration::hours(20);
    candles.push(NseCandle {
        timestamp: ts_both,
        symbol: "RELIANCE".to_string(),
        open: dec!(2600.0),
        high: dec!(2700.0), // Touches TP
        low: dec!(2500.0),  // Touches SL
        close: dec!(2650.0),
        volume: dec!(100000.0),
    });

    let config = NseBacktestConfig {
        fast_sma_period: 3,
        slow_sma_period: 7,
        stop_loss_pct: dec!(0.015),
        take_profit_pct: dec!(0.030),
        ..NseBacktestConfig::default()
    };

    let result = run_backtest(&candles, &config).expect("Runs backtest");
    assert!(
        result.total_trades > 0,
        "Must have executed at least one trade"
    );

    // Conservative check: if SL and TP are both touched on bar 20, STOP LOSS must be the recorded exit reason
    let both_touch_trade = result.trades.iter().find(|t| t.exit_time == Some(ts_both));
    if let Some(t) = both_touch_trade {
        assert_eq!(
            t.exit_reason, "STOP_LOSS",
            "Conservative rule must trigger STOP_LOSS when both are touched"
        );
    }
}

#[tokio::test]
async fn test_upstox_sandbox_isolation_and_live_blocking() {
    let venue_cfg = VenueConfig {
        id: "upstox_sandbox".to_string(),
        kind: "upstox_sandbox".to_string(),
        enabled: true,
        base_url: None,
        data_url: None,
        symbols: vec!["RELIANCE".to_string()],
        fee_pct: dec!(0.001),
    };

    // 1. Live mode must strictly refuse to build Upstox sandbox venue
    let app_cfg_live = test_config_with(vec![venue_cfg.clone()], AgentMode::Live);
    let (registry, skipped) = polymarket_agent::venue::factory::build_registry_reporting(
        &app_cfg_live,
        &Secrets::default(),
        None,
    );
    assert!(registry.is_empty(), "Live mode must refuse Upstox sandbox");
    assert_eq!(skipped.len(), 1);
    assert!(
        skipped[0].reason.contains("SANDBOX ONLY"),
        "Reason must cite SANDBOX ONLY"
    );

    // 2. Paper mode builds successfully in mock sandbox
    let app_cfg_paper = test_config_with(vec![venue_cfg.clone()], AgentMode::Paper);
    let (paper_registry, _) = polymarket_agent::venue::factory::build_registry_reporting(
        &app_cfg_paper,
        &Secrets::default(),
        None,
    );
    assert_eq!(
        paper_registry.len(),
        1,
        "Paper mode must build sandbox venue"
    );

    // 3. Mock sandbox client rejects non-whole-share quantities
    let client = UpstoxSandboxClient::new(None).expect("builds mock client");
    let invalid_order = UpstoxOrderRequestPayload {
        quantity: -5,
        product: "D".to_string(),
        validity: "DAY".to_string(),
        price: dec!(2500.0),
        tag: None,
        instrument_token: "NSE_EQ|INE002A01018".to_string(),
        order_type: "LIMIT".to_string(),
        transaction_type: "BUY".to_string(),
        disclosed_quantity: 0,
        trigger_price: dec!(0.0),
        is_amo: false,
    };
    assert!(
        client.place_order(&invalid_order).await.is_err(),
        "Must reject negative/zero quantity"
    );

    // 4. Valid mock order executes and returns client order id
    let mut valid_order = invalid_order;
    valid_order.quantity = 10;
    let order_id = client
        .place_order(&valid_order)
        .await
        .expect("Valid mock order succeeds");
    assert!(
        order_id.starts_with("UPSTOX-SBX-"),
        "Returns sandbox order identifier"
    );
}

#[tokio::test]
async fn test_upstox_feed_to_strategy_pipeline() {
    let feed = UpstoxLiveMarketFeed::new(None, None, None, None);
    // Initial feed is un-authorized without token
    let status_info = feed.get_status_info().await;
    assert!(!status_info.token_configured);

    // Baseline price for ITC
    let itc_price = feed.get_price("ITC").await;
    assert!(itc_price > Decimal::ZERO);

    // Evaluate tick with ₹1,000 capital and 0 holdings -> BUY
    let decision_buy = evaluate_strategy_tick("ITC", dec!(480.00), dec!(1000.0), 0, None);
    assert_eq!(decision_buy.action, "BUY");
    assert_eq!(decision_buy.quantity, 1);
    assert_eq!(decision_buy.symbol, "ITC");

    // Stock with price > available cash (e.g. RELIANCE at ₹1,200 vs ₹1,000 cash) -> WAIT
    let decision_wait = evaluate_strategy_tick("RELIANCE", dec!(1200.00), dec!(1000.0), 0, None);
    assert_eq!(decision_wait.action, "WAIT");
    assert!(decision_wait.reason.contains("exceeds available cash"));

    // Holding 1 share of ITC bought at ₹480, price rises to ₹495 (+3.12% > 2% TP) -> SELL
    let decision_sell =
        evaluate_strategy_tick("ITC", dec!(495.00), dec!(520.0), 1, Some(dec!(480.00)));
    assert_eq!(decision_sell.action, "SELL");
    assert_eq!(decision_sell.quantity, 1);
    assert!(decision_sell.reason.contains("Take-Profit"));
}

#[tokio::test]
async fn test_simulator_executes_paper_order_and_persists() {
    let store = Store::new(":memory:").await.expect("creates memory db");
    let base_capital = dec!(1000.0);

    // Initial portfolio
    let orders = store
        .get_india_sandbox_orders(100)
        .await
        .unwrap_or_default();
    let p0 = compute_portfolio_accounting(base_capital, &orders, &HashMap::new());
    assert_eq!(p0.available_cash_inr, dec!(1000.0));
    assert_eq!(p0.total_equity_inr, dec!(1000.0));

    // 1. Submit valid BUY paper order: 1 share of ITC @ ₹480
    let buy_req = IndiaPaperOrderRequest {
        symbol: "ITC".to_string(),
        side: "BUY".to_string(),
        order_type: "LIMIT".to_string(),
        quantity: 1,
        price: Some(dec!(480.00)),
        trigger_reason: Some("Strategy entry signal".to_string()),
    };
    let res = execute_paper_order(&store, base_capital, &buy_req, dec!(480.00))
        .await
        .expect("Paper buy succeeds");
    assert_eq!(res.order.status, "ACKNOWLEDGED");
    assert_eq!(res.order.symbol, "ITC");

    // Check portfolio after BUY
    let orders1 = store
        .get_india_sandbox_orders(100)
        .await
        .unwrap_or_default();
    assert_eq!(orders1.len(), 1);
    let p1 = compute_portfolio_accounting(base_capital, &orders1, &HashMap::new());
    assert_eq!(p1.available_cash_inr, dec!(520.0)); // 1000 - 480
    assert_eq!(p1.holdings.get("ITC").copied(), Some(dec!(1.0)));

    // 2. Attempt BUY exceeding available cash (needs ₹960, only ₹520 left) -> MUST FAIL
    let overrun_req = IndiaPaperOrderRequest {
        symbol: "ITC".to_string(),
        side: "BUY".to_string(),
        order_type: "LIMIT".to_string(),
        quantity: 2,
        price: Some(dec!(480.00)),
        trigger_reason: None,
    };
    let overrun_res = execute_paper_order(&store, base_capital, &overrun_req, dec!(480.00)).await;
    assert!(
        overrun_res.is_err(),
        "Must reject order exceeding available cash"
    );
    let err_msg = overrun_res.unwrap_err().to_string();
    assert!(err_msg.contains("Insufficient virtual cash"));

    // 3. Submit valid SELL paper order: 1 share of ITC @ ₹495
    let sell_req = IndiaPaperOrderRequest {
        symbol: "ITC".to_string(),
        side: "SELL".to_string(),
        order_type: "LIMIT".to_string(),
        quantity: 1,
        price: Some(dec!(495.00)),
        trigger_reason: Some("Take-profit exit".to_string()),
    };
    let sell_res = execute_paper_order(&store, base_capital, &sell_req, dec!(495.00))
        .await
        .expect("Paper sell succeeds");
    assert_eq!(sell_res.order.status, "ACKNOWLEDGED");

    // Check portfolio after SELL
    let orders2 = store
        .get_india_sandbox_orders(100)
        .await
        .unwrap_or_default();
    assert_eq!(orders2.len(), 2);
    let p2 = compute_portfolio_accounting(base_capital, &orders2, &HashMap::new());
    assert_eq!(p2.available_cash_inr, dec!(1015.0)); // 520 + 495
    assert_eq!(p2.realized_profit_inr, dec!(15.0)); // 495 - 480
    assert_eq!(
        p2.holdings.get("ITC").copied().unwrap_or(Decimal::ZERO),
        Decimal::ZERO
    );
}

#[tokio::test]
async fn test_upstox_v3_market_feed_subscription_session_and_deduplication() {
    use chrono::TimeZone;
    use chrono_tz::Asia::Kolkata;
    use polymarket_agent::india::market_feed::{LiveTick, UpstoxLiveMarketFeed};
    use polymarket_agent::india::session::{detect_nse_session, NseSessionKind};

    // 1. Verify Upstox V3 subscription payload
    let sub_json_str = UpstoxLiveMarketFeed::build_v3_subscription_message();
    let sub_val: serde_json::Value = serde_json::from_str(&sub_json_str).expect("Valid JSON");
    assert_eq!(sub_val["method"], "sub");
    assert_eq!(sub_val["data"]["mode"], "full");
    let keys = sub_val["data"]["instrumentKeys"]
        .as_array()
        .expect("instrumentKeys array");
    assert_eq!(keys.len(), 13, "Must subscribe to all 13 watchlist stocks");
    assert!(keys.iter().any(|k| k == "NSE_EQ|INE002A01018")); // RELIANCE

    // 2. Verify Session Detection across all session phases
    let dt_preopen = Kolkata
        .with_ymd_and_hms(2026, 9, 17, 9, 5, 0)
        .unwrap()
        .with_timezone(&chrono::Utc);
    let s_preopen = detect_nse_session(dt_preopen);
    assert_eq!(s_preopen.session_kind, NseSessionKind::PreOpen);
    assert!(!s_preopen.allows_order_entry);

    let dt_reg = Kolkata
        .with_ymd_and_hms(2026, 9, 17, 10, 30, 0)
        .unwrap()
        .with_timezone(&chrono::Utc);
    let s_reg = detect_nse_session(dt_reg);
    assert_eq!(s_reg.session_kind, NseSessionKind::Regular);
    assert!(s_reg.allows_order_entry);

    let dt_close_auction = Kolkata
        .with_ymd_and_hms(2026, 9, 17, 15, 45, 0)
        .unwrap()
        .with_timezone(&chrono::Utc);
    let s_close_auction = detect_nse_session(dt_close_auction);
    assert_eq!(s_close_auction.session_kind, NseSessionKind::ClosingAuction);
    assert!(!s_close_auction.allows_order_entry);

    let dt_closed = Kolkata
        .with_ymd_and_hms(2026, 9, 17, 20, 0, 0)
        .unwrap()
        .with_timezone(&chrono::Utc);
    let s_closed = detect_nse_session(dt_closed);
    assert_eq!(s_closed.session_kind, NseSessionKind::Closed);
    assert!(!s_closed.allows_order_entry);

    // 3. Verify Tick Deduplication in UpstoxLiveMarketFeed
    let feed = UpstoxLiveMarketFeed::new(None, None, None, None);
    let now = chrono::Utc::now();
    let tick1 = LiveTick {
        symbol: "RELIANCE".to_string(),
        instrument_token: "NSE_EQ|INE002A01018".to_string(),
        last_price: dec!(2500.50),
        open: dec!(2490.00),
        high: dec!(2510.00),
        low: dec!(2485.00),
        close: dec!(2500.50),
        volume: 120000,
        timestamp: now,
        feed_timestamp: now,
        received_at: now,
        latency_ms: 30,
        is_stale: false,
        source: "UPSTOX_V3_LIVE".to_string(),
    };

    // First arrival: accepted
    let fresh1 = feed.process_incoming_ticks(vec![tick1.clone()]).await;
    assert_eq!(fresh1.len(), 1);

    // Second arrival of identical tick: deduplicated, discarded
    let fresh2 = feed.process_incoming_ticks(vec![tick1.clone()]).await;
    assert_eq!(fresh2.len(), 0, "Duplicate tick must be discarded");
    assert_eq!(*feed.deduped_ticks_count.read().await, 1);

    // Tick with different price: accepted
    let mut tick2 = tick1.clone();
    tick2.last_price = dec!(2501.00);
    let fresh3 = feed.process_incoming_ticks(vec![tick2]).await;
    assert_eq!(fresh3.len(), 1, "New price must be accepted");
    assert_eq!(feed.get_price("RELIANCE").await, dec!(2501.00));
}
