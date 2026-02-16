use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use serde::Serialize;
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

    async fn migrate(&self) -> Result<()> {
        let migration_sql = include_str!("../../migrations/001_init.sql");
        // Execute each statement separately (sqlx doesn't support multiple statements in one call)
        for statement in migration_sql.split(';') {
            let trimmed = statement.trim();
            if !trimmed.is_empty() {
                sqlx::query(trimmed)
                    .execute(&self.pool)
                    .await
                    .with_context(|| format!("Failed to execute migration: {trimmed}"))?;
            }
        }
        Ok(())
    }

    // --- Trade operations ---

    pub async fn insert_trade(&self, trade: &TradeRecord) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO trades (cycle, market_id, market_question, direction, entry_price, size, edge_at_entry, claude_fair_value, confidence, kelly_raw, kelly_adjusted, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(trade.cycle)
        .bind(&trade.market_id)
        .bind(&trade.market_question)
        .bind(&trade.direction)
        .bind(&trade.entry_price)
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
        let trades =
            sqlx::query_as::<_, TradeRecord>("SELECT * FROM trades WHERE market_id = ?")
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
        let cycles =
            sqlx::query_as::<_, CycleRecord>("SELECT * FROM cycles ORDER BY cycle_number")
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
        let trades = sqlx::query_as::<_, TradeRecord>(
            "SELECT * FROM trades ORDER BY id DESC LIMIT ?",
        )
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
        let id = store.insert_cycle(&cycle).await.expect("should insert cycle");
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
        let id = store.insert_trade(&trade).await.expect("should insert trade");
        assert!(id > 0);

        let open = store.get_open_trades().await.expect("should get open trades");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].market_id, "0xabc");
    }
}
