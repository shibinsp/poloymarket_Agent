use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};
use std::str::FromStr;

pub struct Store {
    pool: SqlitePool,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct TradeRecord {
    pub id: Option<i64>,
    pub cycle: i64,
    pub market_id: String,
    pub market_question: Option<String>,
    pub direction: String,
    pub entry_price: String,
    pub size: String,
    pub edge_at_entry: String,
    pub claude_fair_value: String,
    pub confidence: String,
    pub kelly_raw: String,
    pub kelly_adjusted: String,
    pub status: String,
    pub pnl: Option<String>,
    pub created_at: Option<String>,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct CycleRecord {
    pub id: Option<i64>,
    pub cycle_number: i64,
    pub markets_scanned: Option<i64>,
    pub opportunities_found: Option<i64>,
    pub trades_placed: Option<i64>,
    pub api_cost: Option<String>,
    pub bankroll: Option<String>,
    pub unrealized_pnl: Option<String>,
    pub agent_state: String,
    pub duration_ms: Option<i64>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ApiCostRecord {
    pub id: Option<i64>,
    pub provider: String,
    pub endpoint: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cost: String,
    pub cycle: Option<i64>,
    pub created_at: Option<String>,
}

impl Store {
    /// Create a Store from an existing pool (for sharing between Agent and Dashboard).
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Get a reference to the underlying connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Create a clone for use in parallel tasks.
    /// Shares the same underlying connection pool.
    pub fn clone_for_parallel(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }

    pub async fn new(database_path: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(&format!("sqlite:{database_path}"))
            .context("Invalid database path")?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .context("Failed to connect to SQLite database")?;

        let store = Self { pool };
        store.migrate().await?;

        Ok(store)
    }

    /// Apply pending migrations from `./migrations`, tracked in `_sqlx_migrations`.
    ///
    /// Databases created by the previous ad-hoc runner have no tracking table;
    /// `001_init.sql` is entirely `IF NOT EXISTS`, so re-applying it on such a
    /// database is a no-op that simply records the version.
    ///
    /// Unlike the old runner, this checksums each applied migration: editing
    /// an already-applied file (rather than adding a new one) makes every
    /// existing database refuse to start. Schema changes always go in a new
    /// `NNN_description.sql` file — see RULES.md's Database Rules.
    async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .context("Failed to run database migrations")?;
        Ok(())
    }

    // --- Trade operations ---

    pub async fn insert_trade(&self, trade: &TradeRecord) -> Result<i64> {
        let result = sqlx::query(
            // venue_id/symbol/side are derived here rather than defaulted in
            // the schema: an empty symbol would collide across markets, which
            // is the cache-collision bug class all over again. The legacy
            // Polymarket path is always a BUY of the named outcome token, and
            // the symbol format matches what the venue adapter produces.
            "INSERT INTO trades (cycle, venue_id, symbol, side, market_id, market_question, direction, entry_price, size, quantity, edge_at_entry, claude_fair_value, confidence, kelly_raw, kelly_adjusted, status)
             VALUES (?, 'polymarket', ?, 'BUY', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(trade.cycle)
        .bind(format!("{}:{}", trade.market_id, trade.direction))
        .bind(&trade.market_id)
        .bind(&trade.market_question)
        .bind(&trade.direction)
        .bind(&trade.entry_price)
        .bind(&trade.size)
        // quantity mirrors size for the legacy path; they diverge once fills
        // are confirmed rather than assumed.
        .bind(&trade.size)
        .bind(&trade.edge_at_entry)
        .bind(&trade.claude_fair_value)
        .bind(&trade.confidence)
        .bind(&trade.kelly_raw)
        .bind(&trade.kelly_adjusted)
        .bind(&trade.status)
        .execute(&self.pool)
        .await
        .context("Failed to insert trade")?;

        Ok(result.last_insert_rowid())
    }

    pub async fn update_trade_status(
        &self,
        id: i64,
        status: &str,
        pnl: Option<Decimal>,
        resolved_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        sqlx::query("UPDATE trades SET status = ?, pnl = ?, resolved_at = ? WHERE id = ?")
            .bind(status)
            .bind(pnl.map(|d| d.to_string()))
            .bind(resolved_at.map(|dt| dt.to_rfc3339()))
            .bind(id)
            .execute(&self.pool)
            .await
            .context("Failed to update trade status")?;
        Ok(())
    }

    pub async fn get_open_trades(&self) -> Result<Vec<TradeRecord>> {
        let trades = sqlx::query_as::<_, TradeRecord>("SELECT * FROM trades WHERE status = 'OPEN'")
            .fetch_all(&self.pool)
            .await
            .context("Failed to fetch open trades")?;
        Ok(trades)
    }

    pub async fn get_trades_by_market(&self, market_id: &str) -> Result<Vec<TradeRecord>> {
        let trades = sqlx::query_as::<_, TradeRecord>("SELECT * FROM trades WHERE market_id = ?")
            .bind(market_id)
            .fetch_all(&self.pool)
            .await
            .context("Failed to fetch trades by market")?;
        Ok(trades)
    }

    // --- Cycle operations ---

    pub async fn insert_cycle(&self, cycle: &CycleRecord) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO cycles (cycle_number, markets_scanned, opportunities_found, trades_placed, api_cost, bankroll, unrealized_pnl, agent_state, duration_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(cycle.cycle_number)
        .bind(cycle.markets_scanned)
        .bind(cycle.opportunities_found)
        .bind(cycle.trades_placed)
        .bind(&cycle.api_cost)
        .bind(&cycle.bankroll)
        .bind(&cycle.unrealized_pnl)
        .bind(&cycle.agent_state)
        .bind(cycle.duration_ms)
        .execute(&self.pool)
        .await
        .context("Failed to insert cycle")?;

        Ok(result.last_insert_rowid())
    }

    pub async fn get_latest_cycle(&self) -> Result<Option<CycleRecord>> {
        let cycle = sqlx::query_as::<_, CycleRecord>(
            "SELECT * FROM cycles ORDER BY cycle_number DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .context("Failed to fetch latest cycle")?;
        Ok(cycle)
    }

    /// Get all resolved trades (wins and losses).
    pub async fn get_resolved_trades(&self) -> Result<Vec<TradeRecord>> {
        let trades = sqlx::query_as::<_, TradeRecord>(
            "SELECT * FROM trades WHERE status IN ('RESOLVED_WIN', 'RESOLVED_LOSS') ORDER BY resolved_at",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch resolved trades")?;
        Ok(trades)
    }

    /// Get all trades regardless of status.
    pub async fn get_all_trades(&self) -> Result<Vec<TradeRecord>> {
        let trades = sqlx::query_as::<_, TradeRecord>("SELECT * FROM trades ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .context("Failed to fetch all trades")?;
        Ok(trades)
    }

    /// Get total number of cycles completed.
    pub async fn get_cycle_count(&self) -> Result<i64> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM cycles")
            .fetch_one(&self.pool)
            .await
            .context("Failed to count cycles")?;
        Ok(row.0)
    }

    /// Get average cycle duration in milliseconds.
    pub async fn get_avg_cycle_duration_ms(&self) -> Result<Option<f64>> {
        let row: (Option<f64>,) =
            sqlx::query_as("SELECT AVG(duration_ms) FROM cycles WHERE duration_ms IS NOT NULL")
                .fetch_one(&self.pool)
                .await
                .context("Failed to get average cycle duration")?;
        Ok(row.0)
    }

    // --- API cost operations ---

    pub async fn insert_api_cost(&self, cost: &ApiCostRecord) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO api_costs (provider, endpoint, input_tokens, output_tokens, cost, cycle)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&cost.provider)
        .bind(&cost.endpoint)
        .bind(cost.input_tokens)
        .bind(cost.output_tokens)
        .bind(&cost.cost)
        .bind(cost.cycle)
        .execute(&self.pool)
        .await
        .context("Failed to insert API cost")?;

        Ok(result.last_insert_rowid())
    }

    pub async fn get_total_api_cost(&self) -> Result<Decimal> {
        let row: (Option<String>,) =
            sqlx::query_as("SELECT CAST(SUM(CAST(cost AS REAL)) AS TEXT) FROM api_costs")
                .fetch_one(&self.pool)
                .await
                .context("Failed to get total API cost")?;

        match row.0 {
            Some(s) => Ok(Decimal::from_str(&s).unwrap_or(Decimal::ZERO)),
            None => Ok(Decimal::ZERO),
        }
    }

    /// Get total API spend for the current UTC day.
    pub async fn get_today_api_cost(&self) -> Result<Decimal> {
        let row: (Option<String>,) = sqlx::query_as(
            "SELECT CAST(SUM(CAST(cost AS REAL)) AS TEXT) FROM api_costs WHERE created_at >= date('now')",
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to get today's API cost")?;

        match row.0 {
            Some(s) => Ok(Decimal::from_str(&s).unwrap_or(Decimal::ZERO)),
            None => Ok(Decimal::ZERO),
        }
    }

    /// Get all cycles ordered by cycle number.
    pub async fn get_all_cycles(&self) -> Result<Vec<CycleRecord>> {
        let cycles = sqlx::query_as::<_, CycleRecord>("SELECT * FROM cycles ORDER BY cycle_number")
            .fetch_all(&self.pool)
            .await
            .context("Failed to fetch all cycles")?;
        Ok(cycles)
    }

    /// Get all API cost records.
    pub async fn get_all_api_costs(&self) -> Result<Vec<ApiCostRecord>> {
        let costs = sqlx::query_as::<_, ApiCostRecord>("SELECT * FROM api_costs ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .context("Failed to fetch all API costs")?;
        Ok(costs)
    }

    /// Get recent trades with a limit.
    pub async fn get_recent_trades(&self, limit: i64) -> Result<Vec<TradeRecord>> {
        let trades =
            sqlx::query_as::<_, TradeRecord>("SELECT * FROM trades ORDER BY id DESC LIMIT ?")
                .bind(limit)
                .fetch_all(&self.pool)
                .await
                .context("Failed to fetch recent trades")?;
        Ok(trades)
    }

    pub async fn get_api_cost_for_cycle(&self, cycle: i64) -> Result<Decimal> {
        let row: (Option<String>,) = sqlx::query_as(
            "SELECT CAST(SUM(CAST(cost AS REAL)) AS TEXT) FROM api_costs WHERE cycle = ?",
        )
        .bind(cycle)
        .fetch_one(&self.pool)
        .await
        .context("Failed to get API cost for cycle")?;

        match row.0 {
            Some(s) => Ok(Decimal::from_str(&s).unwrap_or(Decimal::ZERO)),
            None => Ok(Decimal::ZERO),
        }
    }

    // === Orders (multi-venue) ===

    /// Record a submitted order. Written *before* the venue call returns, so
    /// an order that times out still leaves a row to reconcile against — the
    /// alternative is an order live at the venue that we have no record of.
    pub async fn insert_order(&self, order: &OrderRecord) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO orders (client_order_id, venue_order_id, venue_id, symbol, side, intent, trade_id, limit_price, qty, filled_qty, avg_fill_price, state, reject_reason, cycle, expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&order.client_order_id)
        .bind(&order.venue_order_id)
        .bind(&order.venue_id)
        .bind(&order.symbol)
        .bind(&order.side)
        .bind(&order.intent)
        .bind(order.trade_id)
        .bind(&order.limit_price)
        .bind(&order.qty)
        .bind(&order.filled_qty)
        .bind(&order.avg_fill_price)
        .bind(&order.state)
        .bind(&order.reject_reason)
        .bind(order.cycle)
        .bind(&order.expires_at)
        .execute(&self.pool)
        .await
        .context("Failed to insert order")?;

        Ok(result.last_insert_rowid())
    }

    /// Update an order after querying the venue for its fate.
    pub async fn update_order_state(
        &self,
        client_order_id: &str,
        state: &str,
        venue_order_id: Option<&str>,
        filled_qty: &str,
        avg_fill_price: Option<&str>,
        reject_reason: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE orders SET state = ?, venue_order_id = COALESCE(?, venue_order_id), filled_qty = ?, avg_fill_price = ?, reject_reason = ?, updated_at = ? WHERE client_order_id = ?",
        )
        .bind(state)
        .bind(venue_order_id)
        .bind(filled_qty)
        .bind(avg_fill_price)
        .bind(reject_reason)
        .bind(Utc::now().to_rfc3339())
        .bind(client_order_id)
        .execute(&self.pool)
        .await
        .context("Failed to update order state")?;
        Ok(())
    }

    /// Orders the venue may still act on, plus any whose fate is unknown.
    /// These must be resolved before placing anything new for the same symbol.
    pub async fn get_unresolved_orders(&self) -> Result<Vec<OrderRecord>> {
        sqlx::query_as::<_, OrderRecord>(
            "SELECT * FROM orders WHERE state IN ('PENDING', 'ACCEPTED', 'PARTIALLY_FILLED', 'UNKNOWN') ORDER BY submitted_at",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch unresolved orders")
    }

    /// Open continuous-asset positions, which unlike prediction markets never
    /// settle themselves and must be explicitly closed.
    pub async fn get_open_venue_trades(&self) -> Result<Vec<VenueOpenTrade>> {
        let rows = sqlx::query_as::<_, VenueOpenTradeRow>(
            "SELECT id, venue_id, symbol, asset_class, entry_price, avg_fill_price, quantity, stop_price, target_price, horizon_hours, created_at
             FROM trades
             WHERE status IN ('OPEN', 'PARTIAL') AND asset_class != 'prediction_binary'",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch open venue trades")?;

        rows.into_iter().map(VenueOpenTrade::try_from).collect()
    }

    /// Record the latest mark so the survival check and dashboard see market
    /// value rather than entry cost.
    pub async fn mark_trade(
        &self,
        id: i64,
        mark_price: &str,
        unrealized_pnl: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE trades SET mark_price = ?, unrealized_pnl = ?, marked_at = ? WHERE id = ?",
        )
        .bind(mark_price)
        .bind(unrealized_pnl)
        .bind(at.to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to mark trade")?;
        Ok(())
    }

    /// Close a position once its exit has actually filled.
    pub async fn close_trade(
        &self,
        id: i64,
        exit_price: &str,
        realized_pnl: &str,
        reason: &str,
        exit_order_id: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE trades SET status = 'CLOSED', mark_price = ?, realized_pnl = ?, pnl = ?, close_reason = ?, exit_order_id = ?, closed_at = ? WHERE id = ?",
        )
        .bind(exit_price)
        .bind(realized_pnl)
        .bind(realized_pnl)
        .bind(reason)
        .bind(exit_order_id)
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to close trade")?;
        Ok(())
    }

    /// Record a trade opened on a venue, once something has actually filled.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_venue_trade(&self, trade: &VenueTradeRecord) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO trades (cycle, venue_id, symbol, asset_class, market_id, market_question, direction, side, entry_price, size, quantity, avg_fill_price, edge_at_entry, claude_fair_value, confidence, kelly_raw, kelly_adjusted, status, stop_price, target_price, horizon_hours, client_order_id, venue_order_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(trade.cycle)
        .bind(&trade.venue_id)
        .bind(&trade.symbol)
        .bind(&trade.asset_class)
        .bind(&trade.symbol)
        .bind(&trade.display_name)
        // `direction` is retained for legacy readers; `side` is authoritative.
        .bind(&trade.side)
        .bind(&trade.side)
        .bind(&trade.entry_price)
        .bind(&trade.quantity)
        .bind(&trade.quantity)
        .bind(&trade.avg_fill_price)
        .bind(&trade.edge_at_entry)
        .bind(&trade.fair_value)
        .bind(&trade.confidence)
        .bind(&trade.risk_pct)
        .bind(&trade.stop_pct)
        .bind(&trade.status)
        .bind(&trade.stop_price)
        .bind(&trade.target_price)
        .bind(trade.horizon_hours)
        .bind(&trade.client_order_id)
        .bind(&trade.venue_order_id)
        .execute(&self.pool)
        .await
        .context("Failed to insert venue trade")?;

        Ok(result.last_insert_rowid())
    }
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct OrderRecord {
    pub id: Option<i64>,
    pub client_order_id: String,
    pub venue_order_id: Option<String>,
    pub venue_id: String,
    pub symbol: String,
    pub side: String,
    pub intent: String,
    pub trade_id: Option<i64>,
    pub limit_price: Option<String>,
    pub qty: String,
    pub filled_qty: String,
    pub avg_fill_price: Option<String>,
    pub state: String,
    pub reject_reason: Option<String>,
    pub cycle: Option<i64>,
    pub submitted_at: Option<String>,
    pub updated_at: Option<String>,
    pub expires_at: Option<String>,
}

/// Raw row for an open venue position; decimals arrive as TEXT.
#[derive(Debug, Clone, FromRow)]
struct VenueOpenTradeRow {
    id: i64,
    venue_id: String,
    symbol: String,
    asset_class: String,
    entry_price: String,
    avg_fill_price: Option<String>,
    quantity: Option<String>,
    stop_price: Option<String>,
    target_price: Option<String>,
    horizon_hours: Option<i64>,
    created_at: Option<String>,
}

/// An open continuous-asset position, with decimals parsed.
#[derive(Debug, Clone)]
pub struct VenueOpenTrade {
    pub id: i64,
    pub venue_id: String,
    pub symbol: String,
    pub asset_class: String,
    pub entry_price: Decimal,
    pub avg_fill_price: Option<Decimal>,
    pub quantity: Decimal,
    pub stop_price: Option<Decimal>,
    pub target_price: Option<Decimal>,
    pub horizon_hours: Option<i64>,
    pub opened_at: Option<DateTime<Utc>>,
}

impl TryFrom<VenueOpenTradeRow> for VenueOpenTrade {
    type Error = anyhow::Error;

    /// Parsing failures are errors, not silent zeroes: a position whose size
    /// or entry can't be read must not be marked or exited on a guess.
    fn try_from(row: VenueOpenTradeRow) -> Result<Self> {
        let parse = |value: &str, field: &str| -> Result<Decimal> {
            Decimal::from_str(value)
                .with_context(|| format!("trade {} has an unparseable {field}: {value}", row.id))
        };
        let parse_opt = |value: &Option<String>, field: &str| -> Result<Option<Decimal>> {
            value.as_deref().map(|v| parse(v, field)).transpose()
        };

        Ok(Self {
            id: row.id,
            entry_price: parse(&row.entry_price, "entry_price")?,
            avg_fill_price: parse_opt(&row.avg_fill_price, "avg_fill_price")?,
            quantity: parse_opt(&row.quantity, "quantity")?
                .context("trade is missing a quantity")?,
            stop_price: parse_opt(&row.stop_price, "stop_price")?,
            target_price: parse_opt(&row.target_price, "target_price")?,
            horizon_hours: row.horizon_hours,
            opened_at: row.created_at.as_deref().and_then(|s| {
                DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
                    .or_else(|| {
                        // SQLite's datetime('now') default has no offset.
                        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                            .ok()
                            .map(|n| n.and_utc())
                    })
            }),
            venue_id: row.venue_id,
            symbol: row.symbol,
            asset_class: row.asset_class,
        })
    }
}

/// A filled position opened through a venue adapter.
#[derive(Debug, Clone)]
pub struct VenueTradeRecord {
    pub cycle: i64,
    pub venue_id: String,
    pub symbol: String,
    pub asset_class: String,
    pub display_name: Option<String>,
    pub side: String,
    pub entry_price: String,
    pub quantity: String,
    pub avg_fill_price: Option<String>,
    pub edge_at_entry: String,
    pub fair_value: String,
    pub confidence: String,
    pub risk_pct: String,
    pub stop_pct: String,
    pub status: String,
    pub stop_price: Option<String>,
    pub target_price: Option<String>,
    pub horizon_hours: Option<i64>,
    pub client_order_id: Option<String>,
    pub venue_order_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_store_create_and_migrate() {
        let store = Store::new(":memory:").await.expect("should create store");
        // Verify tables exist by inserting a cycle
        let cycle = CycleRecord {
            id: None,
            cycle_number: 1,
            markets_scanned: Some(50),
            opportunities_found: Some(3),
            trades_placed: Some(1),
            api_cost: Some("0.05".to_string()),
            bankroll: Some("100.00".to_string()),
            unrealized_pnl: Some("0.00".to_string()),
            agent_state: "ALIVE".to_string(),
            duration_ms: Some(1500),
            created_at: None,
        };
        let id = store
            .insert_cycle(&cycle)
            .await
            .expect("should insert cycle");
        assert!(id > 0);
    }

    #[tokio::test]
    async fn test_trade_insert_and_query() {
        let store = Store::new(":memory:").await.expect("should create store");
        let trade = TradeRecord {
            id: None,
            cycle: 1,
            market_id: "0xabc".to_string(),
            market_question: Some("Will it rain?".to_string()),
            direction: "YES".to_string(),
            entry_price: "0.65".to_string(),
            size: "10.00".to_string(),
            edge_at_entry: "0.12".to_string(),
            claude_fair_value: "0.77".to_string(),
            confidence: "0.85".to_string(),
            kelly_raw: "0.04".to_string(),
            kelly_adjusted: "0.02".to_string(),
            status: "OPEN".to_string(),
            pnl: None,
            created_at: None,
            resolved_at: None,
        };
        let id = store
            .insert_trade(&trade)
            .await
            .expect("should insert trade");
        assert!(id > 0);

        let open = store
            .get_open_trades()
            .await
            .expect("should get open trades");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].market_id, "0xabc");
    }
}
