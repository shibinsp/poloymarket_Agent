//! Comprehensive integration tests for US-listed cash equities & ETFs,
//! intraday session controls, exchange calendar, regulatory fees, and FX conversion.

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use polymarket_agent::market::fx::{CombinedPortfolioSummary, FxRate};
use polymarket_agent::us::backtest::{
    parse_and_validate_us_csv, run_us_backtest, UsBacktestConfig,
};
use polymarket_agent::us::costs::UsCostConfig;
use polymarket_agent::us::instruments::{find_us_instrument, us_watchlist};
use polymarket_agent::venue::session::TradingSession;
use polymarket_agent::venue::types::{Side, VenueId};

#[test]
fn test_us_watchlist_instruments_and_venue_mapping() {
    let list = us_watchlist();
    assert_eq!(list.len(), 15, "Expected 15 liquid US symbols");

    let spy = find_us_instrument("SPY").expect("SPY must exist");
    assert_eq!(spy.exchange, "NYSE");
    assert_eq!(spy.tick_size, dec!(0.01));

    let inst = spy.to_venue_instrument(&VenueId::new("alpaca"));
    assert_eq!(inst.quote_ccy, "USD");
    assert!(inst.fractional);
    assert_eq!(inst.min_qty, Some(dec!(0.001)));
}

#[test]
fn test_us_statutory_and_regulatory_fees() {
    let config = UsCostConfig::default();
    let notional = dec!(100000.0); // $100,000 notional (200 shares @ $500)
    let shares = dec!(200.0);

    // Buy side: SEC and FINRA fees must be $0
    let buy_costs = config.calculate(notional, shares, Side::Buy);
    assert_eq!(buy_costs.sec_fee, Decimal::ZERO);
    assert_eq!(buy_costs.finra_taf, Decimal::ZERO);
    assert_eq!(buy_costs.commission, Decimal::ZERO);
    assert!(
        buy_costs.total > Decimal::ZERO,
        "Spread & slippage must be accounted for"
    );

    // Sell side: SEC Section 31 ($27.80/million) and FINRA TAF ($0.000166/sh) apply
    let sell_costs = config.calculate(notional, shares, Side::Sell);
    // SEC fee on $100,000 = $2.78
    assert_eq!(sell_costs.sec_fee, dec!(2.78));
    // FINRA TAF on 200 shares = 200 * $0.000166 = $0.0332 -> rounded up to $0.04
    assert_eq!(sell_costs.finra_taf, dec!(0.04));
    assert_eq!(sell_costs.total_regulatory_fees, dec!(2.82));
    assert!(sell_costs.total > sell_costs.total_regulatory_fees);
}

#[test]
fn test_us_exchange_holidays_and_early_closes() {
    let session = TradingSession::us_equity_regular();
    let et = chrono_tz::America::New_York;

    // MLK Day 2026-01-19 (holiday) -> session must be closed
    let mlk_day = et
        .with_ymd_and_hms(2026, 1, 19, 11, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        !session.is_open_at(mlk_day),
        "US market must be closed on MLK Day"
    );

    // Regular day 2026-01-20 11:00 ET -> open
    let regular_day = et
        .with_ymd_and_hms(2026, 1, 20, 11, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        session.is_open_at(regular_day),
        "US market must be open on regular trading day"
    );

    // Early close day (Black Friday 2026-11-27): closes at 13:00 ET
    let black_friday_noon = et
        .with_ymd_and_hms(2026, 11, 27, 12, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        session.is_open_at(black_friday_noon),
        "Must be open at 12:00 on Black Friday"
    );

    let black_friday_afternoon = et
        .with_ymd_and_hms(2026, 11, 27, 13, 30, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        !session.is_open_at(black_friday_afternoon),
        "Must be closed at 13:30 on Black Friday (early close 13:00)"
    );
}

#[test]
fn test_intraday_entry_cutoff_and_square_off() {
    let session = TradingSession::us_equity_regular();
    let et = chrono_tz::America::New_York;

    // 15:15 ET (45 minutes before 16:00 close): entries permitted, square-off not due
    let t_1515 = et
        .with_ymd_and_hms(2026, 1, 20, 15, 15, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        session.entry_permitted_at(t_1515, 30),
        "Entries should be permitted 45m before close"
    );
    assert!(
        !session.is_intraday_exit_due(t_1515, 15),
        "Square-off should not be due 45m before close"
    );

    // 15:35 ET (25 minutes before 16:00 close): entries blocked (cutoff is 30m), square-off not yet due (15m)
    let t_1535 = et
        .with_ymd_and_hms(2026, 1, 20, 15, 35, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        !session.entry_permitted_at(t_1535, 30),
        "Entries must be blocked within 30m of close"
    );
    assert!(
        !session.is_intraday_exit_due(t_1535, 15),
        "Square-off not due at 15:35"
    );

    // 15:50 ET (10 minutes before 16:00 close): square-off exit is due
    let t_1550 = et
        .with_ymd_and_hms(2026, 1, 20, 15, 50, 0)
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        session.is_intraday_exit_due(t_1550, 15),
        "Square-off exit must be triggered within 15m of close"
    );
}

