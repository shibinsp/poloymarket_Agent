//! P&L tracking and performance metrics.
//!
//! Computes win rate, total P&L, Sharpe ratio, ROI, and other
//! trading statistics from SQLite trade history.

use anyhow::Result;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::str::FromStr;
use tracing::info;

use crate::db::store::Store;

/// Aggregated performance metrics snapshot.
#[derive(Debug, Clone)]
pub struct PerformanceMetrics {
    pub total_trades: u64,
    pub open_trades: u64,
    pub resolved_trades: u64,
    pub wins: u64,
    pub losses: u64,
    pub win_rate: Decimal,
    pub total_pnl: Decimal,
    pub realized_pnl: Decimal,
    pub unrealized_exposure: Decimal,
    pub avg_edge_at_entry: Decimal,
    pub avg_position_size: Decimal,
    pub total_api_cost: Decimal,
    pub net_profit: Decimal,
    pub roi_pct: Decimal,
    pub sharpe_ratio: Option<Decimal>,
    pub cycles_completed: u64,
    pub avg_cycle_duration_ms: Option<f64>,
}

impl PerformanceMetrics {
    /// Format metrics for logging or alerts.
    pub fn summary(&self) -> String {
        format!(
            "Trades: {} ({} open, {} resolved: {}W/{}L, {:.1}% win rate)\n\
             P&L: ${} realized, ${} net (after ${} API costs)\n\
             ROI: {:.1}% | Sharpe: {} | Avg edge: {:.1}%\n\
             Cycles: {} | Avg duration: {:.0}ms",
            self.total_trades,
            self.open_trades,
            self.resolved_trades,
            self.wins,
            self.losses,
            self.win_rate * dec!(100),
            self.realized_pnl,
            self.net_profit,
            self.total_api_cost,
            self.roi_pct * dec!(100),
            self.sharpe_ratio
                .map(|s| format!("{:.2}", s))
                .unwrap_or_else(|| "N/A".to_string()),
            self.avg_edge_at_entry * dec!(100),
            self.cycles_completed,
            self.avg_cycle_duration_ms.unwrap_or(0.0),
        )
    }
}

/// Compute all performance metrics from the database.
pub async fn compute_metrics(
    store: &Store,
    initial_bankroll: Decimal,
) -> Result<PerformanceMetrics> {
    let all_trades = store.get_all_trades().await?;
    let resolved = store.get_resolved_trades().await?;
    let open = store.get_open_trades().await?;
    let total_api_cost = store.get_total_api_cost().await?;
    let cycle_count = store.get_cycle_count().await?;
    let avg_duration = store.get_avg_cycle_duration_ms().await?;

    let total_trades = all_trades.len() as u64;
    let open_trades = open.len() as u64;
    let resolved_trades = resolved.len() as u64;

    let mut wins = 0u64;
    let mut losses = 0u64;
    let mut realized_pnl = Decimal::ZERO;
    let mut pnl_values: Vec<Decimal> = Vec::new();

    for trade in &resolved {
        let pnl = trade
            .pnl
            .as_deref()
            .and_then(|s| Decimal::from_str(s).ok())
            .unwrap_or(Decimal::ZERO);

        if trade.status == "RESOLVED_WIN" {
            wins += 1;
        } else {
            losses += 1;
        }
        realized_pnl += pnl;
        pnl_values.push(pnl);
    }

    let win_rate = if resolved_trades > 0 {
        Decimal::from(wins) / Decimal::from(resolved_trades)
    } else {
        Decimal::ZERO
    };

    // Unrealized exposure from open trades
    let mut unrealized_exposure = Decimal::ZERO;
    for trade in &open {
        if let (Ok(price), Ok(size)) = (
            Decimal::from_str(&trade.entry_price),
            Decimal::from_str(&trade.size),
        ) {
            unrealized_exposure += price * size;
        }
    }

    // Average edge at entry across all trades
    let avg_edge = if total_trades > 0 {
        let total_edge: Decimal = all_trades
            .iter()
            .filter_map(|t| Decimal::from_str(&t.edge_at_entry).ok())
            .sum();
        total_edge / Decimal::from(total_trades)
    } else {
        Decimal::ZERO
    };

    // Average position size
    let avg_position_size = if total_trades > 0 {
        let total_size: Decimal = all_trades
            .iter()
            .filter_map(|t| Decimal::from_str(&t.size).ok())
            .sum();
        total_size / Decimal::from(total_trades)
    } else {
        Decimal::ZERO
    };

    let net_profit = realized_pnl - total_api_cost;

    // ROI as percentage of initial bankroll
    let roi_pct = if initial_bankroll > Decimal::ZERO {
        net_profit / initial_bankroll
    } else {
        Decimal::ZERO
    };

    // Sharpe ratio: mean(returns) / std(returns)
    let sharpe_ratio = compute_sharpe(&pnl_values);

    Ok(PerformanceMetrics {
        total_trades,
        open_trades,
        resolved_trades,
        wins,
        losses,
        win_rate,
        total_pnl: realized_pnl + unrealized_exposure,
        realized_pnl,
        unrealized_exposure,
        avg_edge_at_entry: avg_edge,
        avg_position_size,
        total_api_cost,
        net_profit,
        roi_pct,
        sharpe_ratio,
        cycles_completed: cycle_count as u64,
        avg_cycle_duration_ms: avg_duration,
    })
}

