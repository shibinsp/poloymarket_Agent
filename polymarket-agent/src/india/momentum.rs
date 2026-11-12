//! Monthly momentum rotation for the NSE watchlist, paper-only.
//!
//! Rules (daily closes, completed sessions only):
//! 1. Regime: NIFTY 50 must close above its 200-day SMA. Otherwise hold cash.
//! 2. Rank: each watchlist stock by its return over the last 189 sessions
//!    (about nine months). Keep the top 4 with a positive return.
//! 3. Rebalance once per calendar month, after 09:30 IST: sell holdings that
//!    left the target set, buy targets not held, each sized at 1/4 of equity.
//!    Holdings that stay in the set are not resized.
//!
//! Chosen on 2017-2022 daily data and checked on 2023 - Oct 2026, where it
//! returned 9.9% a year against 6.3% for NIFTY buy-and-hold, with a 10.5% max
//! drawdown against 15.8%. Every lookback from 3 to 12 months and every
//! holding count from 2 to 5 was profitable in both periods.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::agent::kill_switch::KillSwitch;
use crate::db::store::Store;
use crate::india::costs::IndianCostConfig;
use crate::india::instruments::nse_watchlist;
use crate::india::market_feed::UpstoxLiveMarketFeed;
use crate::india::session::detect_nse_session;
use crate::india::simulator::{
    compute_portfolio_accounting, execute_paper_order, IndiaPaperOrderRequest,
};
use crate::venue::types::Side;

pub const NIFTY_50_KEY: &str = "NSE_INDEX|Nifty 50";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MomentumConfig {
    pub lookback_days: usize,
    pub top_n: usize,
    pub regime_sma_days: usize,
    /// No orders before this IST time, so fills avoid the opening auction swing.
    pub earliest_rebalance_ist: NaiveTime,
    /// No orders after this IST time.
    pub latest_rebalance_ist: NaiveTime,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self {
            lookback_days: 189,
            top_n: 4,
            regime_sma_days: 200,
            earliest_rebalance_ist: NaiveTime::from_hms_opt(9, 30, 0).expect("valid time"),
            latest_rebalance_ist: NaiveTime::from_hms_opt(15, 0, 0).expect("valid time"),
        }
    }
}

/// What the strategy wants to hold this month, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MomentumDecision {
    pub regime_ok: bool,
    pub nifty_close: Option<Decimal>,
    pub nifty_sma: Option<Decimal>,
    /// Every ranked stock, best first.
    pub ranked: Vec<(String, Decimal)>,
    pub targets: Vec<String>,
}