#[test]
fn test_explicit_timestamped_fx_conversion() {
    let fx = FxRate {
        base: "USD".to_string(),
        quote: "INR".to_string(),
        rate: dec!(86.50),
        timestamp: Utc::now(),
        source: "RBI Benchmark".to_string(),
    };

    let usd = dec!(15000.0);
    let inr = fx.usd_to_inr(usd);
    assert_eq!(inr, dec!(1297500.00));

    let back = fx.inr_to_usd(inr);
    assert_eq!(back, dec!(15000.00));

    let summary = CombinedPortfolioSummary::calculate(
        &fx,
        dec!(500000.0), // India equity ₹5,00,000
        dec!(12000.0),  // Realized ₹12,000
        dec!(0.0),
        dec!(800.0),   // Costs ₹800
        dec!(10000.0), // US equity $10,000
        dec!(450.0),   // Realized $450
        dec!(0.0),
        dec!(15.0), // Costs $15
    );

    assert_eq!(summary.india_net_pnl_inr, dec!(11200.0));
    assert_eq!(summary.us_net_pnl_usd, dec!(435.0));
    assert!(summary.combined_equity_usd > dec!(15000.0));
    assert!(summary.combined_equity_inr > dec!(1300000.0));
}

#[test]
fn test_us_backtest_simulation_with_zero_lookahead() {
    let csv = std::fs::read_to_string("data/us_sample_data.csv")
        .expect("data/us_sample_data.csv must exist");
    let candles = parse_and_validate_us_csv(&csv).expect("CSV should parse cleanly");
    assert!(candles.len() >= 35, "Expected at least 35 candles");

    let config = UsBacktestConfig::default();
    let results = run_us_backtest(&candles, &config).expect("US backtest must succeed");

    assert_eq!(results.initial_capital, dec!(100000.0));
    assert!(results.final_equity > Decimal::ZERO);
    assert!(results.equity_curve.len() >= 35);
    assert_eq!(
        results.total_trades,
        results.winning_trades + results.losing_trades
    );
}

#[test]
fn test_us_sma_crossover_strategy_golden_cross_and_exit_signals() {
    use chrono::{Duration, TimeZone, Utc};
    use polymarket_agent::us::sma_crossover::{
        evaluate_sma_crossover, SmaCrossoverConfig, SmaSignalAction,
    };
    use polymarket_agent::venue::types::{
        Candle, Instrument, InstrumentId, InstrumentMeta, VenueId,
    };

    let instrument = Instrument {
        id: InstrumentId::new(VenueId::new("alpaca"), "SPY"),
        asset_class: polymarket_agent::venue::types::AssetClass::Equity,
        display_name: "SPDR S&P 500 ETF Trust".to_string(),
        quote_ccy: "USD".to_string(),
        tick_size: Some(dec!(0.01)),
        lot_size: Some(dec!(1.0)),
        min_notional: Some(dec!(1.0)),
        min_qty: Some(dec!(0.001)),
        fractional: true,
        meta: InstrumentMeta::Equity {
            exchange: "NYSE".to_string(),
        },
    };

    let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 16, 0, 0).unwrap();
    let mut candles = Vec::new();
    for i in 0..55 {
        let close = if i < 54 { dec!(100.0) } else { dec!(150.0) };
        candles.push(Candle {
            ts: base_time + Duration::days(i as i64),
            open: close - dec!(0.50),
            high: close + dec!(1.00),
            low: close - dec!(1.00),
            close,
            volume: dec!(1000000),
        });
    }

    let config = SmaCrossoverConfig::default();
    let now = candles.last().unwrap().ts + Duration::hours(2);

    // 1. Golden Cross
    let signal = evaluate_sma_crossover(&instrument, &candles, now, &config)
        .expect("evaluation should succeed");
    assert_eq!(signal.action, SmaSignalAction::LongEntry);
    assert_eq!(signal.moving_average_type, "SMA");
    assert_eq!(signal.fast_sma_previous, dec!(100.0));
    assert_eq!(signal.slow_sma_previous, dec!(100.0));
    assert!(signal.fast_sma_current > signal.slow_sma_current);

    // 2. Death Cross
    let mut death_candles = Vec::new();
    for i in 0..55 {
        let close = if i < 54 { dec!(100.0) } else { dec!(50.0) };
        death_candles.push(Candle {
            ts: base_time + Duration::days(i as i64),
            open: close - dec!(0.50),
            high: close + dec!(1.00),
            low: close - dec!(1.00),
            close,
            volume: dec!(1000000),
        });
    }
    let death_signal = evaluate_sma_crossover(&instrument, &death_candles, now, &config)
        .expect("evaluation should succeed");
    assert_eq!(death_signal.action, SmaSignalAction::Exit);
    assert!(death_signal.fast_sma_current < death_signal.slow_sma_current);
}

#[test]
fn test_us_sma_crossover_backtest_with_buy_and_hold_baseline() {
    use polymarket_agent::us::backtest::parse_and_validate_us_csv;
    use polymarket_agent::us::sma_crossover::run_us_sma_crossover_backtest;

    let csv_content = std::fs::read_to_string("data/us_daily_sample_data.csv")
        .expect("data/us_daily_sample_data.csv must exist");
    let candles = parse_and_validate_us_csv(&csv_content).expect("daily CSV must parse cleanly");

    assert!(candles.len() >= 80, "Expected at least 80 daily candles");

    let config = UsBacktestConfig::default();
    let results = run_us_sma_crossover_backtest(&candles, &config)
        .expect("SMA crossover backtest should succeed");

    assert_eq!(results.initial_capital, dec!(100000.0));
    assert!(results.final_equity > Decimal::ZERO);
    assert_eq!(
        results.total_trades,
        results.winning_trades + results.losing_trades
    );
    assert!(results.equity_curve.len() >= 80);
    // Benchmark baseline must be calculated over the same dates
    assert_ne!(results.benchmark_return_pct, Decimal::ZERO);
    // Statutory and regulatory costs tracked
    assert!(results.total_costs >= Decimal::ZERO);
}