/// Compute annualized Sharpe ratio from per-trade P&L values.
/// Assumes ~144 trades/day as the scaling factor (one per 10-min cycle).
fn compute_sharpe(pnl_values: &[Decimal]) -> Option<Decimal> {
    if pnl_values.len() < 2 {
        return None;
    }

    let n = Decimal::from(pnl_values.len() as u64);
    let sum: Decimal = pnl_values.iter().sum();
    let mean = sum / n;

    let variance_sum: Decimal = pnl_values
        .iter()
        .map(|p| {
            let diff = *p - mean;
            diff * diff
        })
        .sum();

    let variance = variance_sum / (n - Decimal::ONE);

    if variance <= Decimal::ZERO {
        return None;
    }

    // Approximate sqrt using Newton's method for Decimal
    let std_dev = decimal_sqrt(variance)?;

    if std_dev <= Decimal::ZERO {
        return None;
    }

    // Per-trade Sharpe (not annualized — more meaningful for this context)
    Some(mean / std_dev)
}

/// Approximate square root for Decimal using Newton's method.
fn decimal_sqrt(value: Decimal) -> Option<Decimal> {
    if value < Decimal::ZERO {
        return None;
    }
    if value == Decimal::ZERO {
        return Some(Decimal::ZERO);
    }

    let mut guess = value / dec!(2);
    for _ in 0..20 {
        let next = (guess + value / guess) / dec!(2);
        if (next - guess).abs() < dec!(0.0000001) {
            return Some(next);
        }
        guess = next;
    }
    Some(guess)
}

