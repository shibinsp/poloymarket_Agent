//! Dry run of the NSE momentum rotation: prints the regime, the ranking and
//! the target set from completed daily bars. Places no orders.
//!
//! cargo run --example india_momentum_snapshot

use anyhow::Result;
use polymarket_agent::india::instruments::nse_watchlist;
use polymarket_agent::india::market_feed::UpstoxLiveMarketFeed;
use polymarket_agent::india::momentum::{select_targets, MomentumConfig, NIFTY_50_KEY};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let feed = UpstoxLiveMarketFeed::new(
        None,
        None,
        std::env::var("UPSTOX_ACCESS_TOKEN").ok(),
        None,
    );
    let config = MomentumConfig::default();
    let today = chrono::Utc::now()
        .with_timezone(&chrono_tz::Asia::Kolkata)
        .date_naive();
    let days = config.lookback_days.max(config.regime_sma_days) + 10;

    let nifty = feed.fetch_daily_closes(NIFTY_50_KEY, days, today).await?;
    let mut closes = HashMap::new();
    for instrument in nse_watchlist() {
        match feed
            .fetch_daily_closes(&instrument.upstox_key, days, today)
            .await
        {
            Ok(series) => {
                println!("{:<11} {} daily bars", instrument.symbol, series.len());
                closes.insert(instrument.symbol, series);
            }
            Err(e) => println!("{:<11} no history: {e:#}", instrument.symbol),
        }
    }
    let d = select_targets(&closes, &nifty, &config);
    println!(
        "\nNIFTY 50 close {:?}  200-day SMA {:?}  regime {}",
        d.nifty_close,
        d.nifty_sma.map(|s| s.round_dp(1)),
        if d.regime_ok {
            "ON (may hold stocks)"
        } else {
            "OFF (cash)"
        }
    );
    println!("\n{}-session momentum ranking:", config.lookback_days);
    for (symbol, m) in &d.ranked {
        println!("  {symbol:<11} {:>7.2}%", *m * rust_decimal_macros::dec!(100));
    }
    println!("\nTarget holdings: {:?}", d.targets);
    Ok(())
}
