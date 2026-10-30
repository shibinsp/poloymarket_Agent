//! Replays the U.S. opening-range breakout for one session with the live
//! data client: which stocks were in play, and where each broke out. Places
//! no orders.
//!
//! cargo run --example orb_dry_run -- [YYYY-MM-DD]   (default: the latest session)

use anyhow::Result;
use chrono::{Duration, NaiveDate, Utc};
use polymarket_agent::config::AppConfig;
use polymarket_agent::us::orb::{first_breakout, relative_volume, session_slot, AlpacaBars};
use rust_decimal::Decimal;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let (config, _secrets) = AppConfig::load()?;
    let cfg = &config.us_orb;
    let feed = std::env::var("ALPACA_FEED_TYPE").unwrap_or_else(|_| "iex".to_string());
    let data = AlpacaBars::new(
        std::env::var("ALPACA_API_KEY_ID")?,
        std::env::var("ALPACA_API_SECRET_KEY")?,
        feed.to_ascii_lowercase(),
    );
    let history = data
        .bars(&cfg.symbols, Utc::now() - Duration::days(35))
        .await?;
    let wanted: Option<NaiveDate> = std::env::args().nth(1).and_then(|d| d.parse().ok());
    let day = wanted.unwrap_or_else(|| {
        history
            .values()
            .flat_map(|l| l.iter().filter_map(|b| session_slot(b.ts).map(|s| s.0)))
            .max()
            .expect("some bars")
    });

    println!("{} symbols, session {day}, feed {feed}\n", history.len());
    let mut in_play = Vec::new();
    for symbol in &cfg.symbols {
        let Some(list) = history.get(symbol) else {
            continue;
        };
        let prior: Vec<Decimal> = list
            .iter()
            .filter(|b| matches!(session_slot(b.ts), Some((d, 0)) if d < day))
            .map(|b| b.volume)
            .collect();
        let prior = &prior[prior.len().saturating_sub(14)..];
        let today: Vec<(usize, _)> = list
            .iter()
            .filter_map(|b| {
                session_slot(b.ts)
                    .filter(|(d, _)| *d == day)
                    .map(|(_, i)| (i, *b))
            })
            .collect();
        let Some((_, first)) = today.iter().find(|(i, _)| *i == 0) else {
            continue;
        };
        let Some(rvol) = relative_volume(first.volume, prior) else {
            continue;
        };
        if rvol < cfg.rvol_min || first.open < cfg.min_price {
            continue;
        }
        let signal = first_breakout(first.high, first.low, &today, |_| true);
        in_play.push((rvol, symbol.clone(), first.high, first.low, signal));
    }
    in_play.sort_by_key(|x| std::cmp::Reverse(x.0));
    println!(
        "{:<6} {:>6} {:>10} {:>10}  breakout",
        "symbol", "rvol", "range hi", "range lo"
    );
    for (rvol, s, hi, lo, sig) in &in_play {
        let when = sig
            .map(|(i, side)| {
                let mins = 9 * 60 + 30 + (i as i64 + 1) * 5;
                format!(
                    "{} at {:02}:{:02} ET",
                    if side.to_string() == "BUY" {
                        "LONG"
                    } else {
                        "SHORT"
                    },
                    mins / 60,
                    mins % 60
                )
            })
            .unwrap_or_else(|| "none".to_string());
        println!("{s:<6} {:>6.2} {hi:>10} {lo:>10}  {when}", rvol);
    }
    println!(
        "\n{} stocks in play (rvol >= {}); the agent takes the first {} signals by time.",
        in_play.len(),
        cfg.rvol_min,
        cfg.max_trades_per_day
    );
    Ok(())
}
