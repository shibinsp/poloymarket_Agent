//! Continuous, paper-only NSE pullback strategy driven by fresh Upstox ticks.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::agent::kill_switch::KillSwitch;
use crate::db::store::Store;
use crate::india::instruments::nse_watchlist;
use crate::india::market_feed::{LiveTick, UpstoxLiveMarketFeed};
use crate::india::session::detect_nse_session;
use crate::india::simulator::{
    compute_portfolio_accounting, evaluate_strategy_tick, execute_paper_order,
    IndiaPaperOrderRequest,
};
use crate::strategy::pullback::{
    calculate_position_size, evaluate_pullback_setup, FiveMinCandle, PullbackStrategyConfig,
    SignalAction,
};

#[derive(Clone)]
struct BuildingCandle {
    candle: FiveMinCandle,
    bucket: i64,
    volume_at_open: i64,
    last_cumulative_volume: i64,
}

impl BuildingCandle {
    fn start(symbol: &str, tick: &LiveTick, bucket: i64, volume_at_open: i64) -> Self {
        Self {
            candle: FiveMinCandle {
                timestamp: DateTime::from_timestamp(bucket * 300, 0).unwrap_or(tick.timestamp),
                symbol: symbol.to_string(),
                open: tick.last_price,
                high: tick.last_price,
                low: tick.last_price,
                close: tick.last_price,
                volume: Decimal::ZERO,
            },
            bucket,
            volume_at_open,
            last_cumulative_volume: tick.volume,
        }
    }

    fn update(&mut self, tick: &LiveTick) {
        self.candle.high = self.candle.high.max(tick.last_price);
        self.candle.low = self.candle.low.min(tick.last_price);
        self.candle.close = tick.last_price;
        self.last_cumulative_volume = tick.volume;
        self.candle.volume = Decimal::from(tick.volume.saturating_sub(self.volume_at_open).max(0));
    }
}

