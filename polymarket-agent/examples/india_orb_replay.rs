//! Replays today's NSE session through the intraday breakout rules on a paper
//! Rs 2,500, up to 3 trades, one at a time. Writes nothing and places no orders.
//!
//! cargo run --example india_orb_replay

use anyhow::Result;
use chrono::{Duration, Utc};
use chrono_tz::Asia::Kolkata;
use polymarket_agent::india::momentum::NIFTY_50_KEY;
use polymarket_agent::india::orb::{
    first_breakout, indexed, rank, round_trip, stop_fill, whole_shares, UpstoxBars,
};
use polymarket_agent::venue::types::Side;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let token = std::env::var("UPSTOX_ACCESS_TOKEN").ok();
    let data = UpstoxBars::new(Arc::new(RwLock::new(token)));
    let day = Utc::now().with_timezone(&Kolkata).date_naive();
    let nifty_bars = data.today(NIFTY_50_KEY, 5).await?;
    let nifty = indexed(&nifty_bars, day);
    let nifty_open = nifty.first().map(|(_, b)| b.open).unwrap_or_default();

    let in_play = rank(&data, day).await;
    println!("Session {day}: {} NIFTY 50 stocks in play\n", in_play.len());

    let mut signals = Vec::new();
    for p in &in_play {
        let five = data.today(&p.key, 5).await?;
        let bars = indexed(&five, day);
        let Some((i, side)) = first_breakout(p.high, p.low, &bars, 75) else {
            println!("{:<11} rvol {:>6}: no breakout by 11:30", p.symbol, p.rvol);
            continue;
        };
        let buy = side == Side::Buy;
        let agrees = nifty
            .iter()
            .find(|(k, _)| *k == i)
            .is_some_and(|(_, b)| (b.close > nifty_open) == buy);
        let mins = 9 * 60 + 15 + (i as i64 + 1) * 5;
        println!(
            "{:<11} rvol {:>6}: {} at {:02}:{:02}{}",
            p.symbol,
            p.rvol,
            if buy { "LONG" } else { "SHORT" },
            mins / 60,
            mins % 60,
            if agrees {
                ""
            } else {
                "  (against NIFTY, skipped)"
            }
        );
        if agrees {
            signals.push((i, p.clone(), buy));
        }
    }
    signals.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.rvol.cmp(&a.1.rvol)));

    let mut balance = dec!(2500);
    let mut busy_until = day.and_hms_opt(0, 0, 0).expect("midnight").and_utc();
    let mut trades = 0;
    println!();
    for (i, p, buy) in signals {
        if trades == 3 {
            break;
        }
        let one = data.today(&p.key, 1).await?;
        let session = day
            .and_hms_opt(3, 45, 0)
            .expect("9:15 IST in UTC")
            .and_utc();
        let entry_at = session + Duration::minutes((i as i64 + 1) * 5);
        if entry_at < busy_until {
            continue;
        }
        let Some(first) = one.iter().find(|b| b.ts >= entry_at) else {
            continue;
        };
        let slip = dec!(0.0003);
        let entry = (first.open
            * if buy {
                Decimal::ONE + slip
            } else {
                Decimal::ONE - slip
            })
        .round_dp(2);
        let stop = if buy { p.low } else { p.high };
        let qty = whole_shares(balance, entry, buy);
        if qty < 1 {
            println!(
                "{}: Rs {balance} cannot buy one share at {entry}; skipped",
                p.symbol
            );
            continue;
        }
        let flat = session + Duration::minutes(360);
        let path: Vec<_> = one
            .iter()
            .filter(|b| b.ts >= entry_at && b.ts < flat)
            .collect();
        let hit = path
            .iter()
            .find_map(|b| stop_fill(buy, stop, b).map(|px| (b.ts, px)));
        let (out_at, px, why) = match hit {
            Some((t, px)) => (t, px, "stop"),
            None => (flat, path.last().map(|b| b.close).unwrap_or(entry), "15:15"),
        };
        let exit = (px
            * if buy {
                Decimal::ONE - slip
            } else {
                Decimal::ONE + slip
            })
        .round_dp(2);
        let (pnl, charges) = round_trip(buy, qty, entry, exit);
        balance += pnl;
        busy_until = out_at;
        trades += 1;
        println!(
            "{} {} x{qty}: in {entry}, stop {stop}, out {exit} ({why}), charges Rs {charges}, P&L Rs {pnl}",
            if buy { "LONG" } else { "SHORT" },
            p.symbol
        );
    }
    println!("\n{trades} trades. Rs 2,500 -> Rs {}", balance.round_dp(2));
    Ok(())
}