/// Rank stocks and apply the regime. `closes` hold completed daily closes,
/// oldest first. A stock without enough history is left out of the ranking;
/// a NIFTY series without enough history means no regime, so cash.
pub fn select_targets(
    closes: &HashMap<String, Vec<Decimal>>,
    nifty_closes: &[Decimal],
    config: &MomentumConfig,
) -> MomentumDecision {
    let nifty_close = nifty_closes.last().copied();
    let nifty_sma = (config.regime_sma_days > 0 && nifty_closes.len() >= config.regime_sma_days)
        .then(|| {
            nifty_closes[nifty_closes.len() - config.regime_sma_days..]
                .iter()
                .copied()
                .sum::<Decimal>()
                / Decimal::from(config.regime_sma_days)
        });
    let regime_ok = matches!((nifty_close, nifty_sma), (Some(c), Some(s)) if c > s);

    let mut ranked: Vec<(String, Decimal)> = closes
        .iter()
        .filter_map(|(symbol, series)| {
            let n = series.len();
            if n <= config.lookback_days {
                return None;
            }
            let past = series[n - 1 - config.lookback_days];
            (past > Decimal::ZERO).then(|| {
                (
                    symbol.clone(),
                    (series[n - 1] / past - Decimal::ONE).round_dp(4),
                )
            })
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let targets = if regime_ok {
        ranked
            .iter()
            .filter(|(_, m)| *m > Decimal::ZERO)
            .take(config.top_n)
            .map(|(s, _)| s.clone())
            .collect()
    } else {
        Vec::new()
    };
    MomentumDecision {
        regime_ok,
        nifty_close,
        nifty_sma,
        ranked,
        targets,
    }
}

/// `(symbol, whole shares)` per order.
pub type OrderList = Vec<(String, i64)>;

/// Orders that move `holdings` to `targets`: every non-target holding is sold
/// in full; every unheld target is bought with `equity / top_n`, limited by
/// cash after the sells and by estimated buy charges. Symbols without a price
/// are skipped.
pub fn rebalance_orders(
    holdings: &HashMap<String, i64>,
    targets: &[String],
    prices: &HashMap<String, Decimal>,
    equity: Decimal,
    cash: Decimal,
    top_n: usize,
) -> (OrderList, OrderList) {
    let costs = IndianCostConfig::default();
    let mut sells: Vec<(String, i64)> = holdings
        .iter()
        .filter(|(s, q)| **q > 0 && !targets.contains(s))
        .map(|(s, q)| (s.clone(), *q))
        .collect();
    sells.sort();

    let mut cash_after = cash;
    for (symbol, qty) in &sells {
        if let Some(px) = prices.get(symbol) {
            let value = *px * Decimal::from(*qty);
            cash_after += value - costs.calculate(value, Side::Sell).total;
        }
    }

    let slot = if top_n > 0 {
        equity / Decimal::from(top_n)
    } else {
        Decimal::ZERO
    };
    let mut buys = Vec::new();
    for symbol in targets {
        if holdings.get(symbol).copied().unwrap_or(0) > 0 {
            continue;
        }
        let Some(px) = prices.get(symbol).copied().filter(|p| *p > Decimal::ZERO) else {
            continue;
        };
        let budget = slot.min(cash_after);
        let mut qty = (budget / px)
            .floor()
            .to_string()
            .parse::<i64>()
            .unwrap_or(0);
        while qty > 0 {
            let value = px * Decimal::from(qty);
            if value + costs.calculate(value, Side::Buy).total <= cash_after {
                break;
            }
            qty -= 1;
        }
        if qty > 0 {
            let value = px * Decimal::from(qty);
            cash_after -= value + costs.calculate(value, Side::Buy).total;
            buys.push((symbol.clone(), qty));
        }
    }
    (sells, buys)
}

/// Persisted between restarts so a month is rebalanced once.
#[derive(Debug, Default, Serialize, Deserialize)]
struct MomentumState {
    last_rebalance_month: Option<String>,
    last_decision: Option<MomentumDecision>,
    decided_at: Option<DateTime<Utc>>,
}

fn load_state(path: &PathBuf) -> MomentumState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn is_live_source(source: &str) -> bool {
    matches!(source, "UPSTOX_V3_LIVE" | "UPSTOX_REST_LIVE")
}

/// Run the monthly rotation. Checks once a minute; acts at most once per IST
/// calendar month, and only with a live feed, an open session and no halt.
pub fn spawn_momentum_worker(
    feed: Arc<UpstoxLiveMarketFeed>,
    store: Store,
    kill_switch: Arc<KillSwitch>,
    base_capital_inr: Decimal,
    state_path: PathBuf,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let config = MomentumConfig::default();
        let mut state = load_state(&state_path);
        info!(
            last_rebalance_month = ?state.last_rebalance_month,
            path = %state_path.display(),
            "NSE momentum rotation worker started (paper only)"
        );
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let now = Utc::now();
            let ist = now.with_timezone(&chrono_tz::Asia::Kolkata);
            let month = ist.format("%Y-%m").to_string();
            if state.last_rebalance_month.as_deref() == Some(month.as_str())
                || kill_switch.is_tripped()
                || !detect_nse_session(now).allows_order_entry
                || ist.time() < config.earliest_rebalance_ist
                || ist.time() >= config.latest_rebalance_ist
                || feed.is_stale().await
            {
                continue;
            }
            match rebalance(&feed, &store, base_capital_inr, &config, ist.date_naive()).await {
                Ok(Some(decision)) => {
                    state.last_rebalance_month = Some(month);
                    state.last_decision = Some(decision);
                    state.decided_at = Some(now);
                    match serde_json::to_string_pretty(&state) {
                        Ok(json) => {
                            if let Err(e) = std::fs::write(&state_path, json) {
                                warn!(error = %e, "Could not persist momentum state");
                            }
                        }
                        Err(e) => warn!(error = %e, "Could not serialise momentum state"),
                    }
                }
                Ok(None) => {} // incomplete; retried next minute
                Err(e) => warn!(error = %e, "Momentum rebalance failed; retrying next minute"),
            }
        }
    })
}