/// Build five-minute candles from Upstox ticks, then run the configured
/// pullback/rebound strategy. Exit protection continues to use the simulator's
/// existing take-profit and stop-loss rules. All orders remain local paper fills.
pub fn spawn_upstox_strategy_worker(
    feed: Arc<UpstoxLiveMarketFeed>,
    store: Store,
    kill_switch: Arc<KillSwitch>,
    base_capital_inr: Decimal,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let config = PullbackStrategyConfig::default();
        // Use available paper cash for notional sizing while the strategy's
        // risk-per-trade limit controls the position risk.
        let mut sizing_config = config.clone();
        sizing_config.max_position_pct_of_cash = Decimal::ONE;
        sizing_config.max_risk_per_trade_pct = rust_decimal_macros::dec!(0.02);

        let mut candles: HashMap<String, Vec<FiveMinCandle>> = HashMap::new();
        let mut builders: HashMap<String, BuildingCandle> = HashMap::new();
        let mut handled_ticks: HashMap<String, (i64, Decimal)> = HashMap::new();
        let mut last_evaluated_candle: HashMap<String, DateTime<Utc>> = HashMap::new();

        // Warm EMA history so the strategy can make decisions before 50 live
        // bars have elapsed. The strategy still requires today's own VWAP and
        // pullback bars before it can enter.
        let mut warmup =
            futures_util::stream::iter(nse_watchlist().into_iter().map(|instrument| {
                let feed = feed.clone();
                async move {
                    let symbol = instrument.symbol;
                    (
                        symbol.clone(),
                        feed.fetch_intraday_5m_candles(&symbol).await,
                    )
                }
            }))
            .buffer_unordered(4);
        use futures_util::StreamExt;
        while let Some((symbol, result)) = warmup.next().await {
            match result {
                Ok(history) => {
                    info!(%symbol, candle_count = history.len(), "Loaded Upstox five-minute strategy history");
                    candles.insert(symbol, history);
                }
                Err(error) => {
                    warn!(%symbol, %error, "Could not warm up strategy candle history; collecting live candles")
                }
            }
        }

        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let ticks = feed.prices.read().await.clone();

            for (symbol, tick) in ticks {
                if !matches!(tick.source.as_str(), "UPSTOX_V3_LIVE" | "UPSTOX_REST_LIVE") {
                    continue;
                }

                let tick_key = (tick.timestamp.timestamp_millis(), tick.last_price);
                if handled_ticks.get(&symbol) == Some(&tick_key) {
                    continue;
                }
                handled_ticks.insert(symbol.clone(), tick_key);

                let bucket = tick.timestamp.timestamp().div_euclid(300);
                let mut completed_candle = None;
                if let Some(builder) = builders.get_mut(&symbol) {
                    if bucket > builder.bucket {
                        completed_candle = Some(builder.candle.clone());
                        let prior_volume = builder.last_cumulative_volume;
                        *builder = BuildingCandle::start(&symbol, &tick, bucket, prior_volume);
                    } else if bucket == builder.bucket {
                        builder.update(&tick);
                    }
                } else {
                    builders.insert(
                        symbol.clone(),
                        BuildingCandle::start(&symbol, &tick, bucket, tick.volume),
                    );
                }

                if let Some(candle) = completed_candle.as_ref() {
                    let history = candles.entry(symbol.clone()).or_default();
                    if history
                        .last()
                        .map(|prev| prev.timestamp < candle.timestamp)
                        .unwrap_or(true)
                    {
                        history.push(candle.clone());
                    }
                    let max_history = 500;
                    if history.len() > max_history {
                        history.drain(..history.len() - max_history);
                    }
                }

                if kill_switch.is_tripped()
                    || feed.is_stale().await
                    || !detect_nse_session(Utc::now()).allows_order_entry
                {
                    continue;
                }

                let orders = match store.get_india_sandbox_orders(500).await {
                    Ok(orders) => orders,
                    Err(error) => {
                        warn!(%symbol, %error, "Could not load paper portfolio for market tick");
                        continue;
                    }
                };
                let prices: HashMap<String, Decimal> = feed
                    .prices
                    .read()
                    .await
                    .iter()
                    .filter(|(_, quote)| {
                        matches!(quote.source.as_str(), "UPSTOX_V3_LIVE" | "UPSTOX_REST_LIVE")
                    })
                    .map(|(name, quote)| (name.clone(), quote.last_price))
                    .collect();
                let portfolio = compute_portfolio_accounting(base_capital_inr, &orders, &prices);
                let holding = portfolio
                    .holdings
                    .get(&symbol)
                    .copied()
                    .unwrap_or(Decimal::ZERO)
                    .to_string()
                    .parse::<i64>()
                    .unwrap_or(0);

                // Existing open positions keep their protective exit checks on
                // each fresh quote. Entries are evaluated strictly on completed bars.
                let mut evaluated_entry = false;
                let (action, quantity, reason) = if holding > 0 {
                    let average_entry = portfolio
                        .open_positions
                        .iter()
                        .find(|position| position.symbol == symbol)
                        .map(|position| position.average_buy_price);
                    let decision = evaluate_strategy_tick(
                        &symbol,
                        tick.last_price,
                        portfolio.available_cash_inr,
                        holding,
                        average_entry,
                    );
                    (decision.action, decision.quantity, decision.reason)
                } else {
                    let history = candles.entry(symbol.clone()).or_default();
                    let latest_completed = history.last().cloned();
                    let should_eval = match (
                        latest_completed.as_ref(),
                        last_evaluated_candle.get(&symbol),
                    ) {
                        (Some(latest), Some(last_ts)) => latest.timestamp > *last_ts,
                        (Some(_), None) => true,
                        (None, _) => false,
                    };

                    if should_eval {
                        evaluated_entry = true;
                        if let Some(latest) = latest_completed.as_ref() {
                            last_evaluated_candle.insert(symbol.clone(), latest.timestamp);
                        }
                        let (signal, _) = evaluate_pullback_setup(history, &config);
                        match signal {
                            SignalAction::Buy {
                                planned_risk_per_share,
                                reason,
                                ..
                            } => {
                                let mut raw_qty = calculate_position_size(
                                    portfolio.available_cash_inr,
                                    tick.last_price,
                                    planned_risk_per_share,
                                    &sizing_config,
                                );
                                let cost_config = crate::india::costs::IndianCostConfig::default();
                                while raw_qty >= 1 {
                                    let trade_val = tick.last_price * Decimal::from(raw_qty);
                                    let entry_fee = cost_config
                                        .calculate(trade_val, crate::venue::types::Side::Buy)
                                        .total;
                                    if trade_val + entry_fee <= portfolio.available_cash_inr {
                                        break;
                                    }
                                    raw_qty -= 1;
                                }
                                (
                                    if raw_qty > 0 {
                                        "BUY".to_string()
                                    } else {
                                        "WAIT".to_string()
                                    },
                                    raw_qty,
                                    if raw_qty > 0 {
                                        reason
                                    } else {
                                        format!(
                                            "Pullback setup detected but position cost plus estimated charges exceeds available cash ₹{:.2}",
                                            portfolio.available_cash_inr
                                        )
                                    },
                                )
                            }
                            _ => (
                                "WAIT".to_string(),
                                0,
                                "No completed-bar pullback setup".to_string(),
                            ),
                        }
                    } else {
                        (
                            "WAIT".to_string(),
                            0,
                            "Waiting for next completed five-minute candle".to_string(),
                        )
                    }
                };

                if action != "WAIT" || evaluated_entry {
                    info!(
                        symbol,
                        action = %action,
                        price = %tick.last_price,
                        quantity,
                        reason = %reason,
                        "Upstox completed bar evaluated by NSE pullback strategy"
                    );
                }

                if !matches!(action.as_str(), "BUY" | "SELL")
                    || quantity <= 0
                    || kill_switch.is_tripped()
                    || feed.is_stale().await
                {
                    continue;
                }

                let request = IndiaPaperOrderRequest {
                    symbol: symbol.clone(),
                    side: action.clone(),
                    order_type: "MARKET".to_string(),
                    quantity,
                    price: Some(tick.last_price),
                    trigger_reason: Some(reason),
                };
                match execute_paper_order(&store, base_capital_inr, &request, tick.last_price).await
                {
                    Ok(result) => info!(
                        symbol,
                        action = %action,
                        quantity,
                        price = %tick.last_price,
                        order_id = %result.order.id,
                        "NSE strategy recorded a local paper order; no live order was sent"
                    ),
                    Err(error) => {
                        warn!(%symbol, action = %action, %error, "NSE strategy paper order was rejected")
                    }
                }
            }
        }
    })
}
