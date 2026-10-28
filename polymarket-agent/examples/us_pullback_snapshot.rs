use anyhow::Result;
use polymarket_agent::strategy::{backtest::run_pullback_backtest, pullback::PullbackStrategyConfig};
use polymarket_agent::us::market_feed::AlpacaLiveMarketFeed;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

struct Candidate { name: &'static str, config: PullbackStrategyConfig, train_pnl_pct: Decimal, train_trades: usize }

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let feed = AlpacaLiveMarketFeed::new(None, None, None);
    let feed_name = std::env::var("ALPACA_FEED_TYPE").unwrap_or_else(|_| "iex".to_string());
    let base = PullbackStrategyConfig::default();
    let mut variants = vec![("baseline", base.clone())];
    for (name, fast, slow, pullback, rr) in [
        ("tight_20_50_rr15", 20, 50, dec!(0.005), dec!(1.5)),
        ("tight_20_50_rr20", 20, 50, dec!(0.005), dec!(2.0)),
        ("medium_20_50_rr20", 20, 50, dec!(0.010), dec!(2.0)),
        ("tight_10_30_rr15", 10, 30, dec!(0.005), dec!(1.5)),
        ("medium_10_30_rr20", 10, 30, dec!(0.010), dec!(2.0)),
    ] {
        let mut c = base.clone();
        c.ema_fast_period = fast;
        c.ema_slow_period = slow;
        c.max_pullback_pct_from_vwap = pullback;
        c.reward_to_risk_ratio = rr;
        variants.push((name,c));
    }
    let mut candidates: Vec<Candidate> = variants.into_iter().map(|(name,config)| Candidate{name,config,train_pnl_pct:Decimal::ZERO,train_trades:0}).collect();
    let mut market_data = Vec::new();
    for symbol in ["SPY", "QQQ", "AAPL", "NVDA", "MSFT"] {
        let candles = feed.fetch_intraday_5m_candles(symbol).await?;
        println!("FETCH {symbol} feed={feed_name} regular_session_bars={} first={} last={}", candles.len(), candles.first().unwrap().timestamp, candles.last().unwrap().timestamp);
        market_data.push((symbol,candles));
    }
    for candidate in &mut candidates {
        for (symbol,candles) in &market_data {
            let train_end = candles.len() * 65 / 100;
            let report = run_pullback_backtest(&candles[..train_end], true, dec!(10000), &candidate.config, dec!(0.99));
            candidate.train_pnl_pct += report.overall_metrics.net_pnl_pct;
            candidate.train_trades += report.overall_metrics.trade_count;
            let _ = symbol;
        }
        println!("TRAIN {} aggregate_net_return_pct={} trades={}",candidate.name,candidate.train_pnl_pct,candidate.train_trades);
    }
    let selected = candidates.into_iter().filter(|c| c.train_trades >= 25).max_by_key(|c| c.train_pnl_pct).expect("a candidate with enough training trades");
    println!("SELECTED_ON_TRAIN_ONLY {} aggregate_net_return_pct={} trades={}",selected.name,selected.train_pnl_pct,selected.train_trades);
    let mut oos_total = Decimal::ZERO;
    let mut oos_count = 0usize;
    for (symbol,candles) in &market_data {
        let test_start = candles.len() * 65 / 100;
        let test = &candles[test_start..];
        let report = run_pullback_backtest(test, true, dec!(10000), &selected.config, dec!(0.0001));
        let m = &report.oos_period;
        println!("OOS {symbol} trades={} win_rate_pct={} net_usd={} net_return_pct={} max_drawdown_pct={} profit_factor={} expectancy_usd={} buy_hold_pct={}",m.trade_count,m.win_rate_pct,m.net_pnl,m.net_pnl_pct,m.max_drawdown_pct,m.profit_factor,m.expectancy_per_trade,report.baseline.buy_and_hold_return_pct);
        oos_total += m.net_pnl_pct;
        oos_count += m.trade_count;
    }
    println!("OOS_AGGREGATE selected={} mean_net_return_pct={} trades={} held_out_fraction=35%", selected.name, oos_total / Decimal::from(market_data.len()), oos_count);
    Ok(())
}