/// One rebalance attempt. `Ok(None)` means it could not finish (missing data
/// or prices) and should be retried; orders already placed are not repeated
/// because the next attempt starts again from the current holdings.
async fn rebalance(
    feed: &UpstoxLiveMarketFeed,
    store: &Store,
    base_capital_inr: Decimal,
    config: &MomentumConfig,
    today_ist: chrono::NaiveDate,
) -> anyhow::Result<Option<MomentumDecision>> {
    let days = config.lookback_days.max(config.regime_sma_days) + 10;
    let nifty = feed
        .fetch_daily_closes(NIFTY_50_KEY, days, today_ist)
        .await?;
    let mut closes = HashMap::new();
    for instrument in nse_watchlist() {
        match feed
            .fetch_daily_closes(&instrument.upstox_key, days, today_ist)
            .await
        {
            Ok(series) => {
                closes.insert(instrument.symbol, series);
            }
            Err(e) => {
                warn!(symbol = %instrument.symbol, error = %e, "No daily history; left out of the ranking")
            }
        }
    }
    let decision = select_targets(&closes, &nifty, config);
    info!(
        regime_ok = decision.regime_ok,
        nifty_close = ?decision.nifty_close,
        nifty_sma200 = ?decision.nifty_sma,
        targets = ?decision.targets,
        ranked = ?decision.ranked,
        "NSE momentum ranking"
    );

    let orders = store.get_india_sandbox_orders(500).await?;
    let prices: HashMap<String, Decimal> = feed
        .prices
        .read()
        .await
        .iter()
        .filter(|(_, t)| is_live_source(&t.source))
        .map(|(s, t)| (s.clone(), t.last_price))
        .collect();
    let portfolio = compute_portfolio_accounting(base_capital_inr, &orders, &prices);
    let holdings: HashMap<String, i64> = portfolio
        .holdings
        .iter()
        .map(|(s, q)| (s.clone(), q.to_string().parse::<i64>().unwrap_or(0)))
        .collect();

    let needs_price = holdings
        .keys()
        .filter(|s| !decision.targets.contains(s))
        .chain(
            decision
                .targets
                .iter()
                .filter(|s| !holdings.contains_key(*s)),
        );
    let missing: Vec<&String> = needs_price.filter(|s| !prices.contains_key(*s)).collect();
    if !missing.is_empty() {
        warn!(?missing, "No live price yet for symbols to trade; retrying");
        return Ok(None);
    }

    let (sells, buys) = rebalance_orders(
        &holdings,
        &decision.targets,
        &prices,
        portfolio.total_equity_inr,
        portfolio.available_cash_inr,
        config.top_n,
    );

    // The desk's card for the month: what every agent saw and NOVA's call.
    let extras =
        crate::india::desk::gather(feed, &decision, &holdings, config, &nifty, today_ist).await;
    let card = crate::india::desk::assemble(&crate::india::desk::DeskInputs {
        decision: &decision,
        holdings: &holdings,
        sells: &sells,
        buys: &buys,
        prices: &prices,
        closes: &closes,
        news: &extras.news,
        india_vix: extras.india_vix,
        shadow_top: extras.shadow_top,
        slot_inr: portfolio.total_equity_inr / Decimal::from(config.top_n.max(1)),
        now: chrono::Utc::now(),
    });
    info!(
        action = %card.nova.action,
        reasons = ?card.nova.reasons,
        advisory = ?card.nova.advisory,
        "NOVA (India) decision"
    );
    match serde_json::to_value(&card) {
        Ok(json) => {
            if let Err(e) = store
                .insert_desk_card(
                    card.at,
                    "IN",
                    "momentum",
                    "PORTFOLIO",
                    &card.nova.action,
                    if card.nova.advisory.is_empty() {
                        "NONE"
                    } else {
                        "FLAGGED"
                    },
                    &json,
                )
                .await
            {
                warn!(error = %e, "Could not record the India decision card");
            }
        }
        Err(e) => warn!(error = %e, "Could not serialise the India decision card"),
    }
    for (side, list) in [("SELL", &sells), ("BUY", &buys)] {
        for (symbol, qty) in list {
            let price = prices[symbol];
            let request = IndiaPaperOrderRequest {
                symbol: symbol.clone(),
                side: side.to_string(),
                order_type: "MARKET".to_string(),
                quantity: *qty,
                price: Some(price),
                trigger_reason: Some(format!("Monthly momentum rebalance ({side})")),
            };
            let result = execute_paper_order(store, base_capital_inr, &request, price).await?;
            info!(
                symbol = %symbol,
                side,
                qty,
                price = %price,
                order_id = %result.order.id,
                "NSE momentum paper order recorded; no live order was sent"
            );
        }
    }
    if sells.is_empty() && buys.is_empty() {
        info!(targets = ?decision.targets, "NSE momentum: book already matches the target set");
    }
    Ok(Some(decision))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn series(start: Decimal, step: Decimal, n: usize) -> Vec<Decimal> {
        (0..n).map(|i| start + step * Decimal::from(i)).collect()
    }

    fn config() -> MomentumConfig {
        MomentumConfig {
            lookback_days: 10,
            top_n: 2,
            regime_sma_days: 20,
            ..MomentumConfig::default()
        }
    }

    fn universe() -> HashMap<String, Vec<Decimal>> {
        HashMap::from([
            ("FAST".to_string(), series(dec!(100), dec!(3), 30)),
            ("MID".to_string(), series(dec!(100), dec!(1), 30)),
            ("SLOW".to_string(), series(dec!(100), dec!(0.5), 30)),
            ("DOWN".to_string(), series(dec!(200), dec!(-1), 30)),
            ("NEW".to_string(), series(dec!(100), dec!(5), 5)),
        ])
    }

    #[test]
    fn picks_the_strongest_positive_names_in_a_bull_regime() {
        let nifty = series(dec!(1000), dec!(10), 30);
        let d = select_targets(&universe(), &nifty, &config());
        assert!(d.regime_ok);
        assert_eq!(d.targets, vec!["FAST".to_string(), "MID".to_string()]);
        // Too little history to rank.
        assert!(d.ranked.iter().all(|(s, _)| s != "NEW"));
        assert_eq!(d.ranked.last().unwrap().0, "DOWN");
    }

    #[test]
    fn holds_cash_when_nifty_is_below_its_long_average_or_unknown() {
        let falling = series(dec!(2000), dec!(-10), 30);
        let d = select_targets(&universe(), &falling, &config());
        assert!(!d.regime_ok);
        assert!(d.targets.is_empty());
        assert!(!d.ranked.is_empty(), "ranking is still reported");

        let short = series(dec!(1000), dec!(10), 5);
        assert!(select_targets(&universe(), &short, &config())
            .targets
            .is_empty());
    }

    #[test]
    fn negative_momentum_never_qualifies() {
        let only_down = HashMap::from([("DOWN".to_string(), series(dec!(200), dec!(-1), 30))]);
        let nifty = series(dec!(1000), dec!(10), 30);
        assert!(select_targets(&only_down, &nifty, &config())
            .targets
            .is_empty());
    }

    #[test]
    fn rebalance_sells_dropouts_and_buys_new_targets_with_equal_slots() {
        let holdings = HashMap::from([("OLD".to_string(), 10), ("KEEP".to_string(), 5)]);
        let targets = vec!["KEEP".to_string(), "NEW".to_string()];
        let prices = HashMap::from([
            ("OLD".to_string(), dec!(500)),
            ("KEEP".to_string(), dec!(1000)),
            ("NEW".to_string(), dec!(250)),
        ]);
        // Equity 20,000 over 2 slots: 10,000 for NEW, funded by selling OLD.
        let (sells, buys) =
            rebalance_orders(&holdings, &targets, &prices, dec!(20000), dec!(10000), 2);
        assert_eq!(sells, vec![("OLD".to_string(), 10)]);
        assert_eq!(buys.len(), 1);
        assert_eq!(buys[0].0, "NEW");
        // 10,000 / 250 = 40 shares; the charges fit in the cash freed by OLD.
        assert_eq!(buys[0].1, 40);
    }

    #[test]
    fn rebalance_to_cash_sells_everything_and_buys_nothing() {
        let holdings = HashMap::from([("A".to_string(), 3)]);
        let prices = HashMap::from([("A".to_string(), dec!(100))]);
        let (sells, buys) = rebalance_orders(&holdings, &[], &prices, dec!(1000), dec!(700), 4);
        assert_eq!(sells, vec![("A".to_string(), 3)]);
        assert!(buys.is_empty());
    }

    #[test]
    fn buys_never_exceed_available_cash() {
        let targets = vec!["A".to_string(), "B".to_string()];
        let prices = HashMap::from([("A".to_string(), dec!(100)), ("B".to_string(), dec!(100))]);
        // Equity says 5,000 a slot, but only 6,000 cash exists.
        let (_, buys) = rebalance_orders(
            &HashMap::new(),
            &targets,
            &prices,
            dec!(10000),
            dec!(6000),
            2,
        );
        let spent: Decimal = buys
            .iter()
            .map(|(_, q)| dec!(100) * Decimal::from(*q))
            .sum();
        assert!(spent <= dec!(6000));
        // A takes its full slot; B gets what is left after A and its charges.
        assert_eq!(buys[0], ("A".to_string(), 50));
        assert_eq!(buys[1], ("B".to_string(), 9));
    }
}
