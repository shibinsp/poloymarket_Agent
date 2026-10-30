//! Compare the bare 20/50 SMA crossover against the filtered strategy
//! (volume, ADX and SPY-regime entry filters plus breakeven/trailing stops)
//! on Alpaca daily history.
//!
//! cargo run --example us_sma_compare -- [years] [SYMBOL ...]

use anyhow::Result;
use chrono::{TimeZone, Utc};
use polymarket_agent::us::backtest::{UsBacktestConfig, UsBacktestResults, UsCandle};
use polymarket_agent::us::market_feed::AlpacaLiveMarketFeed;
use polymarket_agent::us::sma_crossover::{
    run_us_sma_crossover_backtest_with_regime, SmaEntryFilters, SmaTrailingStop,
    DEFAULT_FAST_SMA_PERIOD, DEFAULT_SLOW_SMA_PERIOD,
};

const DEFAULT_SYMBOLS: [&str; 9] = [
    "SPY", "QQQ", "AAPL", "MSFT", "NVDA", "AMZN", "META", "GOOGL", "TSLA",
];

fn variants() -> Vec<(&'static str, SmaEntryFilters, SmaTrailingStop)> {
    vec![
        (
            "bare crossover",
            SmaEntryFilters::disabled(),
            SmaTrailingStop::disabled(),
        ),
        (
            "+ entry filters",
            SmaEntryFilters::default(),
            SmaTrailingStop::disabled(),
        ),
        (
            "  regime only",
            SmaEntryFilters {
                regime_sma_period: 200,
                ..SmaEntryFilters::disabled()
            },
            SmaTrailingStop::disabled(),
        ),
        (
            "  close > SMA50 only",
            SmaEntryFilters {
                require_close_above_slow_sma: true,
                ..SmaEntryFilters::disabled()
            },
            SmaTrailingStop::disabled(),
        ),
        (
            "  volume only",
            SmaEntryFilters {
                min_volume_ratio: rust_decimal_macros::dec!(1.2),
                ..SmaEntryFilters::disabled()
            },
            SmaTrailingStop::disabled(),
        ),
        (
            "  ADX only",
            SmaEntryFilters {
                min_adx: SmaEntryFilters::default().min_adx,
                ..SmaEntryFilters::disabled()
            },
            SmaTrailingStop::disabled(),
        ),
        (
            "+ trailing stop",
            SmaEntryFilters::disabled(),
            SmaTrailingStop::default(),
        ),
        (
            "filters + trailing",
            SmaEntryFilters::default(),
            SmaTrailingStop::default(),
        ),
    ]
}

fn print_row(name: &str, r: &UsBacktestResults) {
    println!(
        "{:<20} {:>6} {:>7.1}% {:>6} {:>12.2} {:>8.2}% {:>7.2}%",
        name,
        r.total_trades,
        r.win_rate_pct,
        r.profit_factor,
        r.net_profit,
        r.roi_pct * rust_decimal_macros::dec!(100),
        r.max_drawdown_pct * rust_decimal_macros::dec!(100),
    );
}

fn run_window(title: &str, candles: &[UsCandle], regime: &[UsCandle]) -> Result<()> {
    println!("\n== {title} ==");
    println!(
        "{:<20} {:>6} {:>8} {:>6} {:>12} {:>9} {:>8}",
        "variant", "trades", "win", "PF", "net P&L $", "ROI", "max DD"
    );
    let mut benchmark = None;
    for (name, filters, trailing) in variants() {
        let config = UsBacktestConfig {
            fast_sma_period: DEFAULT_FAST_SMA_PERIOD,
            slow_sma_period: DEFAULT_SLOW_SMA_PERIOD,
            sma_entry_filters: filters,
            sma_trailing_stop: trailing,
            ..UsBacktestConfig::default()
        };
        let results = run_us_sma_crossover_backtest_with_regime(candles, Some(regime), &config)?;
        print_row(name, &results);
        benchmark = Some(results.benchmark_return_pct);
    }
    if let Some(b) = benchmark {
        println!(
            "SPY buy-and-hold over the window: {:.2}%",
            b * rust_decimal_macros::dec!(100)
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let mut args = std::env::args().skip(1);
    let years: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(5);
    let mut symbols: Vec<String> = args.map(|s| s.to_ascii_uppercase()).collect();
    if symbols.is_empty() {
        symbols = DEFAULT_SYMBOLS.iter().map(|s| s.to_string()).collect();
    }

    let feed = AlpacaLiveMarketFeed::new(None, None, None);
    // One extra year of SPY so the 200-day regime is warm from the first day.
    let regime = feed
        .fetch_daily_history_candles("SPY", (years + 1).min(10))
        .await?;
    let mut candles = Vec::new();
    for symbol in &symbols {
        candles.extend(feed.fetch_daily_history_candles(symbol, years).await?);
    }
    println!(
        "{} symbols, {} daily bars, {} years, $100k capital, 10% per position, 50% max exposure",
        symbols.len(),
        candles.len(),
        years
    );

    run_window("Full history", &candles, &regime)?;

    // The 2022 bear market, with three months of lead-in for the 50-day SMA.
    let start = Utc.with_ymd_and_hms(2021, 10, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap();
    let bear: Vec<UsCandle> = candles
        .iter()
        .filter(|c| c.timestamp >= start && c.timestamp < end)
        .cloned()
        .collect();
    let per_symbol_ok = symbols.iter().all(|s| {
        bear.iter().filter(|c| &c.symbol == s).count()
            >= polymarket_agent::us::sma_crossover::MINIMUM_REQUIRED_DAILY_BARS
    });
    if per_symbol_ok {
        run_window("Oct 2021 - Dec 2022 (bear market)", &bear, &regime)?;
    }
    Ok(())
}