/// Log a metrics summary.
pub fn log_metrics(metrics: &PerformanceMetrics) {
    info!(
        total_trades = metrics.total_trades,
        open_trades = metrics.open_trades,
        wins = metrics.wins,
        losses = metrics.losses,
        win_rate = %metrics.win_rate,
        realized_pnl = %metrics.realized_pnl,
        net_profit = %metrics.net_profit,
        roi_pct = %metrics.roi_pct,
        total_api_cost = %metrics.total_api_cost,
        cycles = metrics.cycles_completed,
        "Performance metrics"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::store::{ApiCostRecord, TradeRecord};

    #[test]
    fn test_decimal_sqrt() {
        let root = decimal_sqrt(dec!(4)).unwrap();
        assert!((root - dec!(2)).abs() < dec!(0.001));

        let root = decimal_sqrt(dec!(9)).unwrap();
        assert!((root - dec!(3)).abs() < dec!(0.001));

        let root = decimal_sqrt(dec!(2)).unwrap();
        assert!((root - dec!(1.4142)).abs() < dec!(0.001));

        assert_eq!(decimal_sqrt(Decimal::ZERO), Some(Decimal::ZERO));
        assert_eq!(decimal_sqrt(dec!(-1)), None);
    }

    #[test]
    fn test_sharpe_ratio() {
        // Equal returns → zero variance → None
        let values = vec![dec!(1), dec!(1), dec!(1)];
        assert!(compute_sharpe(&values).is_none());

        // Not enough data
        assert!(compute_sharpe(&[dec!(1)]).is_none());
        assert!(compute_sharpe(&[]).is_none());

        // Positive mean, some variance
        let values = vec![dec!(2), dec!(3), dec!(4), dec!(5)];
        let sharpe = compute_sharpe(&values).unwrap();
        assert!(sharpe > Decimal::ZERO);
    }

    #[tokio::test]
    async fn test_compute_metrics_empty() {
        let store = Store::new(":memory:").await.unwrap();
        let metrics = compute_metrics(&store, dec!(100)).await.unwrap();

        assert_eq!(metrics.total_trades, 0);
        assert_eq!(metrics.wins, 0);
        assert_eq!(metrics.losses, 0);
        assert_eq!(metrics.win_rate, Decimal::ZERO);
        assert_eq!(metrics.realized_pnl, Decimal::ZERO);
        assert_eq!(metrics.net_profit, Decimal::ZERO);
    }

    #[tokio::test]
    async fn test_compute_metrics_with_trades() {
        use chrono::Utc;

        let store = Store::new(":memory:").await.unwrap();

        // Insert trades as OPEN first (insert_trade doesn't persist pnl)
        let trade = TradeRecord {
            id: None,
            cycle: 1,
            market_id: "m1".to_string(),
            market_question: Some("Test?".to_string()),
            direction: "YES".to_string(),
            entry_price: "0.60".to_string(),
            size: "10".to_string(),
            edge_at_entry: "0.10".to_string(),
            claude_fair_value: "0.70".to_string(),
            confidence: "0.85".to_string(),
            kelly_raw: "0.20".to_string(),
            kelly_adjusted: "0.10".to_string(),
            status: "OPEN".to_string(),
            pnl: None,
            created_at: None,
            resolved_at: None,
        };
        let id1 = store.insert_trade(&trade).await.unwrap();

        let trade2 = TradeRecord {
            market_id: "m2".to_string(),
            ..trade.clone()
        };
        let id2 = store.insert_trade(&trade2).await.unwrap();

        // Resolve trades via update_trade_status (which persists pnl)
        store
            .update_trade_status(id1, "RESOLVED_WIN", Some(dec!(4)), Some(Utc::now()))
            .await
            .unwrap();
        store
            .update_trade_status(id2, "RESOLVED_LOSS", Some(dec!(-6)), Some(Utc::now()))
            .await
            .unwrap();

        // Insert API cost
        let cost = ApiCostRecord {
            id: None,
            provider: "anthropic".to_string(),
            endpoint: Some("/v1/messages".to_string()),
            input_tokens: Some(2000),
            output_tokens: Some(300),
            cost: "0.05".to_string(),
            cycle: Some(1),
            created_at: None,
        };
        store.insert_api_cost(&cost).await.unwrap();

        let metrics = compute_metrics(&store, dec!(100)).await.unwrap();

        assert_eq!(metrics.total_trades, 2);
        assert_eq!(metrics.wins, 1);
        assert_eq!(metrics.losses, 1);
        assert_eq!(metrics.win_rate, dec!(0.5));
        assert_eq!(metrics.realized_pnl, dec!(-2)); // 4 - 6
        assert_eq!(metrics.total_api_cost, dec!(0.05));
        // Net: -2 - 0.05 = -2.05
        assert_eq!(metrics.net_profit, dec!(-2.05));
    }

    #[test]
    fn test_metrics_summary_format() {
        let metrics = PerformanceMetrics {
            total_trades: 10,
            open_trades: 2,
            resolved_trades: 8,
            wins: 5,
            losses: 3,
            win_rate: dec!(0.625),
            total_pnl: dec!(15),
            realized_pnl: dec!(12),
            unrealized_exposure: dec!(3),
            avg_edge_at_entry: dec!(0.10),
            avg_position_size: dec!(5),
            total_api_cost: dec!(0.50),
            net_profit: dec!(11.50),
            roi_pct: dec!(0.115),
            sharpe_ratio: Some(dec!(1.25)),
            cycles_completed: 100,
            avg_cycle_duration_ms: Some(1500.0),
        };

        let summary = metrics.summary();
        assert!(summary.contains("10"));
        assert!(summary.contains("62.5%"));
        assert!(summary.contains("5W/3L"));
    }
}
