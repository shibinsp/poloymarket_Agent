//! Fill tracking and reconciliation.
//!
//! Records executed trades in the database and tracks open positions
//! for P&L monitoring.

use anyhow::Result;
use rust_decimal::Decimal;
use tracing::{info, warn};

use crate::db::store::{Store, TradeRecord};
use crate::execution::order::{ExecutionResult, OrderStatus, PreparedOrder};

/// Record a successful trade execution in the database.
pub async fn record_trade(
    store: &Store,
    order: &PreparedOrder,
    execution: &ExecutionResult,
    cycle: u64,
) -> Result<Option<i64>> {
    match &execution.status {
        OrderStatus::Filled => {
            let trade = TradeRecord {
                id: None,
                cycle: cycle as i64,
                market_id: order.market_id.clone(),
                market_question: Some(order.market_question.clone()),
                direction: order.side.to_string(),
                entry_price: execution.price.to_string(),
                size: execution.size.to_string(),
                edge_at_entry: order.edge.to_string(),
                claude_fair_value: order.fair_value.to_string(),
                confidence: order.confidence.to_string(),
                kelly_raw: order.kelly_raw.to_string(),
                kelly_adjusted: order.kelly_adjusted.to_string(),
                status: "OPEN".to_string(),
                pnl: None,
                created_at: None,
                resolved_at: None,
            };

            let trade_id = store.insert_trade(&trade).await?;

            info!(
                trade_id,
                order_id = %execution.order_id,
                market = %order.market_id,
                side = %order.side,
                price = %execution.price,
                size = %execution.size,
                edge = %order.edge,
                "Trade recorded"
            );

            Ok(Some(trade_id))
        }
        OrderStatus::Rejected(reason) => {
            warn!(
                market = %order.market_id,
                reason = %reason,
                "Trade rejected — not recorded"
            );
            Ok(None)
        }
    }
}

/// Count currently open trades.
pub async fn open_trade_count(store: &Store) -> Result<usize> {
    let trades = store.get_open_trades().await?;
    Ok(trades.len())
}

/// Calculate total unrealized exposure from open trades.
pub async fn unrealized_exposure(store: &Store) -> Result<Decimal> {
    let trades = store.get_open_trades().await?;
    let mut total = Decimal::ZERO;
    for trade in &trades {
        if let (Ok(price), Ok(size)) = (
            trade.entry_price.parse::<Decimal>(),
            trade.size.parse::<Decimal>(),
        ) {
            total += price * size;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::order::{ExecutionResult, OrderStatus, PreparedOrder};
    use crate::market::models::Side;
    use rust_decimal_macros::dec;

    fn test_order() -> PreparedOrder {
        PreparedOrder {
            token_id: "tok1".to_string(),
            side: Side::Yes,
            price: dec!(0.62),
            size: dec!(10),
            market_id: "m1".to_string(),
            market_question: "Will BTC hit 100k?".to_string(),
            edge: dec!(0.15),
            fair_value: dec!(0.75),
            confidence: dec!(0.85),
            kelly_raw: dec!(0.27),
            kelly_adjusted: dec!(0.12),
        }
    }

    #[tokio::test]
    async fn test_record_filled_trade() {
        let store = Store::new(":memory:").await.unwrap();
        let order = test_order();
        let execution = ExecutionResult {
            order_id: "order-123".to_string(),
            token_id: "tok1".to_string(),
            side: Side::Yes,
            price: dec!(0.62),
            size: dec!(10),
            status: OrderStatus::Filled,
        };

        let trade_id = record_trade(&store, &order, &execution, 1).await.unwrap();
        assert!(trade_id.is_some());

        let open = store.get_open_trades().await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].market_id, "m1");
        assert_eq!(open[0].direction, "YES");
    }

    #[tokio::test]
    async fn test_record_rejected_trade() {
        let store = Store::new(":memory:").await.unwrap();
        let order = test_order();
        let execution = ExecutionResult {
            order_id: String::new(),
            token_id: "tok1".to_string(),
            side: Side::Yes,
            price: dec!(0.62),
            size: dec!(10),
            status: OrderStatus::Rejected("Insufficient balance".to_string()),
        };

        let trade_id = record_trade(&store, &order, &execution, 1).await.unwrap();
        assert!(trade_id.is_none());

        let open = store.get_open_trades().await.unwrap();
        assert_eq!(open.len(), 0);
    }

    #[tokio::test]
    async fn test_unrealized_exposure() {
        let store = Store::new(":memory:").await.unwrap();

        // Insert two trades manually
        let trade1 = TradeRecord {
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
        let trade2 = TradeRecord {
            id: None,
            cycle: 1,
            market_id: "m2".to_string(),
            market_question: Some("Test 2?".to_string()),
            direction: "NO".to_string(),
            entry_price: "0.40".to_string(),
            size: "20".to_string(),
            edge_at_entry: "0.12".to_string(),
            claude_fair_value: "0.30".to_string(),
            confidence: "0.80".to_string(),
            kelly_raw: "0.15".to_string(),
            kelly_adjusted: "0.08".to_string(),
            status: "OPEN".to_string(),
            pnl: None,
            created_at: None,
            resolved_at: None,
        };

        store.insert_trade(&trade1).await.unwrap();
        store.insert_trade(&trade2).await.unwrap();

        let exposure = unrealized_exposure(&store).await.unwrap();
        // 0.60 * 10 + 0.40 * 20 = 6 + 8 = 14
        assert_eq!(exposure, dec!(14));
    }
}
