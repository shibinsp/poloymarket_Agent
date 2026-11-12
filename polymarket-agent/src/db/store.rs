use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};
use std::str::FromStr;

use crate::venue::types::PROTECTIVE_STOP_PREFIX;

pub struct Store {
    pool: SqlitePool,
}

#[derive(Debug, Clone, FromRow, Serialize, Default)]
pub struct TradeRecord {
    pub id: Option<i64>,
    pub cycle: i64,
    #[sqlx(default)]
    pub venue_id: Option<String>,
    pub market_id: String,
    #[sqlx(default)]
    pub symbol: Option<String>,
    #[sqlx(default)]
    pub asset_class: Option<String>,
    pub market_question: Option<String>,
    pub direction: String,
    #[sqlx(default)]
    pub side: Option<String>,
    pub entry_price: String,
    pub size: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(default)]
    pub quantity: Option<String>,
    #[sqlx(default)]
    pub avg_fill_price: Option<String>,
    pub edge_at_entry: String,
    pub claude_fair_value: String,
    pub confidence: String,
    pub kelly_raw: String,
    pub kelly_adjusted: String,
    pub status: String,
    pub pnl: Option<String>,
    #[sqlx(default)]
    pub realized_pnl: Option<String>,
    #[sqlx(default)]
    pub unrealized_pnl: Option<String>,
    #[sqlx(default)]
    pub mark_price: Option<String>,
    #[sqlx(default)]
    pub marked_at: Option<String>,
    #[sqlx(default)]
    pub close_reason: Option<String>,
    #[sqlx(default)]
    pub closed_at: Option<String>,
    #[sqlx(default)]
    pub venue_order_id: Option<String>,
    #[sqlx(default)]
    pub client_order_id: Option<String>,
    #[sqlx(default)]
    pub stop_price: Option<String>,
    #[sqlx(default)]
    pub target_price: Option<String>,
    #[sqlx(default)]
    pub strategy: Option<String>,
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
        // `quantity` stays NULL for Polymarket orders unless explicitly supplied.
        .bind(&trade.quantity)
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

    /// Tag a trade with the strategy that opened it.
    pub async fn set_trade_strategy(&self, id: i64, strategy: &str) -> Result<()> {
        sqlx::query("UPDATE trades SET strategy = ? WHERE id = ?")
            .bind(strategy)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("Failed to tag trade strategy")?;
        Ok(())
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
    /// Trades whose outcome is final, whichever way they got there.
    ///
    /// A prediction market settles into RESOLVED_WIN/RESOLVED_LOSS; a
    /// continuous position is closed by trading out of it and lands on CLOSED.
    /// Selecting only the first two made every crypto and equity trade
    /// invisible to performance metrics — realized P&L, win rate and the
    /// Sharpe input all read zero while `total_trades` counted them, so the
    /// figures were not merely incomplete but mutually inconsistent.
    pub async fn get_resolved_trades(&self) -> Result<Vec<TradeRecord>> {
        let trades = sqlx::query_as::<_, TradeRecord>(
            "SELECT * FROM trades WHERE status IN ('RESOLVED_WIN', 'RESOLVED_LOSS', 'CLOSED') ORDER BY COALESCE(resolved_at, closed_at)",
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

    /// Lifetime API spend.
    ///
    /// Summed in SQLite over the TEXT column, not in Rust. The float
    /// round-trip this used to do (`SUM(CAST(cost AS REAL))`) is gone, but so
    /// is the fix that replaced it: `api_costs` gains a row per model call
    /// forever, and `/api/metrics` calls this on every dashboard poll, so
    /// streaming the table decoded tens of thousands of rows to produce one
    /// display number.
    ///
    /// `SUM` over TEXT still goes through a float internally, which is why
    /// this is used for display only — the budget ledger and the daily cap
    /// read `get_today_api_cost`, which is bounded by the day and summed
    /// exactly in `Decimal`.
    pub async fn get_total_api_cost(&self) -> Result<Decimal> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT cost FROM api_costs")
            .fetch_all(&self.pool)
            .await
            .context("Failed to get total API cost")?;
        sum_money(&rows, "api_costs.cost")
    }

    /// Get total API spend for the current UTC day.
    pub async fn get_today_api_cost(&self) -> Result<Decimal> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT cost FROM api_costs WHERE created_at >= date('now')")
                .fetch_all(&self.pool)
                .await
                .context("Failed to get today's API cost")?;
        sum_money(&rows, "api_costs.cost")
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
        // Previously `fetch_one` against a SUM, which returns exactly one row
        // even when nothing matches. Now that the rows are summed in Rust,
        // `fetch_all` is both correct and what a cycle with no calls needs.
        let rows: Vec<(String,)> = sqlx::query_as("SELECT cost FROM api_costs WHERE cycle = ?")
            .bind(cycle)
            .fetch_all(&self.pool)
            .await
            .context("Failed to get API cost for cycle")?;
        sum_money(&rows, "api_costs.cost")
    }

    // === Orders (multi-venue) ===

    /// Record a submitted order. Written *before* the venue call returns, so
    /// an order that times out still leaves a row to reconcile against — the
    /// alternative is an order live at the venue that we have no record of.
    pub async fn insert_order(&self, order: &OrderRecord) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO orders (client_order_id, venue_order_id, venue_id, symbol, side, intent, trade_id, limit_price, qty, filled_qty, avg_fill_price, state, reject_reason, cycle, mid_at_submit, expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(&order.mid_at_submit)
        .bind(&order.expires_at)
        .execute(&self.pool)
        .await
        .context("Failed to insert order")?;

        Ok(result.last_insert_rowid())
    }

    /// Update an order after querying the venue for its fate.
    /// Record what the venue now says about an order.
    ///
    /// `None` for an optional field means "nothing new to record", never
    /// "erase what is there" — hence COALESCE on all three. That matters most
    /// for `reject_reason`, which for an EXIT order carries the *exit* reason
    /// (STOP_LOSS, TAKE_PROFIT, MAX_HOLD) written when the order was placed:
    /// without COALESCE the very next successful update nulls it, and every
    /// exit that did not fill on submission closes with a generic "EXIT" in
    /// `trades.close_reason`, losing the only record of why the agent sold.
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
            "UPDATE orders SET state = ?, venue_order_id = COALESCE(?, venue_order_id), filled_qty = ?, avg_fill_price = COALESCE(?, avg_fill_price), reject_reason = COALESCE(?, reject_reason), updated_at = ? WHERE client_order_id = ?",
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

    /// Whether the venue has refused a protective stop for this trade.
    pub async fn protective_stop_rejected(&self, trade_id: i64) -> Result<bool> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM orders WHERE trade_id = ? AND state = 'REJECTED' AND client_order_id LIKE ?",
        )
        .bind(trade_id)
        .bind(format!("{PROTECTIVE_STOP_PREFIX}%"))
        .fetch_one(&self.pool)
        .await
        .context("Failed to look up rejected protective stops")?;
        Ok(count > 0)
    }

    /// Open continuous-asset positions, which unlike prediction markets never
    /// settle themselves and must be explicitly closed.
    pub async fn get_open_venue_trades(&self) -> Result<Vec<VenueOpenTrade>> {
        let rows = sqlx::query_as::<_, VenueOpenTradeRow>(
            "SELECT id, venue_id, symbol, asset_class, side, strategy, entry_price, avg_fill_price, quantity, stop_pct, stop_price, target_price, horizon_hours, created_at
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

    /// Move an open position's protective stop. Callers only ever raise it.
    pub async fn update_trade_stop(&self, id: i64, stop_price: &str) -> Result<()> {
        sqlx::query("UPDATE trades SET stop_price = ? WHERE id = ?")
            .bind(stop_price)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("Failed to update trade stop")?;
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

    /// Trade linked to an order, with its current status, if one was recorded.
    pub async fn trade_for_order(&self, client_order_id: &str) -> Result<Option<(i64, String)>> {
        sqlx::query_as("SELECT id, status FROM trades WHERE client_order_id = ? LIMIT 1")
            .bind(client_order_id)
            .fetch_optional(&self.pool)
            .await
            .context("Failed to look up trade by client order id")
    }

    /// Current status of a trade, whatever its stage.
    pub async fn trade_status(&self, id: i64) -> Result<Option<String>> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT status FROM trades WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .context("Failed to fetch trade status")?;
        Ok(row.and_then(|r| r.0))
    }

    /// Point an order row at the trade whose thesis it carries.
    pub async fn link_order_to_trade(&self, client_order_id: &str, trade_id: i64) -> Result<()> {
        sqlx::query("UPDATE orders SET trade_id = ? WHERE client_order_id = ?")
            .bind(trade_id)
            .bind(client_order_id)
            .execute(&self.pool)
            .await
            .context("Failed to link order to trade")?;
        Ok(())
    }

    /// Entry price and quantity of any trade, open or not — needed to price an
    /// exit that filled after the cycle that placed it.
    pub async fn get_trade_entry(&self, id: i64) -> Result<Option<(Decimal, Decimal)>> {
        let row: Option<(Option<String>, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT entry_price, avg_fill_price, quantity FROM trades WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .context("Failed to fetch trade entry")?;

        let Some((entry, avg_fill, qty)) = row else {
            return Ok(None);
        };
        // Prefer the actual fill; fall back to the intended entry.
        let price = avg_fill.or(entry).unwrap_or_default();
        // Fail loud: a silently-zeroed entry price turns a loss into a
        // reported profit.
        let parse = |v: &str, field: &str| -> Result<Decimal> {
            Decimal::from_str(v).with_context(|| format!("Invalid decimal in {field}: {v:?}"))
        };
        Ok(Some((
            parse(&price, "trades.entry_price")?,
            parse(qty.as_deref().unwrap_or("0"), "trades.quantity")?,
        )))
    }

    /// Promote a pending trade to an open position once its entry filled.
    pub async fn activate_trade(
        &self,
        id: i64,
        avg_fill_price: &str,
        quantity: &str,
        status: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE trades SET status = ?, avg_fill_price = ?, entry_price = ?, quantity = ?, size = ? WHERE id = ?",
        )
        .bind(status)
        .bind(avg_fill_price)
        .bind(avg_fill_price)
        .bind(quantity)
        .bind(quantity)
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to activate trade")?;
        Ok(())
    }

    /// Mark a trade that never opened — its entry was rejected, cancelled or
    /// expired without filling.
    pub async fn cancel_trade(&self, id: i64, reason: &str) -> Result<()> {
        sqlx::query(
            "UPDATE trades SET status = 'CANCELLED', close_reason = ?, closed_at = ? WHERE id = ?",
        )
        .bind(reason)
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to cancel trade")?;
        Ok(())
    }

    /// Record a trade opened on a venue, once something has actually filled.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_venue_trade(&self, trade: &VenueTradeRecord) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO trades (cycle, venue_id, symbol, asset_class, market_id, market_question, direction, side, entry_price, size, quantity, avg_fill_price, edge_at_entry, claude_fair_value, confidence, kelly_raw, kelly_adjusted, risk_pct, stop_pct, status, stop_price, target_price, horizon_hours, client_order_id, venue_order_id, strategy)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        // `size` is the legacy notional column, so it gets the cash value of
        // the position; `quantity` is units. Binding units to both made the
        // dashboard's "Size" mean dollars for one kind of trade and units for
        // the other.
        .bind(&trade.notional)
        .bind(&trade.quantity)
        .bind(&trade.avg_fill_price)
        .bind(&trade.edge_at_entry)
        .bind(&trade.fair_value)
        .bind(&trade.confidence)
        // Kelly is not how continuous assets are sized — they use ATR
        // volatility targeting — so these are zero rather than borrowed to
        // carry the risk fraction and stop distance, which is what they used
        // to hold. Those now have columns of their own (migration 003).
        .bind("0")
        .bind("0")
        .bind(&trade.risk_pct)
        .bind(&trade.stop_pct)
        .bind(&trade.status)
        .bind(&trade.stop_price)
        .bind(&trade.target_price)
        .bind(trade.horizon_hours)
        .bind(&trade.client_order_id)
        .bind(&trade.venue_order_id)
        .bind(&trade.strategy)
        .execute(&self.pool)
        .await
        .context("Failed to insert venue trade")?;

        Ok(result.last_insert_rowid())
    }

    // ---------------------------------------------------------------------
    // Phase 3: equity marks, halts, and the counters the breakers read.
    // ---------------------------------------------------------------------

    /// Record today's equity and return the marks the circuit breaker needs.
    ///
    /// The first call on a new UTC day fixes that day's *starting* equity —
    /// every later call on the same day leaves it alone. Recomputing it would
    /// make the daily-loss breaker measure the loss since the last cycle
    /// rather than since the open, which is a limit that can never be reached
    /// no matter how much is lost.
    ///
    /// The high-water mark is kept across days, not within one: drawdown is
    /// peak-to-trough over the life of the account, and resetting the peak
    /// every midnight would hide a slow bleed completely.
    pub async fn record_equity(
        &self,
        day: chrono::NaiveDate,
        equity: Decimal,
    ) -> Result<crate::risk::circuit_breaker::DayMarks> {
        let day_str = day.to_string();

        // Carry the peak forward from whatever the account has ever reached.
        //
        // Every stored mark is read and compared in `Decimal`, not picked by a
        // SQL `MAX` over a float cast. The number of rows is one per day, so
        // reading them all costs nothing, and money never goes out through a
        // binary float and back — the discipline the rest of this file keeps.
        let marks: Vec<(String,)> = sqlx::query_as("SELECT high_water_mark FROM daily_equity")
            .fetch_all(&self.pool)
            .await
            .context("Failed to read the prior high-water marks")?;
        let prior_peak = marks
            .iter()
            .map(|(v,)| parse_money(v, "daily_equity.high_water_mark"))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .max()
            .unwrap_or(Decimal::ZERO);

        let high_water_mark = prior_peak.max(equity);

        // The first mark of a new day is also the true close of the day
        // before: nothing trades between the U.S. close and UTC midnight's
        // next reading. If the agent was down at the close (the laptop
        // restarted overnight on 11 Nov 2026), the previous day's
        // closing_equity is a stale mid-session mark (it showed -$24.22
        // while the day really made +$14.78). Carry this first mark back as
        // that close.
        let is_new_day: bool =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM daily_equity WHERE day = ?")
                .bind(&day_str)
                .fetch_one(&self.pool)
                .await
                .context("Failed to check for today's equity row")?
                == 0;
        if is_new_day {
            sqlx::query(
                "UPDATE daily_equity SET closing_equity = ?, updated_at = datetime('now')
                 WHERE day = (SELECT MAX(day) FROM daily_equity WHERE day < ?)",
            )
            .bind(equity.to_string())
            .bind(&day_str)
            .execute(&self.pool)
            .await
            .context("Failed to close the previous day's equity")?;
        }

        sqlx::query(
            "INSERT INTO daily_equity (day, starting_equity, high_water_mark, closing_equity, updated_at)
             VALUES (?, ?, ?, ?, datetime('now'))
             ON CONFLICT(day) DO UPDATE SET
                 high_water_mark = excluded.high_water_mark,
                 closing_equity = excluded.closing_equity,
                 updated_at = datetime('now')",
        )
        .bind(&day_str)
        .bind(equity.to_string())
        .bind(high_water_mark.to_string())
        .bind(equity.to_string())
        .execute(&self.pool)
        .await
        .context("Failed to record daily equity")?;

        let row: (String, String) = sqlx::query_as(
            "SELECT starting_equity, high_water_mark FROM daily_equity WHERE day = ?",
        )
        .bind(&day_str)
        .fetch_one(&self.pool)
        .await
        .context("Failed to read back daily equity")?;

        Ok(crate::risk::circuit_breaker::DayMarks {
            day,
            starting_equity: parse_money(&row.0, "daily_equity.starting_equity")?,
            high_water_mark: parse_money(&row.1, "daily_equity.high_water_mark")?,
        })
    }

    /// Positions opened during the given UTC day.
    ///
    /// Counts entries, not fills: the limit is on how many times the agent is
    /// willing to take a new view in a day, and an entry that filled in three
    /// parts is still one decision.
    pub async fn count_trades_opened_on(&self, day: chrono::NaiveDate) -> Result<u32> {
        // Polymarket paper bets are left out: the underdog loop places many
        // $2 bets a day, bounded by its own $50 cash, and on 11 Nov 2026 its
        // 21 bets tripped this shared cap and halted the stock books for the
        // whole U.S. session.
        let count_legacy: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM trades WHERE date(created_at) = ? AND venue_id <> 'polymarket'",
        )
        .bind(day.to_string())
        .fetch_one(&self.pool)
        .await
        .context("Failed to count today's trades")?;
        let count_us: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM us_paper_orders WHERE date(created_at) = ?")
                .bind(day.to_string())
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);
        let count_india: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sandbox_orders WHERE date(created_at) = ?")
                .bind(day.to_string())
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);
        let total = (count_legacy + count_us + count_india).max(0);
        Ok(total as u32)
    }

    /// Count U.S. equity entry attempts for one venue on a UTC date.
    ///
    /// The venue path persists a trade row for every submitted entry, including
    /// a pending order, so this remains reliable across restarts and counts
    /// orders whose acknowledgement was lost as well as completed fills.
    pub async fn count_venue_equity_entries_opened_on(
        &self,
        venue_id: &str,
        day: chrono::NaiveDate,
    ) -> Result<u32> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM trades
             WHERE venue_id = ? AND asset_class = 'equity' AND side IN ('BUY', 'SELL')
               AND date(created_at) = ?",
        )
        .bind(venue_id)
        .bind(day.to_string())
        .fetch_one(&self.pool)
        .await
        .context("Failed to count venue equity entries")?;

        Ok(count.max(0) as u32)
    }

    /// Open a position in one strategy's NSE intraday paper book.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_intraday_trade(
        &self,
        strategy: &str,
        day: &str,
        symbol: &str,
        is_buy: bool,
        qty: i64,
        entry_price: &str,
        stop_price: &str,
        capital: &str,
        at: DateTime<Utc>,
    ) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO india_intraday_trades (strategy, day, symbol, is_buy, qty, entry_price, stop_price, capital, entry_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(strategy)
        .bind(day)
        .bind(symbol)
        .bind(is_buy)
        .bind(qty)
        .bind(entry_price)
        .bind(stop_price)
        .bind(capital)
        .bind(at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("Failed to open intraday trade")?;
        Ok(result.last_insert_rowid())
    }

    #[allow(clippy::type_complexity)]
    pub async fn open_intraday_trades(&self, strategy: &str) -> Result<Vec<IntradayOpen>> {
        let rows: Vec<(i64, String, String, bool, i64, String, String, String)> = sqlx::query_as(
            "SELECT id, day, symbol, is_buy, qty, entry_price, stop_price, entry_at
             FROM india_intraday_trades WHERE exit_at IS NULL AND strategy = ? ORDER BY id",
        )
        .bind(strategy)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read open intraday trades")?;
        rows.into_iter()
            .map(|(id, day, symbol, is_buy, qty, entry, stop, at)| {
                Ok(IntradayOpen {
                    id,
                    day,
                    symbol,
                    is_buy,
                    qty,
                    entry_price: Decimal::from_str(&entry).context("bad intraday entry price")?,
                    stop_price: Decimal::from_str(&stop).context("bad intraday stop price")?,
                    entry_at: DateTime::parse_from_rfc3339(&at)
                        .context("bad intraday entry time")?
                        .with_timezone(&Utc),
                })
            })
            .collect()
    }

    pub async fn close_intraday_trade(
        &self,
        id: i64,
        exit_price: &str,
        reason: &str,
        charges: &str,
        pnl: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE india_intraday_trades SET exit_price = ?, exit_reason = ?, charges = ?, pnl = ?, exit_at = ? WHERE id = ?",
        )
        .bind(exit_price)
        .bind(reason)
        .bind(charges)
        .bind(pnl)
        .bind(at.to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to close intraday trade")?;
        Ok(())
    }

    /// One strategy's (trades opened on `day`, P&L closed on `day`, P&L
    /// closed ever).
    pub async fn intraday_totals(
        &self,
        strategy: &str,
        day: &str,
    ) -> Result<(i64, Decimal, Decimal)> {
        let rows: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT day, pnl FROM india_intraday_trades WHERE strategy = ?")
                .bind(strategy)
                .fetch_all(&self.pool)
                .await
                .context("Failed to read intraday totals")?;
        let (mut count, mut today, mut all) = (0i64, Decimal::ZERO, Decimal::ZERO);
        for (d, pnl) in rows {
            let v = pnl
                .as_deref()
                .and_then(|p| Decimal::from_str(p).ok())
                .unwrap_or_default();
            all += v;
            if d == day {
                count += 1;
                today += v;
            }
        }
        Ok((count, today, all))
    }

    /// Whether the pre-FOMC book already bought for this meeting.
    pub async fn prefomc_has(&self, symbol: &str, fomc_day: &str) -> Result<bool> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM us_prefomc_trades WHERE symbol = ? AND fomc_day = ?",
        )
        .bind(symbol)
        .bind(fomc_day)
        .fetch_one(&self.pool)
        .await
        .context("Failed to read pre-FOMC trades")?;
        Ok(n > 0)
    }

    /// Open pre-FOMC positions: (id, qty, entry price).
    pub async fn prefomc_open(&self) -> Result<Vec<(i64, String, String)>> {
        sqlx::query_as(
            "SELECT id, qty, entry_price FROM us_prefomc_trades WHERE exit_at IS NULL ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to read open pre-FOMC trades")
    }

    pub async fn prefomc_realized(&self) -> Result<Decimal> {
        let rows: Vec<(Option<String>,)> =
            sqlx::query_as("SELECT pnl FROM us_prefomc_trades WHERE pnl IS NOT NULL")
                .fetch_all(&self.pool)
                .await
                .context("Failed to read pre-FOMC P&L")?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.0.and_then(|p| Decimal::from_str(&p).ok()))
            .sum())
    }

    pub async fn prefomc_open_trade(
        &self,
        symbol: &str,
        qty: Decimal,
        fomc_day: &str,
        day: &str,
        price: Decimal,
        at: DateTime<Utc>,
    ) -> Result<i64> {
        let r = sqlx::query(
            "INSERT INTO us_prefomc_trades (symbol, qty, fomc_day, entry_day, entry_price, entry_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(symbol)
        .bind(qty.to_string())
        .bind(fomc_day)
        .bind(day)
        .bind(price.to_string())
        .bind(at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("Failed to open a pre-FOMC trade")?;
        Ok(r.last_insert_rowid())
    }

    pub async fn prefomc_close_trade(
        &self,
        id: i64,
        day: &str,
        price: Decimal,
        pnl: Decimal,
        at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query("UPDATE us_prefomc_trades SET exit_day = ?, exit_price = ?, pnl = ?, exit_at = ? WHERE id = ? AND exit_at IS NULL")
            .bind(day)
            .bind(price.to_string())
            .bind(pnl.round_dp(4).to_string())
            .bind(at.to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await
            .context("Failed to close a pre-FOMC trade")?;
        Ok(())
    }

    /// Every pre-FOMC trade, newest first, as JSON.
    pub async fn prefomc_trades(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, String, String, String, String, String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT id, symbol, qty, fomc_day, entry_day, entry_price, exit_day, exit_price, pnl FROM us_prefomc_trades ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read pre-FOMC trades")?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.0, "symbol": r.1, "qty": r.2, "fomc_day": r.3, "entry_day": r.4, "entry_price": r.5,
                    "exit_day": r.6, "exit_price": r.7, "pnl": r.8, "status": if r.6.is_some() { "CLOSED" } else { "OPEN" },
                })
            })
            .collect())
    }

    /// Whether the Turnaround Tuesday book already acted on this Monday (a trade or a skip).
    pub async fn turnaround_entered_on(&self, symbol: &str, day: &str) -> Result<bool> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM us_turnaround_trades WHERE symbol = ? AND entry_day = ?",
        )
        .bind(symbol)
        .bind(day)
        .fetch_one(&self.pool)
        .await
        .context("Failed to read Turnaround Tuesday entries")?;
        Ok(n > 0)
    }

    /// Record a Monday with no signal, so a restart does not re-decide it.
    /// Stored as a closed zero-size row (qty 0, pnl 0) marked SKIP.
    pub async fn turnaround_mark_skip(&self, symbol: &str, day: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT OR IGNORE INTO us_turnaround_trades (symbol, qty, entry_day, entry_price, friday_close, sma25, entry_at, exit_day, exit_price, exit_at, pnl)
             VALUES (?, '0', ?, '0', 'SKIP', 'SKIP', ?, ?, '0', ?, '0')",
        )
        .bind(symbol)
        .bind(day)
        .bind(&now)
        .bind(day)
        .bind(&now)
        .execute(&self.pool)
        .await
        .context("Failed to record a Turnaround Tuesday skip")?;
        Ok(())
    }

    /// Open Turnaround Tuesday positions: (id, symbol, qty, entry price, entry day).
    pub async fn turnaround_open(&self) -> Result<Vec<(i64, String, String, String, String)>> {
        sqlx::query_as(
            "SELECT id, symbol, qty, entry_price, entry_day FROM us_turnaround_trades WHERE exit_at IS NULL ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to read open Turnaround Tuesday trades")
    }

    /// Sum of closed Turnaround Tuesday P&L.
    pub async fn turnaround_realized(&self) -> Result<Decimal> {
        let rows: Vec<(Option<String>,)> =
            sqlx::query_as("SELECT pnl FROM us_turnaround_trades WHERE pnl IS NOT NULL")
                .fetch_all(&self.pool)
                .await
                .context("Failed to read Turnaround Tuesday P&L")?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.0.and_then(|p| Decimal::from_str(&p).ok()))
            .sum())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn turnaround_open_trade(
        &self,
        symbol: &str,
        qty: Decimal,
        day: &str,
        price: Decimal,
        friday_close: Decimal,
        sma25: Decimal,
        at: DateTime<Utc>,
    ) -> Result<i64> {
        let r = sqlx::query(
            "INSERT INTO us_turnaround_trades (symbol, qty, entry_day, entry_price, friday_close, sma25, entry_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(symbol)
        .bind(qty.to_string())
        .bind(day)
        .bind(price.to_string())
        .bind(friday_close.to_string())
        .bind(sma25.round_dp(4).to_string())
        .bind(at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("Failed to open a Turnaround Tuesday trade")?;
        Ok(r.last_insert_rowid())
    }

    pub async fn turnaround_close_trade(
        &self,
        id: i64,
        day: &str,
        price: Decimal,
        pnl: Decimal,
        at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE us_turnaround_trades SET exit_day = ?, exit_price = ?, pnl = ?, exit_at = ? WHERE id = ? AND exit_at IS NULL",
        )
        .bind(day)
        .bind(price.to_string())
        .bind(pnl.round_dp(4).to_string())
        .bind(at.to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to close a Turnaround Tuesday trade")?;
        Ok(())
    }

    /// Every Turnaround Tuesday trade (skip weeks left out), newest first, as JSON.
    pub async fn turnaround_trades(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, String, String, String, String, String, String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT id, symbol, qty, entry_day, entry_price, friday_close, sma25, exit_day, exit_price, pnl
             FROM us_turnaround_trades WHERE friday_close <> 'SKIP' ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read Turnaround Tuesday trades")?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.0, "symbol": r.1, "qty": r.2, "entry_day": r.3, "entry_price": r.4,
                    "friday_close": r.5, "sma25": r.6, "exit_day": r.7, "exit_price": r.8, "pnl": r.9,
                    "status": if r.7.is_some() { "CLOSED" } else { "OPEN" },
                })
            })
            .collect())
    }

    /// Open IBS positions: (id, symbol, qty, entry price, entry day).
    pub async fn ibs_open(&self) -> Result<Vec<(i64, String, String, String, String)>> {
        sqlx::query_as(
            "SELECT id, symbol, qty, entry_price, entry_day FROM us_ibs_trades WHERE exit_at IS NULL ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to read open IBS trades")
    }

    /// Sum of closed IBS P&L.
    pub async fn ibs_realized(&self) -> Result<Decimal> {
        let rows: Vec<(Option<String>,)> =
            sqlx::query_as("SELECT pnl FROM us_ibs_trades WHERE pnl IS NOT NULL")
                .fetch_all(&self.pool)
                .await
                .context("Failed to read IBS P&L")?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.0.and_then(|p| Decimal::from_str(&p).ok()))
            .sum())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ibs_open_trade(
        &self,
        symbol: &str,
        qty: Decimal,
        day: &str,
        price: Decimal,
        ibs: Decimal,
        at: DateTime<Utc>,
    ) -> Result<i64> {
        let r = sqlx::query(
            "INSERT INTO us_ibs_trades (symbol, qty, entry_day, entry_price, entry_ibs, entry_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(symbol)
        .bind(qty.to_string())
        .bind(day)
        .bind(price.to_string())
        .bind(ibs.round_dp(3).to_string())
        .bind(at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("Failed to open an IBS trade")?;
        Ok(r.last_insert_rowid())
    }

    pub async fn ibs_close_trade(
        &self,
        id: i64,
        day: &str,
        price: Decimal,
        ibs: Decimal,
        pnl: Decimal,
        at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE us_ibs_trades SET exit_day = ?, exit_price = ?, exit_ibs = ?, pnl = ?, exit_at = ? WHERE id = ? AND exit_at IS NULL",
        )
        .bind(day)
        .bind(price.to_string())
        .bind(ibs.round_dp(3).to_string())
        .bind(pnl.round_dp(4).to_string())
        .bind(at.to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to close an IBS trade")?;
        Ok(())
    }

    /// Every IBS trade, newest first, as JSON.
    pub async fn ibs_trades(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, String, String, String, String, String, Option<String>, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT id, symbol, qty, entry_day, entry_price, entry_ibs, exit_day, exit_price, exit_ibs, pnl
             FROM us_ibs_trades ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read IBS trades")?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.0, "symbol": r.1, "qty": r.2, "entry_day": r.3, "entry_price": r.4,
                    "entry_ibs": r.5, "exit_day": r.6, "exit_price": r.7, "exit_ibs": r.8, "pnl": r.9,
                    "status": if r.6.is_some() { "CLOSED" } else { "OPEN" },
                })
            })
            .collect())
    }

    /// Closed IBS trades for the ladder: (id, exit day, symbol, qty, entry, pnl).
    #[allow(clippy::type_complexity)]
    pub async fn us_ladder_ibs_sources(
        &self,
    ) -> Result<Vec<(i64, String, String, String, String, String)>> {
        sqlx::query_as(
            "SELECT id, exit_day, symbol, qty, entry_price, pnl FROM us_ibs_trades WHERE pnl IS NOT NULL ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to read IBS trades for the ladder")
    }

    /// Closed US breakout trades from `since_utc` on: (id, symbol, created_at,
    /// quantity, avg fill, realized pnl).
    #[allow(clippy::type_complexity)]
    pub async fn us_ladder_orb_sources(
        &self,
        since_utc: &str,
    ) -> Result<Vec<(i64, String, String, String, String, String)>> {
        sqlx::query_as(
            "SELECT id, symbol, created_at, CAST(quantity AS TEXT), CAST(avg_fill_price AS TEXT), CAST(realized_pnl AS TEXT)
             FROM trades WHERE venue_id = 'alpaca' AND strategy = 'orb' AND status = 'CLOSED'
               AND realized_pnl IS NOT NULL AND avg_fill_price IS NOT NULL
               AND COALESCE(close_reason, '') NOT LIKE 'Failed%' AND created_at >= ?
             ORDER BY id",
        )
        .bind(since_utc)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read breakout trades for the ladder")
    }

    /// Every US breakout entry's created_at from `since_utc` on, closed or not.
    pub async fn us_ladder_orb_entries(&self, since_utc: &str) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT created_at FROM trades WHERE venue_id = 'alpaca' AND strategy = 'orb'
               AND avg_fill_price IS NOT NULL AND created_at >= ?
               AND COALESCE(close_reason, '') NOT LIKE 'Failed%'",
        )
        .bind(since_utc)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read breakout entries for the ladder")?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    /// Closed option trades from `since_day`: (id, day, contract, entry, exit).
    pub async fn us_ladder_option_sources(
        &self,
        since_day: &str,
    ) -> Result<Vec<(i64, String, String, String, String)>> {
        sqlx::query_as(
            "SELECT id, day, contract, entry_price, exit_price FROM us_option_trades
             WHERE status = 'CLOSED' AND entry_price IS NOT NULL AND exit_price IS NOT NULL AND day >= ?
             ORDER BY id",
        )
        .bind(since_day)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read option trades for the ladder")
    }

    /// Spread trades from `since_day`: (id, day, underlying, short, long,
    /// credit, debit, status). Open ones included so a day waits for all.
    #[allow(clippy::type_complexity)]
    pub async fn us_ladder_spread_sources(
        &self,
        since_day: &str,
    ) -> Result<
        Vec<(
            i64,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        )>,
    > {
        sqlx::query_as(
            "SELECT id, day, underlying, short_strike, long_strike, entry_credit, exit_debit, status
             FROM us_spread_trades WHERE day >= ? ORDER BY id",
        )
        .bind(since_day)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read spread trades for the ladder")
    }

    /// Source ids already on a ladder book.
    pub async fn us_ladder_done(&self, book: &str) -> Result<std::collections::HashSet<i64>> {
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT DISTINCT source_id FROM us_ladder_trades WHERE book = ?")
                .bind(book)
                .fetch_all(&self.pool)
                .await
                .context("Failed to read the US ladder")?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    /// One amount's ladder rows: (day, units, pnl).
    pub async fn us_ladder_rows(
        &self,
        book: &str,
        amount: i64,
    ) -> Result<Vec<(String, String, String)>> {
        sqlx::query_as(
            "SELECT day, units, pnl FROM us_ladder_trades WHERE book = ? AND amount = ? ORDER BY id",
        )
        .bind(book)
        .bind(amount)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read US ladder rows")
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_us_ladder(
        &self,
        book: &str,
        amount: i64,
        source_id: i64,
        day: &str,
        symbol: &str,
        units: Decimal,
        pnl: Decimal,
        at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO us_ladder_trades (book, amount, source_id, day, symbol, units, pnl, recorded_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(book)
        .bind(amount)
        .bind(source_id)
        .bind(day)
        .bind(symbol)
        .bind(units.to_string())
        .bind(pnl.round_dp(2).to_string())
        .bind(at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("Failed to record a US ladder row")?;
        Ok(())
    }

    /// Sum of a book's losing trades only (zero or negative). Profits are set
    /// aside, so they never offset a loss in the cash the book may trade.
    pub async fn intraday_losses(&self, strategy: &str) -> Result<Decimal> {
        let rows: Vec<(Option<String>,)> =
            sqlx::query_as("SELECT pnl FROM india_intraday_trades WHERE strategy = ?")
                .bind(strategy)
                .fetch_all(&self.pool)
                .await
                .context("Failed to read intraday losses")?;
        Ok(rows
            .into_iter()
            .filter_map(|(p,)| p.as_deref().and_then(|p| Decimal::from_str(p).ok()))
            .filter(|v| v.is_sign_negative())
            .sum())
    }

    /// ORB and burst stock trades opened at or after `since` (UTC, the
    /// `created_at` format): (id, symbol, side, status). The options book
    /// mirrors these.
    pub async fn us_orb_stock_trades_since(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<(i64, String, String, String)>> {
        sqlx::query_as(
            "SELECT id, symbol, side, status FROM trades
             WHERE strategy IN ('orb', 'us_burst') AND created_at >= ?
             ORDER BY id",
        )
        .bind(since.format("%Y-%m-%d %H:%M:%S").to_string())
        .fetch_all(&self.pool)
        .await
        .context("Failed to read ORB stock trades")
    }

    /// Record an options entry order as sent.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_option_trade(
        &self,
        day: &str,
        stock_trade_id: i64,
        underlying: &str,
        contract: &str,
        is_call: bool,
        qty: i64,
        limit_price: &str,
        client_order_id: &str,
        venue_order_id: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO us_option_trades (day, stock_trade_id, underlying, contract, is_call, qty,
                 limit_price, client_order_id, venue_order_id, status, entry_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'PENDING', ?)",
        )
        .bind(day)
        .bind(stock_trade_id)
        .bind(underlying)
        .bind(contract)
        .bind(is_call)
        .bind(qty)
        .bind(limit_price)
        .bind(client_order_id)
        .bind(venue_order_id)
        .bind(at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("Failed to record option entry")?;
        Ok(result.last_insert_rowid())
    }

    /// Alpaca accepted the entry order.
    pub async fn set_option_order_id(&self, id: i64, venue_order_id: &str) -> Result<()> {
        sqlx::query("UPDATE us_option_trades SET venue_order_id = ? WHERE id = ?")
            .bind(venue_order_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("Failed to record option order id")?;
        Ok(())
    }

    /// Stock trades that already have an option row (taken or tried).
    pub async fn option_stock_ids(&self) -> Result<std::collections::HashSet<i64>> {
        let rows: Vec<(i64,)> = sqlx::query_as("SELECT stock_trade_id FROM us_option_trades")
            .fetch_all(&self.pool)
            .await
            .context("Failed to read option rows")?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Option rows still in flight: PENDING, OPEN or CLOSING.
    pub async fn live_option_trades(&self) -> Result<Vec<OptionTrade>> {
        sqlx::query_as(
            "SELECT id, day, stock_trade_id, contract, qty, client_order_id, venue_order_id, status,
                    entry_price, exit_order_id, entry_at
             FROM us_option_trades WHERE status IN ('PENDING', 'OPEN', 'CLOSING') ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to read live option trades")
    }

    /// Entry filled: the position is open.
    pub async fn option_opened(&self, id: i64, entry_price: &str, qty: i64) -> Result<()> {
        sqlx::query(
            "UPDATE us_option_trades SET status = 'OPEN', entry_price = ?, qty = ? WHERE id = ?",
        )
        .bind(entry_price)
        .bind(qty)
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to open option trade")?;
        Ok(())
    }

    /// Entry never filled (cancelled or rejected): nothing was bought.
    pub async fn option_cancelled(&self, id: i64, reason: &str) -> Result<()> {
        sqlx::query(
            "UPDATE us_option_trades SET status = 'CANCELLED', exit_reason = ?, pnl = '0' WHERE id = ?",
        )
        .bind(reason)
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to cancel option trade")?;
        Ok(())
    }

    /// Exit order sent.
    pub async fn option_closing(
        &self,
        id: i64,
        exit_client_order_id: &str,
        exit_order_id: Option<&str>,
        reason: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE us_option_trades SET status = 'CLOSING', exit_client_order_id = ?, exit_order_id = ?,
                 exit_reason = ? WHERE id = ?",
        )
        .bind(exit_client_order_id)
        .bind(exit_order_id)
        .bind(reason)
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to mark option closing")?;
        Ok(())
    }

    /// Exit filled: book the P&L.
    pub async fn option_closed(
        &self,
        id: i64,
        exit_price: &str,
        pnl: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE us_option_trades SET status = 'CLOSED', exit_price = ?, pnl = ?, exit_at = ? WHERE id = ?",
        )
        .bind(exit_price)
        .bind(pnl)
        .bind(at.to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to close option trade")?;
        Ok(())
    }

    /// Exit order died unfilled: back to OPEN so the next pass sells again.
    pub async fn option_reopen(&self, id: i64) -> Result<()> {
        sqlx::query(
            "UPDATE us_option_trades SET status = 'OPEN', exit_order_id = NULL WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to reopen option trade")?;
        Ok(())
    }

    /// (entries today, P&L today, P&L total, losing trades only) for the book.
    pub async fn option_totals(&self, day: &str) -> Result<(i64, Decimal, Decimal, Decimal)> {
        let rows: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT day, status, pnl FROM us_option_trades")
                .fetch_all(&self.pool)
                .await
                .context("Failed to read option totals")?;
        let (mut n, mut today, mut all, mut losses) =
            (0i64, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);
        for (d, status, pnl) in rows {
            if d == day && status != "CANCELLED" {
                n += 1;
            }
            let v = pnl
                .as_deref()
                .and_then(|p| Decimal::from_str(p).ok())
                .unwrap_or_default();
            all += v;
            if v.is_sign_negative() {
                losses += v;
            }
            if d == day {
                today += v;
            }
        }
        Ok((n, today, all, losses))
    }

    // ---- U.S. credit-spread book (us::spreads) ----

    /// Record a spread entry about to be sent; returns the row id.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_spread_trade(
        &self,
        day: &str,
        underlying: &str,
        short_contract: &str,
        long_contract: &str,
        short_strike: &str,
        long_strike: &str,
        underlying_price: &str,
        qty: i64,
        limit_credit: &str,
        client_order_id: &str,
        at: DateTime<Utc>,
    ) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO us_spread_trades (day, underlying, short_contract, long_contract, short_strike,
                 long_strike, underlying_price, qty, limit_credit, client_order_id, status, entry_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'PENDING', ?)",
        )
        .bind(day)
        .bind(underlying)
        .bind(short_contract)
        .bind(long_contract)
        .bind(short_strike)
        .bind(long_strike)
        .bind(underlying_price)
        .bind(qty)
        .bind(limit_credit)
        .bind(client_order_id)
        .bind(at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("Failed to record spread trade")?;
        Ok(result.last_insert_rowid())
    }

    /// Run one fixed UPDATE on a spread row: the binds, then the id.
    async fn spread_update(&self, sql: &str, binds: &[&str], id: i64) -> Result<()> {
        let mut q = sqlx::query(sql);
        for b in binds {
            q = q.bind(*b);
        }
        q.bind(id)
            .execute(&self.pool)
            .await
            .context("Failed to update spread trade")?;
        Ok(())
    }

    pub async fn set_spread_order_id(&self, id: i64, order_id: &str) -> Result<()> {
        self.spread_update(
            "UPDATE us_spread_trades SET venue_order_id = ? WHERE id = ?",
            &[order_id],
            id,
        )
        .await
    }

    /// The entry sent again at a new credit (the mid went unfilled).
    pub async fn spread_resend(
        &self,
        id: i64,
        limit_credit: &str,
        client_order_id: &str,
        order_id: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        self.spread_update(
            "UPDATE us_spread_trades SET limit_credit = ?, client_order_id = ?, venue_order_id = ?, entry_at = ? WHERE id = ?",
            &[limit_credit, client_order_id, order_id, &at.to_rfc3339()],
            id,
        )
        .await
    }

    /// Entry filled for `credit` a share.
    pub async fn spread_opened(&self, id: i64, credit: &str) -> Result<()> {
        self.spread_update(
            "UPDATE us_spread_trades SET status = 'OPEN', entry_credit = ? WHERE id = ?",
            &[credit],
            id,
        )
        .await
    }

    /// Entry never filled, or skipped: nothing was sold.
    pub async fn spread_cancelled(&self, id: i64, reason: &str) -> Result<()> {
        self.spread_update(
            "UPDATE us_spread_trades SET status = 'CANCELLED', exit_reason = ?, pnl = '0' WHERE id = ?",
            &[reason],
            id,
        )
        .await
    }

    /// Buy-back order sent.
    pub async fn spread_closing(
        &self,
        id: i64,
        client_order_id: &str,
        order_id: &str,
        reason: &str,
    ) -> Result<()> {
        self.spread_update(
            "UPDATE us_spread_trades SET status = 'CLOSING', exit_client_order_id = ?, exit_order_id = ?,
                 exit_reason = ?, exit_tries = exit_tries + 1 WHERE id = ?",
            &[client_order_id, order_id, reason],
            id,
        )
        .await
    }

    /// Buy-back order died unfilled: open again, so the next pass retries.
    pub async fn spread_reopen(&self, id: i64) -> Result<()> {
        self.spread_update(
            "UPDATE us_spread_trades SET status = 'OPEN', exit_order_id = NULL WHERE id = ?",
            &[],
            id,
        )
        .await
    }

    /// Bought back (or settled) for `debit` a share.
    pub async fn spread_closed(
        &self,
        id: i64,
        debit: &str,
        pnl: &str,
        reason: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        self.spread_update(
            "UPDATE us_spread_trades SET status = 'CLOSED', exit_debit = ?, pnl = ?, exit_reason = ?,
                 exit_at = ? WHERE id = ?",
            &[debit, pnl, reason, &at.to_rfc3339()],
            id,
        )
        .await
    }

    /// Spreads in flight: entries sent, open, or being bought back.
    pub async fn live_spread_trades(&self) -> Result<Vec<SpreadTrade>> {
        sqlx::query_as(
            "SELECT id, day, underlying, short_contract, long_contract, short_strike, long_strike, qty,
                    client_order_id, venue_order_id, status, entry_credit, entry_at, exit_order_id, exit_tries
             FROM us_spread_trades WHERE status IN ('PENDING', 'OPEN', 'CLOSING') ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to read live spread trades")
    }

    /// Underlyings that already have a spread row today (any status).
    pub async fn spread_underlyings_on(&self, day: &str) -> Result<Vec<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT underlying FROM us_spread_trades WHERE day = ?")
                .bind(day)
                .fetch_all(&self.pool)
                .await
                .context("Failed to read today's spreads")?;
        Ok(rows.into_iter().map(|(u,)| u).collect())
    }

    /// (spreads today, P&L today, P&L in total, losses in total) in dollars.
    pub async fn spread_totals(&self, day: &str) -> Result<(i64, Decimal, Decimal, Decimal)> {
        let rows: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT day, status, pnl FROM us_spread_trades")
                .fetch_all(&self.pool)
                .await
                .context("Failed to read spread totals")?;
        let (mut n, mut today, mut all, mut losses) =
            (0i64, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);
        for (d, status, pnl) in rows {
            if d == day && status != "CANCELLED" {
                n += 1;
            }
            let v = pnl
                .as_deref()
                .and_then(|p| Decimal::from_str(p).ok())
                .unwrap_or_default();
            all += v;
            if v.is_sign_negative() {
                losses += v;
            }
            if d == day {
                today += v;
            }
        }
        Ok((n, today, all, losses))
    }

    /// The latest spread trades as JSON, newest first.
    pub async fn recent_spread_trades(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            String,
            String,
            String,
            String,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT day, underlying, short_strike, long_strike, underlying_price, qty, status,
                    entry_credit, exit_debit, exit_reason, pnl, entry_at, exit_at
             FROM us_spread_trades ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read spread trades")?;
        Ok(rows
            .into_iter()
            .map(|(day, underlying, ks, kl, px, qty, status, credit, debit, reason, pnl, entry_at, exit_at)| {
                serde_json::json!({
                    "day": day, "underlying": underlying, "short_strike": ks, "long_strike": kl,
                    "underlying_price": px, "qty": qty, "status": status, "entry_credit": credit,
                    "exit_debit": debit, "exit_reason": reason, "pnl": pnl,
                    "entry_at": entry_at, "exit_at": exit_at,
                })
            })
            .collect())
    }

    /// Every client order id the options and spread books have sent,
    /// entries and exits. The reconciler counts these as known, not as orphans.
    pub async fn option_client_order_ids(&self) -> Result<Vec<String>> {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT client_order_id, exit_client_order_id FROM us_option_trades
             UNION ALL SELECT client_order_id, exit_client_order_id FROM us_spread_trades",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to read option order ids")?;
        Ok(rows
            .into_iter()
            .flat_map(|(a, b)| std::iter::once(a).chain(b))
            .collect())
    }

    /// The latest option trades as JSON, newest first.
    pub async fn recent_option_trades(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            String,
            String,
            bool,
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT day, underlying, contract, is_call, qty, status, entry_price, exit_price,
                        exit_reason, pnl, entry_at, exit_at
                 FROM us_option_trades ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read option trades")?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    day,
                    underlying,
                    contract,
                    is_call,
                    qty,
                    status,
                    entry,
                    exit,
                    reason,
                    pnl,
                    entry_at,
                    exit_at,
                )| {
                    serde_json::json!({
                        "day": day, "underlying": underlying, "contract": contract,
                        "kind": if is_call { "CALL" } else { "PUT" }, "qty": qty, "status": status,
                        "entry_price": entry, "exit_price": exit, "exit_reason": reason, "pnl": pnl,
                        "entry_at": entry_at, "exit_at": exit_at,
                    })
                },
            )
            .collect())
    }

    /// The open crypto position of a book in one coin: (id, qty, cost).
    pub async fn crypto_open(
        &self,
        book: &str,
        symbol: &str,
    ) -> Result<Option<(i64, Decimal, Decimal)>> {
        let row: Option<(i64, String, String)> = sqlx::query_as(
            "SELECT id, qty, cost FROM crypto_trades WHERE book = ? AND symbol = ? AND status = 'OPEN'
             ORDER BY id DESC LIMIT 1",
        )
        .bind(book)
        .bind(symbol)
        .fetch_optional(&self.pool)
        .await
        .context("Failed to read open crypto trade")?;
        Ok(row.map(|(id, q, c)| {
            (
                id,
                Decimal::from_str(&q).unwrap_or_default(),
                Decimal::from_str(&c).unwrap_or_default(),
            )
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn crypto_insert_open(
        &self,
        book: &str,
        symbol: &str,
        qty: Decimal,
        cost: Decimal,
        price: Decimal,
        reason: &str,
        client_order_id: Option<&str>,
    ) -> Result<i64> {
        let r = sqlx::query(
            "INSERT INTO crypto_trades (book, symbol, status, qty, cost, entry_price, entry_at, entry_reason, client_order_id)
             VALUES (?, ?, 'OPEN', ?, ?, ?, ?, ?, ?)",
        )
        .bind(book)
        .bind(symbol)
        .bind(qty.to_string())
        .bind(cost.round_dp(6).to_string())
        .bind(price.to_string())
        .bind(Utc::now().to_rfc3339())
        .bind(reason)
        .bind(client_order_id)
        .execute(&self.pool)
        .await
        .context("Failed to record crypto entry")?;
        Ok(r.last_insert_rowid())
    }

    pub async fn crypto_close(
        &self,
        id: i64,
        price: Decimal,
        proceeds: Decimal,
        cost: Decimal,
        reason: &str,
        exit_client_order_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE crypto_trades SET status = 'CLOSED', exit_price = ?, exit_at = ?, exit_reason = ?,
                    proceeds = ?, pnl = ?, exit_client_order_id = ? WHERE id = ?",
        )
        .bind(price.to_string())
        .bind(Utc::now().to_rfc3339())
        .bind(reason)
        .bind(proceeds.round_dp(6).to_string())
        .bind((proceeds - cost).round_dp(6).to_string())
        .bind(exit_client_order_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .context("Failed to record crypto exit")?;
        Ok(())
    }

    /// (losses, profits) of a crypto book's closed trades. Losses come off the
    /// capital; profits are set aside.
    pub async fn crypto_realized(&self, book: &str) -> Result<(Decimal, Decimal)> {
        let rows: Vec<(Option<String>,)> =
            sqlx::query_as("SELECT pnl FROM crypto_trades WHERE book = ? AND status = 'CLOSED'")
                .bind(book)
                .fetch_all(&self.pool)
                .await
                .context("Failed to read crypto P&L")?;
        let (mut losses, mut profits) = (Decimal::ZERO, Decimal::ZERO);
        for (p,) in rows {
            let v = p
                .as_deref()
                .and_then(|p| Decimal::from_str(p).ok())
                .unwrap_or_default();
            if v.is_sign_negative() {
                losses += v;
            } else {
                profits += v;
            }
        }
        Ok((losses, profits))
    }

    /// Every Alpaca client order id the crypto book has sent.
    pub async fn crypto_client_order_ids(&self) -> Result<Vec<String>> {
        let rows: Vec<(Option<String>, Option<String>)> =
            sqlx::query_as("SELECT client_order_id, exit_client_order_id FROM crypto_trades")
                .fetch_all(&self.pool)
                .await
                .context("Failed to read crypto order ids")?;
        Ok(rows
            .into_iter()
            .flat_map(|(a, b)| a.into_iter().chain(b))
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_crypto_check(
        &self,
        day: &str,
        symbol: &str,
        kind: &str,
        price: Decimal,
        chg: (Option<Decimal>, Option<Decimal>, Option<Decimal>),
        rule_says: &str,
        news_risk: &str,
        news_reason: &str,
        market_view: &str,
        headlines: &str,
        action: &str,
    ) -> Result<()> {
        let pct =
            |x: Option<Decimal>| x.map(|v| (v * Decimal::ONE_HUNDRED).round_dp(2).to_string());
        sqlx::query(
            "INSERT INTO crypto_checks (at, day, symbol, kind, price, chg_24h, chg_7d, chg_60d, rule_says,
                                        news_risk, news_reason, market_view, headlines, action)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(day)
        .bind(symbol)
        .bind(kind)
        .bind(price.to_string())
        .bind(pct(chg.0))
        .bind(pct(chg.1))
        .bind(pct(chg.2))
        .bind(rule_says)
        .bind(news_risk)
        .bind(news_reason)
        .bind(market_view)
        .bind(headlines)
        .bind(action)
        .execute(&self.pool)
        .await
        .context("Failed to record crypto check")?;
        Ok(())
    }

    /// When the last check of this kind ran for a coin (RFC 3339), if ever.
    pub async fn crypto_last_check(
        &self,
        symbol: &str,
        kind: &str,
    ) -> Result<Option<(String, String)>> {
        sqlx::query_as("SELECT day, at FROM crypto_checks WHERE symbol = ? AND kind = ? ORDER BY id DESC LIMIT 1")
            .bind(symbol)
            .bind(kind)
            .fetch_optional(&self.pool)
            .await
            .context("Failed to read last crypto check")
    }

    /// Crypto trades and checks as JSON, newest first.
    pub async fn crypto_recent(
        &self,
        limit: i64,
    ) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>)> {
        #[allow(clippy::type_complexity)]
        let trades: Vec<(
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT book, symbol, status, qty, cost, entry_price, entry_at, entry_reason,
                        exit_price, exit_at, exit_reason, pnl
                 FROM crypto_trades ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read crypto trades")?;
        #[allow(clippy::type_complexity)]
        let checks: Vec<(
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        )> = sqlx::query_as(
            "SELECT at, symbol, kind, price, chg_24h, chg_7d, chg_60d, rule_says, news_risk,
                        news_reason, market_view, headlines, action
                 FROM crypto_checks ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read crypto checks")?;
        Ok((
            trades
                .into_iter()
                .map(|(book, symbol, status, qty, cost, entry, entry_at, why, exit, exit_at, exit_why, pnl)| {
                    serde_json::json!({
                        "book": book, "symbol": symbol, "status": status, "qty": qty, "cost": cost,
                        "entry_price": entry, "entry_at": entry_at, "entry_reason": why,
                        "exit_price": exit, "exit_at": exit_at, "exit_reason": exit_why, "pnl": pnl,
                    })
                })
                .collect(),
            checks
                .into_iter()
                .map(|(at, symbol, kind, price, c24, c7, c60, rule, risk, reason, view, heads, action)| {
                    serde_json::json!({
                        "at": at, "symbol": symbol, "kind": kind, "price": price,
                        "chg_24h_pct": c24, "chg_7d_pct": c7, "chg_60d_pct": c60,
                        "rule_says": rule, "news_risk": risk, "news_reason": reason,
                        "market_view": view, "headlines": heads, "action": action,
                    })
                })
                .collect(),
        ))
    }

    /// The latest intraday trades as JSON, newest first.
    pub async fn recent_intraday_trades(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(String, String, bool, i64, String, String, String, Option<String>, Option<String>, Option<String>, Option<String>, String)> = sqlx::query_as(
            "SELECT day, symbol, is_buy, qty, entry_price, stop_price, entry_at, exit_price, exit_reason, charges, pnl, strategy
             FROM india_intraday_trades WHERE strategy NOT GLOB '*_cap[0-9]*' ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read intraday trades")?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "day": r.0, "symbol": r.1, "side": if r.2 { "LONG" } else { "SHORT" }, "qty": r.3,
                    "entry_price": r.4, "stop_price": r.5, "entry_at": r.6, "exit_price": r.7,
                    "exit_reason": r.8, "charges": r.9, "pnl": r.10, "strategy": r.11,
                })
            })
            .collect())
    }

    /// The Polymarket paper book from the ledger: (cost of open positions,
    /// realized P&L of settled or exited ones). Polymarket trades are the
    /// YES / NO ones; every other venue records BUY / SELL.
    pub async fn polymarket_book(&self) -> Result<(Decimal, Decimal)> {
        let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT status, entry_price, size, pnl FROM trades WHERE direction IN ('YES', 'NO')",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to read the Polymarket book")?;
        let (mut open_cost, mut realized) = (Decimal::ZERO, Decimal::ZERO);
        for (status, price, size, pnl) in rows {
            if status == "OPEN" {
                if let (Ok(p), Ok(s)) = (Decimal::from_str(&price), Decimal::from_str(&size)) {
                    open_cost += p * s;
                }
            } else if let Some(v) = pnl.as_deref().and_then(|v| Decimal::from_str(v).ok()) {
                realized += v;
            }
        }
        Ok((open_cost, realized))
    }

    /// Polymarket trades, newest first, for the dashboard.
    pub async fn recent_polymarket_trades(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, String, Option<String>, String, String, String, Option<String>, String, Option<String>, Option<String>, Option<String>)> =
            sqlx::query_as(
                "SELECT id, market_id, market_question, direction, entry_price, size, claude_fair_value,
                        status, pnl, created_at, resolved_at
                 FROM trades WHERE direction IN ('YES', 'NO') ORDER BY id DESC LIMIT ?",
            )
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .context("Failed to read Polymarket trades")?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.0, "market_id": r.1, "question": r.2, "side": r.3, "entry_price": r.4,
                    "size": r.5, "fair_value": r.6, "status": r.7, "pnl": r.8, "entry_at": r.9,
                    "closed_at": r.10,
                })
            })
            .collect())
    }

    /// Closed NSE intraday trades (any book) that have no post-mortem yet.
    pub async fn unreviewed_intraday_trades(&self) -> Result<Vec<ClosedTrade>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, String, String, bool, String, String, Option<String>, String, Option<String>, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT t.id, t.strategy, t.symbol, t.is_buy, t.entry_price, t.stop_price, t.exit_price,
                    t.entry_at, t.exit_at, t.exit_reason, t.pnl, t.charges
             FROM india_intraday_trades t
             LEFT JOIN trade_reviews r
               ON r.market = 'IN' AND r.strategy = t.strategy AND r.trade_ref = t.id
             WHERE t.exit_at IS NOT NULL AND r.id IS NULL AND t.strategy NOT GLOB '*_cap[0-9]*'
             ORDER BY t.id",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to read unreviewed intraday trades")?;
        let dec = |s: &str| Decimal::from_str(s).ok();
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some(ClosedTrade {
                    market: "IN".into(),
                    strategy: r.1,
                    trade_ref: r.0,
                    symbol: r.2,
                    is_buy: r.3,
                    entry_price: dec(&r.4)?,
                    stop_price: dec(&r.5)?,
                    exit_price: dec(r.6.as_deref()?)?,
                    entry_at: parse_db_time(&r.7)?,
                    exit_at: parse_db_time(r.8.as_deref()?)?,
                    exit_reason: r.9,
                    pnl: dec(r.10.as_deref()?)?,
                    charges: r.11.as_deref().and_then(dec),
                })
            })
            .collect())
    }

    /// One strategy's closed trades on one venue that have no post-mortem yet.
    /// A closed trade's mark is its exit price.
    pub async fn unreviewed_venue_trades(
        &self,
        venue_id: &str,
        strategy: &str,
    ) -> Result<Vec<ClosedTrade>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT t.id, t.symbol, t.side, COALESCE(t.avg_fill_price, t.entry_price), t.stop_price,
                    t.mark_price, t.created_at, t.closed_at, t.close_reason, t.realized_pnl
             FROM trades t
             LEFT JOIN trade_reviews r
               ON r.market = 'US' AND r.strategy = t.strategy AND r.trade_ref = t.id
             WHERE t.venue_id = ? AND t.strategy = ? AND t.status = 'CLOSED' AND r.id IS NULL
             ORDER BY t.id",
        )
        .bind(venue_id)
        .bind(strategy)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read unreviewed venue trades")?;
        let dec = |s: Option<String>| s.as_deref().and_then(|v| Decimal::from_str(v).ok());
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some(ClosedTrade {
                    market: "US".into(),
                    strategy: strategy.to_string(),
                    trade_ref: r.0,
                    symbol: r.1?,
                    is_buy: r.2.as_deref() != Some("SELL"),
                    entry_price: dec(r.3)?,
                    stop_price: dec(r.4)?,
                    exit_price: dec(r.5)?,
                    entry_at: parse_db_time(r.6.as_deref()?)?,
                    exit_at: parse_db_time(r.7.as_deref()?)?,
                    exit_reason: r.8,
                    pnl: dec(r.9)?,
                    charges: None,
                })
            })
            .collect())
    }

    /// File a trade's post-mortem. A second review of the same trade is ignored.
    pub async fn insert_trade_review(
        &self,
        t: &ClosedTrade,
        r: &TradeReviewRow,
        at: DateTime<Utc>,
    ) -> Result<()> {
        let s = |d: Option<Decimal>| d.map(|v| v.to_string());
        sqlx::query(
            "INSERT OR IGNORE INTO trade_reviews
               (market, strategy, trade_ref, symbol, side, day, entry_at, exit_at, entry_price,
                stop_price, exit_price, exit_reason, pnl, charges, minutes_held, mfe_r, mae_r,
                index_move_pct, label, lesson, reviewed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&t.market)
        .bind(&t.strategy)
        .bind(t.trade_ref)
        .bind(&t.symbol)
        .bind(if t.is_buy { "LONG" } else { "SHORT" })
        .bind(&r.day)
        .bind(t.entry_at.to_rfc3339())
        .bind(t.exit_at.to_rfc3339())
        .bind(t.entry_price.to_string())
        .bind(t.stop_price.to_string())
        .bind(t.exit_price.to_string())
        .bind(&t.exit_reason)
        .bind(t.pnl.to_string())
        .bind(s(t.charges))
        .bind(r.minutes_held)
        .bind(s(r.mfe_r))
        .bind(s(r.mae_r))
        .bind(s(r.index_move_pct))
        .bind(&r.label)
        .bind(&r.lesson)
        .bind(at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("Failed to store trade review")?;
        Ok(())
    }

    /// The latest post-mortems as JSON, newest trade first.
    pub async fn recent_trade_reviews(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        let rows: Vec<StoredReview> = sqlx::query_as(
            "SELECT market, strategy, trade_ref, symbol, side, day, entry_at, exit_at, entry_price,
                    stop_price, exit_price, exit_reason, pnl, charges, minutes_held, mfe_r, mae_r,
                    index_move_pct, label, lesson
             FROM trade_reviews ORDER BY entry_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read trade reviews")?;
        Ok(rows
            .into_iter()
            .map(|r| serde_json::to_value(r).unwrap_or_default())
            .collect())
    }

    /// One strategy's trades on one venue, newest first, for the dashboard. A
    /// closed trade's mark is its exit price.
    pub async fn recent_strategy_trades(
        &self,
        venue_id: &str,
        strategy: &str,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT id, symbol, side, quantity, COALESCE(avg_fill_price, entry_price), stop_price, mark_price,
                    realized_pnl, unrealized_pnl, close_reason, status, created_at, closed_at
             FROM trades WHERE venue_id = ? AND strategy = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(venue_id)
        .bind(strategy)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read strategy trades")?;
        Ok(rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.0, "symbol": r.1, "side": r.2, "qty": r.3, "entry_price": r.4,
                    "stop_price": r.5, "mark_price": r.6, "realized_pnl": r.7, "unrealized_pnl": r.8,
                    "close_reason": r.9, "status": r.10, "entry_at": r.11, "closed_at": r.12,
                    "strategy": strategy,
                })
            })
            .collect())
    }

    /// Record one trading-desk decision card (the full card as JSON).
    pub async fn insert_desk_decision(&self, card: &crate::us::desk::DecisionCard) -> Result<i64> {
        let action = serde_json::to_value(card.nova.action)?;
        let advisory = serde_json::to_value(card.nova.advisory_action)?;
        self.insert_desk_card(
            card.at,
            &card.market,
            &card.strategy,
            &card.symbol,
            action.as_str().unwrap_or("UNKNOWN"),
            advisory.as_str().unwrap_or("UNKNOWN"),
            &serde_json::to_value(card)?,
        )
        .await
    }

    /// Record any market's decision card.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_desk_card(
        &self,
        at: DateTime<Utc>,
        market: &str,
        strategy: &str,
        symbol: &str,
        action: &str,
        advisory_action: &str,
        card: &serde_json::Value,
    ) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO desk_decisions (created_at, market, strategy, symbol, action, advisory_action, card)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(at.to_rfc3339())
        .bind(market)
        .bind(strategy)
        .bind(symbol)
        .bind(action)
        .bind(advisory_action)
        .bind(card.to_string())
        .execute(&self.pool)
        .await
        .context("Failed to record desk decision")?;
        Ok(result.last_insert_rowid())
    }

    /// One market's most recent decision cards, newest first, as stored JSON.
    pub async fn recent_desk_decisions(
        &self,
        market: &str,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT card FROM desk_decisions WHERE market = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(market)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read desk decisions")?;
        Ok(rows
            .into_iter()
            .filter_map(|(c,)| serde_json::from_str(&c).ok())
            .collect())
    }

    /// Today's P&L for one strategy's venue trades: realized on positions
    /// closed today plus the latest mark on those still open.
    /// Realized P&L of these strategies' trades closed on or after `since`
    /// (UTC date), or ever when `since` is None.
    pub async fn strategies_realized_since(
        &self,
        strategies: &[&str],
        since: Option<chrono::NaiveDate>,
    ) -> Result<Decimal> {
        let mut total = Decimal::ZERO;
        for strategy in strategies {
            let rows: Vec<(Option<String>,)> = sqlx::query_as(
                "SELECT realized_pnl FROM trades
                 WHERE strategy = ? AND status = 'CLOSED' AND substr(closed_at, 1, 10) >= ?",
            )
            .bind(strategy)
            .bind(since.map(|d| d.to_string()).unwrap_or_default())
            .fetch_all(&self.pool)
            .await
            .context("Failed to read strategy realized P&L")?;
            for (v,) in rows {
                if let Some(v) = v.as_deref().and_then(|v| Decimal::from_str(v).ok()) {
                    total += v;
                }
            }
        }
        Ok(total)
    }

    /// Today's P&L of every equity trade on a venue: closed today (realized)
    /// plus still open (marked). The U.S. stock books' figure for the
    /// circuit breaker; options and crypto live in tables of their own.
    pub async fn venue_equity_day_pnl(
        &self,
        venue: &str,
        day: chrono::NaiveDate,
    ) -> Result<Decimal> {
        let rows: Vec<(Option<String>, Option<String>, String)> = sqlx::query_as(
            "SELECT realized_pnl, unrealized_pnl, status FROM trades
             WHERE venue_id = ? AND asset_class = 'equity'
               AND (date(closed_at) = ? OR status IN ('OPEN', 'PARTIAL'))",
        )
        .bind(venue)
        .bind(day.to_string())
        .fetch_all(&self.pool)
        .await
        .context("Failed to read the venue's day P&L")?;
        let mut total = Decimal::ZERO;
        for (realized, unrealized, status) in rows {
            let v = if status == "CLOSED" {
                realized
            } else {
                unrealized
            };
            if let Some(v) = v.as_deref().and_then(|v| Decimal::from_str(v).ok()) {
                total += v;
            }
        }
        Ok(total)
    }

    pub async fn strategy_day_pnl(
        &self,
        strategy: &str,
        day: chrono::NaiveDate,
    ) -> Result<Decimal> {
        let rows: Vec<(Option<String>, Option<String>, String)> = sqlx::query_as(
            "SELECT realized_pnl, unrealized_pnl, status FROM trades
             WHERE strategy = ? AND (date(closed_at) = ? OR status IN ('OPEN', 'PARTIAL'))",
        )
        .bind(strategy)
        .bind(day.to_string())
        .fetch_all(&self.pool)
        .await
        .context("Failed to read strategy day P&L")?;
        let mut total = Decimal::ZERO;
        for (realized, unrealized, status) in rows {
            let v = if status == "CLOSED" {
                realized
            } else {
                unrealized
            };
            if let Some(v) = v.as_deref().and_then(|v| Decimal::from_str(v).ok()) {
                total += v;
            }
        }
        Ok(total)
    }

    /// Symbols a strategy already entered today, open or closed: one attempt
    /// per symbol per day.
    pub async fn strategy_symbols_entered_on(
        &self,
        strategy: &str,
        day: chrono::NaiveDate,
    ) -> Result<Vec<String>> {
        sqlx::query_scalar(
            "SELECT DISTINCT symbol FROM trades WHERE strategy = ? AND date(created_at) = ?",
        )
        .bind(strategy)
        .bind(day.to_string())
        .fetch_all(&self.pool)
        .await
        .context("Failed to read strategy symbols")
    }

    /// How many of the most recently closed positions were losses, counting
    /// back from the latest until a non-loss is reached.
    ///
    /// Ordered by when each position *closed*, not when it opened: a streak
    /// is about the order the results arrived in, and a long-held winner
    /// opened before three quick losers does not break them up.
    ///
    /// Both close columns are consulted, because two paths write two of them:
    /// venue exits set `closed_at` and only the legacy prediction resolution
    /// sets `resolved_at`. Falling back to `created_at` alone sorted by entry
    /// time, so the streak read zero during an actual losing run.
    pub async fn consecutive_losses(&self) -> Result<u32> {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            // Polymarket paper bets are left out, as in count_trades_opened_on:
            // on 11 Nov 2026 its run of lost underdog bets tripped this limit
            // and halted the stock books for the U.S. session.
            "SELECT status, pnl FROM trades
             WHERE status IN ('CLOSED', 'RESOLVED_WIN', 'RESOLVED_LOSS') AND venue_id <> 'polymarket'
             ORDER BY COALESCE(resolved_at, closed_at, created_at) DESC, id DESC
             LIMIT 50",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to read recent closes")?;

        let mut streak = 0u32;
        for (status, pnl) in rows {
            let lost = match status.as_str() {
                "RESOLVED_LOSS" => true,
                "RESOLVED_WIN" => false,
                // A continuous position has no notion of winning; its P&L
                // decides. A close with no P&L recorded is not evidence of a
                // loss, so it ends the streak rather than extending it.
                _ => match pnl.as_deref() {
                    Some(v) => parse_money(v, "trades.pnl")? < Decimal::ZERO,
                    None => false,
                },
            };
            if !lost {
                break;
            }
            streak += 1;
        }
        Ok(streak)
    }

    /// Record one venue's reconciliation result.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_reconciliation_run(
        &self,
        venue_id: &str,
        cycle: i64,
        equity: Option<Decimal>,
        positions_missing_locally: i64,
        positions_missing_on_venue: i64,
        qty_mismatches: i64,
        unknown_open_orders: i64,
        passed: bool,
        detail: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO reconciliation_runs
                 (venue_id, cycle, balance_delta, positions_missing_locally,
                  positions_missing_on_venue, qty_mismatches, unknown_open_orders,
                  passed, detail)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(venue_id)
        .bind(cycle)
        // The column predates this pass and was specified for a cash
        // comparison that is not implemented; the venue's reported equity is
        // what there is, and recording it gives the audit trail a number.
        .bind(equity.map(|e| e.to_string()))
        .bind(positions_missing_locally)
        .bind(positions_missing_on_venue)
        .bind(qty_mismatches)
        .bind(unknown_open_orders)
        .bind(passed)
        .bind(detail)
        .execute(&self.pool)
        .await
        .context("Failed to record reconciliation run")?;
        Ok(())
    }

    /// Persist a halt so that restarting the process does not lift it.
    pub async fn insert_halt(
        &self,
        source: &str,
        scope: &str,
        detail: &str,
        raised_at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            // OR IGNORE, because the same halt is re-offered on every restart
            // while it is in force. See the UNIQUE constraint in 004.
            "INSERT OR IGNORE INTO halts (source, scope, detail, raised_at, day)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(source)
        .bind(scope)
        .bind(detail)
        .bind(raised_at.to_rfc3339())
        .bind(raised_at.date_naive().to_string())
        .execute(&self.pool)
        .await
        .context("Failed to record halt")?;
        Ok(())
    }

    /// Mark every in-force halt as cleared.
    pub async fn clear_halts(&self, by: &str, at: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE halts SET cleared_at = ?, cleared_by = ? WHERE cleared_at IS NULL")
            .bind(at.to_rfc3339())
            .bind(by)
            .execute(&self.pool)
            .await
            .context("Failed to clear halts")?;
        Ok(())
    }

    /// Orders, newest first. The execution record: what was asked for, what
    /// came back, and what it actually filled at.
    pub async fn get_orders(&self, limit: i64) -> Result<Vec<OrderRecord>> {
        sqlx::query_as::<_, OrderRecord>(
            "SELECT id, client_order_id, venue_order_id, venue_id, symbol, side, intent,
                    trade_id, limit_price, qty, filled_qty, avg_fill_price, state,
                    reject_reason, cycle, mid_at_submit, submitted_at, updated_at, expires_at
             FROM orders ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch orders")
    }

    /// Individual executions, newest first, with the order they belong to.
    ///
    /// Slippage and time-to-fill live here rather than on `orders` because an
    /// order can fill in several pieces at several prices, and averaging them
    /// away is how "median slippage 10bps" stops meaning anything.
    pub async fn get_fills(&self, limit: i64) -> Result<Vec<FillRecord>> {
        sqlx::query_as::<_, FillRecord>(
            "SELECT f.id, f.order_id, o.client_order_id, o.venue_id, o.symbol, o.side,
                    o.intent, f.qty, f.price, f.fee, f.mid_at_submit, f.slippage_bps,
                    f.time_to_fill_ms, f.filled_at
             FROM fills f JOIN orders o ON o.id = f.order_id
             ORDER BY f.id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch fills")
    }

    /// One row per UTC day: where equity opened, its running peak, where it
    /// closed. The series the drawdown breaker reads.
    pub async fn get_daily_equity(&self) -> Result<Vec<DailyEquityRecord>> {
        sqlx::query_as::<_, DailyEquityRecord>(
            "SELECT day, starting_equity, high_water_mark, closing_equity, realized_pnl, updated_at
             FROM daily_equity ORDER BY day ASC",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch daily equity")
    }

    /// Closed P&L per day of the real India books (shadow and tracking ledgers
    /// left out), in rupees, from `since` on. For the daily target tracker.
    pub async fn india_real_pnl_by_day(
        &self,
        since: &str,
    ) -> Result<std::collections::BTreeMap<String, Decimal>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT day, pnl FROM india_intraday_trades
             WHERE pnl IS NOT NULL AND day >= ?
               AND strategy IN ('india_orb', 'india_burst', 'india_gapgo')",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read India P&L by day")?;
        let mut out = std::collections::BTreeMap::new();
        for (day, pnl) in rows {
            if let Ok(v) = Decimal::from_str(&pnl) {
                *out.entry(day).or_insert(Decimal::ZERO) += v;
            }
        }
        Ok(out)
    }

    /// Closed Polymarket P&L per UTC day from `since` on, in dollars.
    pub async fn polymarket_pnl_by_day(
        &self,
        since: &str,
    ) -> Result<std::collections::BTreeMap<String, Decimal>> {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT date(closed_at), realized_pnl FROM trades
             WHERE asset_class = 'prediction_binary' AND status = 'CLOSED'
               AND closed_at IS NOT NULL AND date(closed_at) >= ?",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .context("Failed to read Polymarket P&L by day")?;
        let mut out = std::collections::BTreeMap::new();
        for (day, pnl) in rows {
            if let Some(v) = pnl.as_deref().and_then(|p| Decimal::from_str(p).ok()) {
                *out.entry(day).or_insert(Decimal::ZERO) += v;
            }
        }
        Ok(out)
    }

    /// Reconciliation history, newest first.
    pub async fn get_reconciliation_runs(
        &self,
        limit: i64,
    ) -> Result<Vec<ReconciliationRunRecord>> {
        sqlx::query_as::<_, ReconciliationRunRecord>(
            "SELECT id, venue_id, cycle, balance_delta, positions_missing_locally,
                    positions_missing_on_venue, qty_mismatches, unknown_open_orders,
                    passed, detail, created_at
             FROM reconciliation_runs ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch reconciliation runs")
    }

    /// Row id for a client order id, for callers holding only the latter.
    pub async fn order_id(&self, client_order_id: &str) -> Result<Option<i64>> {
        sqlx::query_scalar("SELECT id FROM orders WHERE client_order_id = ?")
            .bind(client_order_id)
            .fetch_optional(&self.pool)
            .await
            .context("Failed to look up order id")
    }

    /// Record the *new* part of a fill, given what was already recorded.
    ///
    /// The venue reports cumulative filled quantity and a cumulative average
    /// price. Every path that learns about a fill — the submit
    /// acknowledgement and the reconciliation pass alike — sees the running
    /// total, so recording it verbatim writes one row per observation rather
    /// than one per execution.
    ///
    /// Returns whether anything was written.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_fill_increment(
        &self,
        order_id: i64,
        venue_trade_id: Option<&str>,
        cumulative_qty: Decimal,
        cumulative_avg: Decimal,
        prev_qty: Decimal,
        prev_avg: Option<Decimal>,
        fee: Option<Decimal>,
        mid_at_submit: Option<Decimal>,
        side: &str,
        submitted_at: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let delta_qty = cumulative_qty - prev_qty;
        if delta_qty <= Decimal::ZERO {
            return Ok(false);
        }

        // Back the increment's own VWAP out of the two running averages.
        //
        // Guarded, because the back-out amplifies the venue's rounding of its
        // cumulative average by `cumulative_qty / delta_qty`: a 0.99-filled
        // order finishing its last 0.01 multiplies any wobble a hundredfold,
        // and can produce zero or a negative price. A fill recorded at zero
        // would enter the slippage series as -10000 bps — a spectacular price
        // improvement that never happened, dragging the median with it.
        let price = match prev_avg {
            Some(prev) if prev_qty > Decimal::ZERO => {
                let implied = (cumulative_avg * cumulative_qty - prev * prev_qty) / delta_qty;
                if implied > Decimal::ZERO {
                    implied
                } else {
                    // Implausible. The cumulative average is the honest
                    // fallback: less precise, but never a fiction.
                    cumulative_avg
                }
            }
            _ => cumulative_avg,
        };

        self.record_fill(
            order_id,
            venue_trade_id,
            delta_qty,
            price,
            fee,
            mid_at_submit,
            side,
            submitted_at,
            at,
        )
        .await?;
        Ok(true)
    }

    /// Record one execution.
    ///
    /// Slippage is signed so that **positive always means it cost us**: a buy
    /// above the mid and a sell below it both read positive, and price
    /// improvement reads negative. Without that convention a median over
    /// mixed sides cancels itself out and reports excellent execution on a
    /// book that is bleeding on both.
    ///
    /// `mid_at_submit` is `None` for orders placed before it was recorded,
    /// and those fills simply carry no slippage — which the dashboard shows
    /// as unmeasured rather than as zero.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_fill(
        &self,
        order_id: i64,
        venue_trade_id: Option<&str>,
        qty: Decimal,
        price: Decimal,
        fee: Option<Decimal>,
        mid_at_submit: Option<Decimal>,
        side: &str,
        submitted_at: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<()> {
        let slippage_bps = mid_at_submit.and_then(|mid| {
            if mid <= Decimal::ZERO {
                return None;
            }
            let raw = (price - mid) / mid * Decimal::from(10_000);
            // A sell fills *below* the mid to our cost, so flip the sign.
            Some(if side.eq_ignore_ascii_case("SELL") {
                -raw
            } else {
                raw
            })
        });

        // Measured from submission to *this observation*, not to the moment
        // the venue filled. `OrderAck` carries no fill timestamp, so for a
        // fill discovered by reconciliation this is quantised to the cycle
        // interval — an order filled two seconds after submission and seen
        // ten minutes later reads as ten minutes. It is an upper bound on
        // latency and a useful one for spotting orders that rest, but it is
        // not the venue's fill latency, and the ≤ thresholds in the promotion
        // criteria should be read with that in mind. Plumbing Alpaca's
        // `filled_at` through `OrderAck` is what would make it exact.
        let time_to_fill_ms = submitted_at
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .or_else(|| {
                // SQLite's `datetime('now')` default is zone-less UTC.
                submitted_at.and_then(|s| {
                    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                        .ok()
                        .map(|n| n.and_utc().into())
                })
            })
            .map(|t| (at - t.with_timezone(&Utc)).num_milliseconds())
            .filter(|ms| *ms >= 0);

        sqlx::query(
            "INSERT INTO fills (order_id, venue_trade_id, qty, price, fee, mid_at_submit,
                                slippage_bps, time_to_fill_ms, filled_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(order_id)
        .bind(venue_trade_id)
        .bind(qty.to_string())
        .bind(price.to_string())
        .bind(fee.map(|f| f.to_string()))
        .bind(mid_at_submit.map(|m| m.to_string()))
        .bind(slippage_bps.map(|s| s.round_dp(2).to_string()))
        .bind(time_to_fill_ms)
        .bind(at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("Failed to record fill")?;
        Ok(())
    }

    /// Marked profit and loss across every open position.
    ///
    /// Written by the mark pass each cycle. Needed because cost basis alone
    /// does not move when a position loses: buying $10 of something takes $10
    /// out of cash and adds $10 of exposure, and if it then halves, neither
    /// number changes. Account equity is cash + cost basis + this.
    ///
    /// A position the mark pass has not reached yet contributes nothing,
    /// which is the honest answer — not a guess at what it might be worth.
    pub async fn total_unrealized_pnl(&self) -> Result<Decimal> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT unrealized_pnl FROM trades
             WHERE status IN ('OPEN', 'PARTIAL') AND unrealized_pnl IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to read unrealized P&L")?;
        sum_money(&rows, "trades.unrealized_pnl")
    }

    /// Record one halt however many times it is offered.
    #[cfg(test)]
    pub async fn halt_count(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM halts")
            .fetch_one(&self.pool)
            .await?)
    }

    /// The halt still in force, if any — the newest uncleared row.
    pub async fn active_halt(&self) -> Result<Option<StoredHalt>> {
        let row: Option<StoredHalt> = sqlx::query_as(
            "SELECT source, scope, detail, raised_at, day FROM halts
             WHERE cleared_at IS NULL
             ORDER BY raised_at DESC, id DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .context("Failed to read the active halt")?;
        Ok(row)
    }

    /// Save an Indian market backtest run.
    pub async fn save_india_backtest(
        &self,
        id: &str,
        name: &str,
        config_json: &str,
        results_json: &str,
        status: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO india_backtests (id, name, created_at, config_json, results_json, status)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(now)
        .bind(config_json)
        .bind(results_json)
        .bind(status)
        .execute(&self.pool)
        .await
        .context("Failed to save India backtest record")?;
        Ok(())
    }

    /// Get recent Indian market backtests.
    pub async fn get_india_backtests(&self, limit: i64) -> Result<Vec<IndiaBacktestRecord>> {
        let rows = sqlx::query_as::<_, IndiaBacktestRecord>(
            "SELECT id, name, created_at, config_json, results_json, status
             FROM india_backtests
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to load India backtests")?;
        Ok(rows)
    }

    /// Get a specific Indian market backtest by ID.
    pub async fn get_india_backtest_by_id(&self, id: &str) -> Result<Option<IndiaBacktestRecord>> {
        let row = sqlx::query_as::<_, IndiaBacktestRecord>(
            "SELECT id, name, created_at, config_json, results_json, status
             FROM india_backtests
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("Failed to load India backtest by id")?;
        Ok(row)
    }

    /// Save a US backtest run to database.
    pub async fn save_us_backtest(
        &self,
        id: &str,
        name: &str,
        config_json: &str,
        results_json: &str,
        status: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO us_backtests (id, name, created_at, config_json, results_json, status)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(now)
        .bind(config_json)
        .bind(results_json)
        .bind(status)
        .execute(&self.pool)
        .await
        .context("Failed to save US backtest record")?;
        Ok(())
    }

    /// Get recent US market backtests.
    pub async fn get_us_backtests(&self, limit: i64) -> Result<Vec<UsBacktestRecord>> {
        let rows = sqlx::query_as::<_, UsBacktestRecord>(
            "SELECT id, name, created_at, config_json, results_json, status
             FROM us_backtests
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to load US backtests")?;
        Ok(rows)
    }

    /// Get a specific US market backtest by ID.
    pub async fn get_us_backtest_by_id(&self, id: &str) -> Result<Option<UsBacktestRecord>> {
        let row = sqlx::query_as::<_, UsBacktestRecord>(
            "SELECT id, name, created_at, config_json, results_json, status
             FROM us_backtests
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("Failed to load US backtest by id")?;
        Ok(row)
    }

    /// Upsert an Indian sandbox order record.
    pub async fn save_india_sandbox_order(&self, order: &IndiaSandboxOrderRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO india_sandbox_orders (
                id, client_order_id, venue_order_id, symbol, side, order_type,
                quantity, price, trigger_price, status, fill_quantity, average_fill_price,
                rejection_reason, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(client_order_id) DO UPDATE SET
                venue_order_id = excluded.venue_order_id,
                status = excluded.status,
                fill_quantity = excluded.fill_quantity,
                average_fill_price = excluded.average_fill_price,
                rejection_reason = excluded.rejection_reason,
                updated_at = excluded.updated_at",
        )
        .bind(&order.id)
        .bind(&order.client_order_id)
        .bind(&order.venue_order_id)
        .bind(&order.symbol)
        .bind(&order.side)
        .bind(&order.order_type)
        .bind(order.quantity)
        .bind(&order.price)
        .bind(&order.trigger_price)
        .bind(&order.status)
        .bind(order.fill_quantity)
        .bind(&order.average_fill_price)
        .bind(&order.rejection_reason)
        .bind(&order.created_at)
        .bind(&order.updated_at)
        .execute(&self.pool)
        .await
        .context("Failed to upsert India sandbox order")?;
        Ok(())
    }

    /// Get recent Indian sandbox orders.
    pub async fn get_india_sandbox_orders(
        &self,
        limit: i64,
    ) -> Result<Vec<IndiaSandboxOrderRecord>> {
        let rows = sqlx::query_as::<_, IndiaSandboxOrderRecord>(
            "SELECT id, client_order_id, venue_order_id, symbol, side, order_type,
                    quantity, price, trigger_price, status, fill_quantity, average_fill_price,
                    rejection_reason, created_at, updated_at
             FROM india_sandbox_orders
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to load India sandbox orders")?;
        Ok(rows)
    }

    /// Upsert a US paper order record.
    pub async fn save_us_paper_order(&self, order: &UsPaperOrderRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO us_paper_orders (
                id, client_order_id, venue_order_id, symbol, side, order_type,
                quantity, price, trigger_price, status, fill_quantity, average_fill_price,
                rejection_reason, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(client_order_id) DO UPDATE SET
                venue_order_id = excluded.venue_order_id,
                status = excluded.status,
                fill_quantity = excluded.fill_quantity,
                average_fill_price = excluded.average_fill_price,
                rejection_reason = excluded.rejection_reason,
                updated_at = excluded.updated_at",
        )
        .bind(&order.id)
        .bind(&order.client_order_id)
        .bind(&order.venue_order_id)
        .bind(&order.symbol)
        .bind(&order.side)
        .bind(&order.order_type)
        .bind(order.quantity)
        .bind(&order.price)
        .bind(&order.trigger_price)
        .bind(&order.status)
        .bind(order.fill_quantity)
        .bind(&order.average_fill_price)
        .bind(&order.rejection_reason)
        .bind(&order.created_at)
        .bind(&order.updated_at)
        .execute(&self.pool)
        .await
        .context("Failed to upsert US paper order")?;
        Ok(())
    }

    /// Get recent US paper orders.
    pub async fn get_us_paper_orders(&self, limit: i64) -> Result<Vec<UsPaperOrderRecord>> {
        let rows = sqlx::query_as::<_, UsPaperOrderRecord>(
            "SELECT id, client_order_id, venue_order_id, symbol, side, order_type,
                    quantity, price, trigger_price, status, fill_quantity, average_fill_price,
                    rejection_reason, created_at, updated_at
             FROM us_paper_orders
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to load US paper orders")?;
        Ok(rows)
    }

    /// Clear all US paper orders to reset the market to initial $5.00 capital.
    pub async fn clear_us_paper_orders(&self) -> Result<()> {
        sqlx::query("DELETE FROM us_paper_orders")
            .execute(&self.pool)
            .await
            .context("Failed to clear US paper orders")?;
        Ok(())
    }

    /// Clear all India sandbox orders to reset the market to initial ₹1,000 capital.
    pub async fn clear_india_sandbox_orders(&self) -> Result<()> {
        sqlx::query("DELETE FROM india_sandbox_orders")
            .execute(&self.pool)
            .await
            .context("Failed to clear India sandbox orders")?;
        Ok(())
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IndiaBacktestRecord {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub config_json: String,
    pub results_json: String,
    pub status: String,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct UsBacktestRecord {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub config_json: String,
    pub results_json: String,
    pub status: String,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct IndiaSandboxOrderRecord {
    pub id: String,
    pub client_order_id: String,
    pub venue_order_id: Option<String>,
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub quantity: i64,
    pub price: String,
    pub trigger_price: Option<String>,
    pub status: String,
    pub fill_quantity: i64,
    pub average_fill_price: Option<String>,
    pub rejection_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct UsPaperOrderRecord {
    pub id: String,
    pub client_order_id: String,
    pub venue_order_id: Option<String>,
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub quantity: i64,
    pub price: String,
    pub trigger_price: Option<String>,
    pub status: String,
    pub fill_quantity: i64,
    pub average_fill_price: Option<String>,
    pub rejection_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Add up a money column in `Decimal`, not in `f64`.
///
/// These used to be `SUM(CAST(cost AS REAL))`: SQLite stores these values as
/// TEXT, so the sum went out through a binary float and back. That is exactly
/// the round-trip `rust_decimal` is in this project to avoid, and the result
/// was being compared against a budget — a number whose whole job is to be
/// exact.
///
/// A row that will not parse is an error rather than a zero. A silently
/// dropped cost makes the day's spend read low, which is the direction that
/// keeps spending.
fn sum_money(rows: &[(String,)], field: &str) -> Result<Decimal> {
    rows.iter().try_fold(Decimal::ZERO, |acc, (value,)| {
        Ok(acc + parse_money(value, field)?)
    })
}

/// Parse a money column, loudly.
///
/// A silently-zeroed price turns a loss into a reported profit, and a
/// silently-zeroed equity trips every breaker at once. Neither is a failure
/// anyone would notice in time.
fn parse_money(value: &str, field: &str) -> Result<Decimal> {
    Decimal::from_str(value).with_context(|| format!("Invalid decimal in {field}: {value:?}"))
}

/// One execution, with the order it belongs to. All decimals stay as strings
/// on the way to the dashboard — the UI parses them, and re-encoding through
/// f64 here is the round-trip the rest of this file exists to avoid.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct FillRecord {
    pub id: i64,
    pub order_id: i64,
    pub client_order_id: String,
    pub venue_id: String,
    pub symbol: String,
    pub side: String,
    pub intent: String,
    pub qty: String,
    pub price: String,
    pub fee: Option<String>,
    pub mid_at_submit: Option<String>,
    pub slippage_bps: Option<String>,
    pub time_to_fill_ms: Option<i64>,
    pub filled_at: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DailyEquityRecord {
    pub day: String,
    pub starting_equity: String,
    pub high_water_mark: String,
    pub closing_equity: Option<String>,
    pub realized_pnl: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ReconciliationRunRecord {
    pub id: i64,
    pub venue_id: String,
    pub cycle: Option<i64>,
    pub balance_delta: Option<String>,
    pub positions_missing_locally: i64,
    pub positions_missing_on_venue: i64,
    pub qty_mismatches: i64,
    pub unknown_open_orders: i64,
    pub passed: bool,
    pub detail: Option<String>,
    pub created_at: Option<String>,
}

/// A halt as persisted. Decimal-free, so it needs no parsing pass.
#[derive(Debug, Clone, FromRow)]
pub struct StoredHalt {
    pub source: String,
    pub scope: String,
    pub detail: Option<String>,
    pub raised_at: String,
    pub day: String,
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
    /// Mid price when this order was submitted, for measuring slippage once
    /// it fills. Only knowable here — by fill time the market has moved.
    pub mid_at_submit: Option<String>,
    pub submitted_at: Option<String>,
    pub updated_at: Option<String>,
    pub expires_at: Option<String>,
}

impl OrderRecord {
    /// A stop resting at the venue for an open position. Stored as an EXIT,
    /// since a fill closes the trade the same way, and told apart by its id.
    pub fn is_protective_stop(&self) -> bool {
        self.client_order_id.starts_with(PROTECTIVE_STOP_PREFIX)
    }
}

/// Raw row for an open venue position; decimals arrive as TEXT.
#[derive(Debug, Clone, FromRow)]
struct VenueOpenTradeRow {
    id: i64,
    venue_id: String,
    symbol: String,
    asset_class: String,
    side: Option<String>,
    strategy: Option<String>,
    entry_price: String,
    avg_fill_price: Option<String>,
    quantity: Option<String>,
    stop_pct: Option<String>,
    stop_price: Option<String>,
    target_price: Option<String>,
    horizon_hours: Option<i64>,
    created_at: Option<String>,
}

/// An open position in the NSE intraday paper book.
/// A closed strategy trade waiting for its post-mortem (src/review.rs).
#[derive(Debug, Clone, PartialEq)]
pub struct ClosedTrade {
    /// "IN" or "US".
    pub market: String,
    pub strategy: String,
    /// The row id in the trade's own table.
    pub trade_ref: i64,
    pub symbol: String,
    pub is_buy: bool,
    pub entry_price: Decimal,
    pub stop_price: Decimal,
    pub exit_price: Decimal,
    pub entry_at: DateTime<Utc>,
    pub exit_at: DateTime<Utc>,
    pub exit_reason: Option<String>,
    pub pnl: Decimal,
    /// Round-trip charges, where the book records them (India).
    pub charges: Option<Decimal>,
}

/// A finished post-mortem, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeReviewRow {
    pub day: String,
    pub minutes_held: i64,
    pub mfe_r: Option<Decimal>,
    pub mae_r: Option<Decimal>,
    pub index_move_pct: Option<Decimal>,
    pub label: String,
    pub lesson: String,
}

#[derive(Debug, FromRow, Serialize)]
struct StoredReview {
    market: String,
    strategy: String,
    trade_ref: i64,
    symbol: String,
    side: String,
    day: String,
    entry_at: String,
    exit_at: String,
    entry_price: String,
    stop_price: String,
    exit_price: String,
    exit_reason: Option<String>,
    pnl: String,
    charges: Option<String>,
    minutes_held: i64,
    mfe_r: Option<String>,
    mae_r: Option<String>,
    index_move_pct: Option<String>,
    label: String,
    lesson: String,
}

/// Timestamps in this database are RFC 3339 or SQLite's "YYYY-MM-DD HH:MM:SS"
/// (UTC).
fn parse_db_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|t| t.and_utc())
        })
}

#[derive(Debug, Clone)]
pub struct IntradayOpen {
    pub id: i64,
    pub day: String,
    pub symbol: String,
    pub is_buy: bool,
    pub qty: i64,
    pub entry_price: Decimal,
    pub stop_price: Decimal,
    pub entry_at: DateTime<Utc>,
}

/// One row of the U.S. options book still in flight.
/// A credit spread in flight (us::spreads).
#[derive(Debug, Clone, FromRow)]
pub struct SpreadTrade {
    pub id: i64,
    pub day: String,
    pub underlying: String,
    pub short_contract: String,
    pub long_contract: String,
    pub short_strike: String,
    pub long_strike: String,
    pub qty: i64,
    pub client_order_id: String,
    pub venue_order_id: Option<String>,
    pub status: String,
    pub entry_credit: Option<String>,
    pub entry_at: String,
    pub exit_order_id: Option<String>,
    pub exit_tries: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct OptionTrade {
    pub id: i64,
    pub day: String,
    pub stock_trade_id: i64,
    pub contract: String,
    pub qty: i64,
    pub client_order_id: String,
    pub venue_order_id: Option<String>,
    pub status: String,
    pub entry_price: Option<String>,
    pub exit_order_id: Option<String>,
    pub entry_at: String,
}

/// An open continuous-asset position, with decimals parsed.
#[derive(Debug, Clone)]
pub struct VenueOpenTrade {
    pub id: i64,
    pub venue_id: String,
    pub symbol: String,
    pub asset_class: String,
    /// A SELL entry: a short, closed by buying back. Quantity stays positive.
    pub is_short: bool,
    pub strategy: Option<String>,
    pub entry_price: Decimal,
    pub avg_fill_price: Option<Decimal>,
    pub quantity: Decimal,
    /// Stop distance at entry as a fraction of the entry price; 1R.
    pub stop_pct: Option<Decimal>,
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
            is_short: row
                .side
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("SELL")),
            strategy: row.strategy,
            entry_price: parse(&row.entry_price, "entry_price")?,
            avg_fill_price: parse_opt(&row.avg_fill_price, "avg_fill_price")?,
            quantity: parse_opt(&row.quantity, "quantity")?
                .context("trade is missing a quantity")?,
            // Legacy rows may carry an empty or unparseable stop_pct; it only
            // feeds stop management, so treat it as unknown rather than fail.
            stop_pct: row
                .stop_pct
                .as_deref()
                .and_then(|v| Decimal::from_str(v).ok())
                .filter(|v| *v > Decimal::ZERO && *v < Decimal::ONE),
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
    /// Units. What the legacy model calls `size` is cash, so both are carried.
    pub quantity: String,
    /// Cash value of the position, for the legacy `size` column.
    pub notional: String,
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
    /// The strategy that opened it ("orb", "sma_crossover"); None for legacy rows.
    pub strategy: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// Continuous assets are not sized by Kelly, so the risk fraction and the
    /// stop distance have columns of their own. They used to be written into
    /// kelly_raw and kelly_adjusted, which reported a plausible-looking Kelly
    /// fraction to anything reading those columns — including /api/trades —
    /// and was unrecoverable afterwards, because nothing recorded which rows
    /// meant which.
    #[tokio::test]
    async fn a_venue_trade_records_sizing_in_its_own_columns() {
        let store = Store::new(":memory:").await.expect("store");
        store
            .insert_venue_trade(&VenueTradeRecord {
                cycle: 1,
                venue_id: "alpaca".to_string(),
                symbol: "BTC/USD".to_string(),
                asset_class: "crypto_spot".to_string(),
                display_name: Some("BTC/USD".to_string()),
                side: "BUY".to_string(),
                entry_price: "100".to_string(),
                quantity: "6".to_string(),
                notional: "600".to_string(),
                avg_fill_price: Some("100".to_string()),
                edge_at_entry: "0.05".to_string(),
                fair_value: "104".to_string(),
                confidence: "0.8".to_string(),
                risk_pct: "0.0018".to_string(),
                stop_pct: "0.03".to_string(),
                status: "OPEN".to_string(),
                stop_price: Some("97".to_string()),
                target_price: Some("110".to_string()),
                horizon_hours: Some(24),
                client_order_id: Some("c-1".to_string()),
                venue_order_id: Some("v-1".to_string()),
                strategy: None,
            })
            .await
            .expect("insert");

        let row: (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT size, quantity, kelly_raw, kelly_adjusted, risk_pct, stop_pct FROM trades",
        )
        .fetch_one(store.pool())
        .await
        .expect("read back");

        // `size` is the legacy cash column; `quantity` is units. Binding units
        // to both made the dashboard's Size column mean dollars for one kind
        // of trade and units for the other.
        assert_eq!(row.0, "600", "size is the cash value");
        assert_eq!(row.1, "6", "quantity is units");
        assert_eq!(row.2, "0", "Kelly was not the sizing method");
        assert_eq!(row.3, "0");
        assert_eq!(row.4.as_deref(), Some("0.0018"), "risk fraction");
        assert_eq!(row.5.as_deref(), Some("0.03"), "stop distance");
    }

    #[tokio::test]
    async fn desk_decision_cards_round_trip_newest_first() {
        use crate::us::desk::*;
        let store = Store::new(":memory:").await.expect("store");
        let card = |symbol: &str| DecisionCard {
            at: Utc::now(),
            market: "US".into(),
            strategy: "orb".into(),
            symbol: symbol.into(),
            atlas: Atlas {
                relative_volume: dec!(3.1),
                range_high: dec!(101),
                range_low: dec!(99),
                in_play_today: 7,
            },
            vega: Vega {
                side: "LONG".into(),
                setup: "5-minute opening-range breakout".into(),
                signal_at: "09:40 ET".into(),
                trigger_close: dec!(101.5),
                stop: dec!(99),
                breakout_pct_of_range: dec!(25),
            },
            orion: orion(&[], Utc::now()),
            titan: Titan {
                allowed: true,
                reason: "within limits".into(),
                qty: dec!(1.9),
                notional_usd: dec!(200),
                risk_usd: dec!(4.9),
            },
            nova: Nova {
                action: Action::Trade,
                advisory_action: Action::Trade,
                enforced: false,
                reasons: vec!["ok".into()],
            },
        };
        store.insert_desk_decision(&card("AAA")).await.unwrap();
        store.insert_desk_decision(&card("BBB")).await.unwrap();
        let cards = store.recent_desk_decisions("US", 10).await.unwrap();
        assert!(store
            .recent_desk_decisions("IN", 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0]["symbol"], "BBB");
        assert_eq!(cards[0]["nova"]["action"], "TRADE");
        assert_eq!(cards[1]["atlas"]["in_play_today"], 7);
    }

    #[tokio::test]
    async fn the_intraday_book_opens_closes_and_totals() {
        let store = Store::new(":memory:").await.unwrap();
        store
            .insert_intraday_trade(
                "orb",
                "2026-11-02",
                "ITC",
                false,
                9,
                "250",
                "255",
                "2500",
                Utc::now(),
            )
            .await
            .unwrap();
        store
            .insert_intraday_trade(
                "burst",
                "2026-11-02",
                "TCS",
                true,
                2,
                "3000",
                "2970",
                "10000",
                Utc::now(),
            )
            .await
            .unwrap();
        let open = store.open_intraday_trades("orb").await.unwrap();
        assert_eq!(open.len(), 1, "each strategy sees only its own book");
        assert!(!open[0].is_buy);
        store
            .close_intraday_trade(open[0].id, "245", "FLAT_1515", "3.50", "41.50", Utc::now())
            .await
            .unwrap();
        assert!(store.open_intraday_trades("orb").await.unwrap().is_empty());
        assert_eq!(store.open_intraday_trades("burst").await.unwrap().len(), 1);
        assert_eq!(
            store.intraday_totals("orb", "2026-11-02").await.unwrap(),
            (1, dec!(41.50), dec!(41.50))
        );
        assert_eq!(
            store.intraday_totals("orb", "2026-11-03").await.unwrap(),
            (0, Decimal::ZERO, dec!(41.50))
        );
        assert_eq!(
            store.intraday_totals("burst", "2026-11-02").await.unwrap(),
            (1, Decimal::ZERO, Decimal::ZERO)
        );
        let recent = store.recent_intraday_trades(5).await.unwrap();
        assert_eq!(recent[0]["strategy"], "burst");
        assert_eq!(recent[1]["side"], "SHORT");
    }

    #[tokio::test]
    async fn a_closed_trade_is_reviewed_once() {
        let store = Store::new(":memory:").await.unwrap();
        store
            .insert_intraday_trade(
                "india_orb",
                "2026-11-02",
                "TCS",
                true,
                2,
                "100",
                "98",
                "10000",
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(
            store.unreviewed_intraday_trades().await.unwrap().is_empty(),
            "still open"
        );
        let id = store.open_intraday_trades("india_orb").await.unwrap()[0].id;
        store
            .close_intraday_trade(id, "97.5", "STOP", "3", "-8", Utc::now())
            .await
            .unwrap();
        let pending = store.unreviewed_intraday_trades().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].pnl, dec!(-8));
        let row = TradeReviewRow {
            day: "2026-11-02".into(),
            minutes_held: 5,
            mfe_r: Some(dec!(0.2)),
            mae_r: Some(dec!(1.25)),
            index_move_pct: None,
            label: "FALSE_BREAKOUT".into(),
            lesson: "test".into(),
        };
        store
            .insert_trade_review(&pending[0], &row, Utc::now())
            .await
            .unwrap();
        store
            .insert_trade_review(&pending[0], &row, Utc::now())
            .await
            .unwrap();
        assert!(store.unreviewed_intraday_trades().await.unwrap().is_empty());
        let all = store.recent_trade_reviews(10).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0]["label"], "FALSE_BREAKOUT");
        assert_eq!(all[0]["mae_r"], "1.25");
    }

    /// A SELL entry is a short: the open-trade view must say so, carry the
    /// strategy that opened it, and count it as one of the day's entries.
    #[tokio::test]
    async fn a_sell_entry_loads_as_a_short_with_its_strategy() {
        let store = Store::new(":memory:").await.expect("store");
        let id = store
            .insert_venue_trade(&VenueTradeRecord {
                cycle: 1,
                venue_id: "alpaca".to_string(),
                symbol: "TSLA".to_string(),
                asset_class: "equity".to_string(),
                display_name: Some("Tesla".to_string()),
                side: "SELL".to_string(),
                entry_price: "360".to_string(),
                quantity: "2".to_string(),
                notional: "720".to_string(),
                avg_fill_price: Some("360".to_string()),
                edge_at_entry: "0".to_string(),
                fair_value: "0".to_string(),
                confidence: "0".to_string(),
                risk_pct: "0.01".to_string(),
                stop_pct: "0.01".to_string(),
                status: "OPEN".to_string(),
                stop_price: Some("363.6".to_string()),
                target_price: None,
                horizon_hours: Some(7),
                client_order_id: Some("c-s".to_string()),
                venue_order_id: Some("v-s".to_string()),
                strategy: Some("orb".to_string()),
            })
            .await
            .expect("insert");

        let open = store.get_open_venue_trades().await.expect("open trades");
        assert_eq!(open.len(), 1);
        assert!(open[0].is_short);
        assert_eq!(open[0].strategy.as_deref(), Some("orb"));
        assert_eq!(
            store
                .count_venue_equity_entries_opened_on("alpaca", Utc::now().date_naive())
                .await
                .unwrap(),
            1,
            "a short is an entry too"
        );
        assert_eq!(
            store
                .strategy_symbols_entered_on("orb", Utc::now().date_naive())
                .await
                .unwrap(),
            vec!["TSLA".to_string()]
        );

        store.mark_trade(id, "350", "20", Utc::now()).await.unwrap();
        assert_eq!(
            store
                .strategy_day_pnl("orb", Utc::now().date_naive())
                .await
                .unwrap(),
            Decimal::from(20)
        );
    }

    /// A continuous position is closed by trading out of it, so it lands on
    /// CLOSED rather than RESOLVED_*. Selecting only the RESOLVED_ statuses
    /// made the entire continuous-asset P&L invisible to performance metrics
    /// while `total_trades` still counted it.
    #[tokio::test]
    async fn a_closed_position_counts_as_resolved() {
        let store = Store::new(":memory:").await.expect("store");
        store
            .insert_venue_trade(&VenueTradeRecord {
                cycle: 1,
                venue_id: "alpaca".to_string(),
                symbol: "BTC/USD".to_string(),
                asset_class: "crypto_spot".to_string(),
                display_name: None,
                side: "BUY".to_string(),
                entry_price: "100".to_string(),
                quantity: "1".to_string(),
                notional: "100".to_string(),
                avg_fill_price: Some("100".to_string()),
                edge_at_entry: "0.05".to_string(),
                fair_value: "104".to_string(),
                confidence: "0.8".to_string(),
                risk_pct: "0.0075".to_string(),
                stop_pct: "0.03".to_string(),
                status: "OPEN".to_string(),
                stop_price: None,
                target_price: None,
                horizon_hours: None,
                client_order_id: Some("c-1".to_string()),
                venue_order_id: None,
                strategy: None,
            })
            .await
            .expect("insert");

        let id = store.get_open_venue_trades().await.unwrap()[0].id;
        store
            .close_trade(id, "112", "12", "TAKE_PROFIT", "exit-1")
            .await
            .expect("close");

        let resolved = store.get_resolved_trades().await.expect("resolved");
        assert_eq!(resolved.len(), 1, "a closed position is a finished trade");
        assert_eq!(resolved[0].status, "CLOSED");
    }

    /// A database at 001 with real rows has to survive the rebuild in 002 and
    /// come out mapped — the migration drops and recreates `trades`, so a
    /// mistake here is silent data loss rather than an error. 003 then strips
    /// the fill claims 002's backfill invented.
    #[tokio::test]
    async fn a_legacy_database_upgrades_and_maps_its_rows() {
        let path = std::env::temp_dir().join(format!("migrate-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = sqlx::SqlitePool::connect(&url).await.expect("opens");

        // Stand up the pre-002 schema and put a trade in it.
        sqlx::raw_sql(include_str!("../../migrations/001_init.sql"))
            .execute(&pool)
            .await
            .expect("001 applies");
        sqlx::query(
            "INSERT INTO trades (cycle, market_id, market_question, direction, entry_price,
             size, edge_at_entry, claude_fair_value, confidence, kelly_raw, kelly_adjusted,
             status, pnl)
             VALUES (7, '0xdead', 'Will it rain?', 'NO', '0.35', '12.5', '0.11', '0.52',
                     '0.8', '0.2', '0.1', 'OPEN', '1.25')",
        )
        .execute(&pool)
        .await
        .expect("legacy row inserts");

        sqlx::raw_sql(include_str!("../../migrations/002_multi_venue.sql"))
            .execute(&pool)
            .await
            .expect("002 applies");
        // 002 carries the row across but copies the requested values into the
        // fill columns; 003 is what removes that claim. Applying both is the
        // chain a real database actually goes through.
        sqlx::raw_sql(include_str!(
            "../../migrations/003_fill_columns_and_sizing.sql"
        ))
        .execute(&pool)
        .await
        .expect("003 applies");

        let row: (
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT venue_id, symbol, asset_class, side, entry_price, size, quantity,
                 avg_fill_price FROM trades",
        )
        .fetch_one(&pool)
        .await
        .expect("the row survived the rebuild");

        assert_eq!(row.0, "polymarket");
        // A buy of the NO outcome, which is what the old direction column meant.
        assert_eq!(row.1, "0xdead:NO");
        assert_eq!(row.2, "prediction_binary");
        assert_eq!(row.3, "BUY");
        // What was requested is preserved...
        assert_eq!(row.4, "0.35");
        assert_eq!(row.5, "12.5");
        // ...and what filled stays unknown, because it never was known. Copying
        // the request in here would assert a fill nobody confirmed, and make
        // slippage over historical rows measure zero by construction.
        assert_eq!(row.6, None, "quantity must not claim a filled size");
        assert_eq!(row.7, None, "avg_fill_price must not claim a fill price");

        drop(pool);
        let _ = std::fs::remove_file(&path);
    }

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
            quantity: None,
            edge_at_entry: "0.12".to_string(),
            claude_fair_value: "0.77".to_string(),
            confidence: "0.85".to_string(),
            kelly_raw: "0.04".to_string(),
            kelly_adjusted: "0.02".to_string(),
            status: "OPEN".to_string(),
            pnl: None,
            created_at: None,
            resolved_at: None,
            ..Default::default()
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

    /// A halt in force is re-offered on every restart, because the loop
    /// deliberately re-runs its side effects then — re-cancelling resting
    /// orders after a crash is worth doing. The audit trail must not grow a
    /// row each time, or "when did this halt start" stops being answerable.
    #[tokio::test]
    async fn re_recording_the_same_halt_does_not_duplicate_it() {
        use chrono::TimeZone;
        let at = Utc.with_ymd_and_hms(2026, 9, 21, 12, 0, 30).unwrap();
        let store = Store::new(":memory:").await.unwrap();
        for _ in 0..5 {
            store
                .insert_halt("api", "until_resume", "operator", at)
                .await
                .unwrap();
        }
        assert_eq!(store.halt_count().await.unwrap(), 1);
        assert_eq!(store.active_halt().await.unwrap().unwrap().source, "api");
    }

    #[tokio::test]
    async fn a_genuinely_later_halt_is_a_new_row() {
        use chrono::TimeZone;
        let first = Utc.with_ymd_and_hms(2026, 9, 21, 12, 0, 30).unwrap();
        let second = Utc.with_ymd_and_hms(2026, 9, 21, 12, 0, 31).unwrap();
        let store = Store::new(":memory:").await.unwrap();
        store
            .insert_halt("api", "until_resume", "first", first)
            .await
            .unwrap();
        store
            .insert_halt("circuit_breaker", "rest_of_day", "second", second)
            .await
            .unwrap();
        assert_eq!(store.halt_count().await.unwrap(), 2);
        assert_eq!(
            store.active_halt().await.unwrap().unwrap().source,
            "circuit_breaker",
            "the newest uncleared row is the one in force"
        );
    }

    #[tokio::test]
    async fn clearing_lifts_every_halt_in_force() {
        use chrono::TimeZone;
        let t = |s| Utc.with_ymd_and_hms(2026, 9, 21, 12, 0, s).unwrap();
        let store = Store::new(":memory:").await.unwrap();
        store
            .insert_halt("api", "until_resume", "first", t(30))
            .await
            .unwrap();
        store
            .insert_halt("signal", "until_resume", "second", t(31))
            .await
            .unwrap();
        store.clear_halts("api", t(40)).await.unwrap();
        assert!(
            store.active_halt().await.unwrap().is_none(),
            "a resume must not leave a second halt silently in force"
        );
    }

    /// The day's *opening* equity is fixed by the first cycle of that day and
    /// never moves again.
    ///
    /// The daily-loss breaker measures against it. Letting a later cycle
    /// rewrite it would make the limit measure the loss since the last cycle
    /// rather than since the open — a limit that can never be reached no
    /// matter how much is lost.
    #[tokio::test]
    async fn the_days_opening_equity_is_fixed_by_the_first_cycle() {
        let store = Store::new(":memory:").await.unwrap();
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();

        let first = store.record_equity(day, dec!(100)).await.unwrap();
        assert_eq!(first.starting_equity, dec!(100));

        let later = store.record_equity(day, dec!(80)).await.unwrap();
        assert_eq!(
            later.starting_equity,
            dec!(100),
            "the open must not follow the current value down"
        );
    }

    #[tokio::test]
    async fn the_high_water_mark_rises_with_a_new_high_and_never_falls() {
        let store = Store::new(":memory:").await.unwrap();
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();

        assert_eq!(
            store
                .record_equity(day, dec!(100))
                .await
                .unwrap()
                .high_water_mark,
            dec!(100)
        );
        assert_eq!(
            store
                .record_equity(day, dec!(120))
                .await
                .unwrap()
                .high_water_mark,
            dec!(120),
            "a new high raises the peak"
        );
        assert_eq!(
            store
                .record_equity(day, dec!(90))
                .await
                .unwrap()
                .high_water_mark,
            dec!(120),
            "a fall must not lower it, or drawdown is always zero"
        );
    }

    /// Drawdown is peak-to-trough over the life of the account. A peak that
    /// reset each midnight would let an account bleed a few percent a day for
    /// a fortnight without ever reporting a drawdown worth halting on.
    #[tokio::test]
    async fn the_high_water_mark_carries_across_days() {
        let store = Store::new(":memory:").await.unwrap();
        let d = |n| chrono::NaiveDate::from_ymd_opt(2026, 9, n).unwrap();

        store.record_equity(d(19), dec!(100)).await.unwrap();
        store.record_equity(d(19), dec!(140)).await.unwrap();

        let next_day = store.record_equity(d(20), dec!(90)).await.unwrap();
        assert_eq!(
            next_day.starting_equity,
            dec!(90),
            "a new day opens where it opens"
        );
        assert_eq!(
            next_day.high_water_mark,
            dec!(140),
            "yesterday's peak is still the yardstick"
        );
    }

    /// 11 Nov 2026: the last mark of the day was taken mid-session (an open
    /// spread at -$24), the laptop then restarted, and the spread expired
    /// for +$15. The first mark of the next day is the true close and must
    /// replace the stale one, or the day shows a loss it never had.
    #[tokio::test]
    async fn the_next_days_first_mark_closes_a_day_left_stale() {
        let store = Store::new(":memory:").await.unwrap();
        let d = |n| chrono::NaiveDate::from_ymd_opt(2026, 11, n).unwrap();
        store.record_equity(d(11), dec!(1935.04)).await.unwrap();
        store.record_equity(d(11), dec!(1910.82)).await.unwrap(); // stale mid-session mark
        store.record_equity(d(12), dec!(1949.82)).await.unwrap(); // agent back after the close
        store.record_equity(d(12), dec!(1949.75)).await.unwrap();
        let rows = store.get_daily_equity().await.unwrap();
        let close = |day: &str| {
            rows.iter()
                .find(|r| r.day == day)
                .and_then(|r| r.closing_equity.clone())
                .unwrap()
        };
        assert_eq!(
            close("2026-11-11"),
            "1949.82",
            "yesterday closes where today opened"
        );
        assert_eq!(
            close("2026-11-12"),
            "1949.75",
            "today's close is today's latest mark"
        );
        // a later mark on the same day must not touch yesterday again
        store.record_equity(d(12), dec!(1800)).await.unwrap();
        let rows = store.get_daily_equity().await.unwrap();
        assert_eq!(
            rows.iter()
                .find(|r| r.day == "2026-11-11")
                .unwrap()
                .closing_equity
                .as_deref(),
            Some("1949.82")
        );
    }

    #[tokio::test]
    async fn the_first_ever_reading_becomes_the_peak() {
        let store = Store::new(":memory:").await.unwrap();
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        let marks = store.record_equity(day, dec!(250)).await.unwrap();
        assert_eq!(marks.high_water_mark, dec!(250));
        assert_eq!(marks.starting_equity, dec!(250));
    }

    /// The peak is compared in `Decimal`, not by a SQL MAX over a float cast.
    /// These two differ only past f64's precision, which is exactly where a
    /// float comparison would pick the wrong row.
    #[tokio::test]
    async fn the_peak_is_chosen_by_decimal_comparison() {
        let store = Store::new(":memory:").await.unwrap();
        let d = |n| chrono::NaiveDate::from_ymd_opt(2026, 9, n).unwrap();
        store
            .record_equity(d(19), dec!(10000.00000000000001))
            .await
            .unwrap();
        store
            .record_equity(d(20), dec!(10000.00000000000002))
            .await
            .unwrap();
        let marks = store.record_equity(d(21), dec!(1)).await.unwrap();
        assert_eq!(marks.high_water_mark, dec!(10000.00000000000002));
    }

    #[tokio::test]
    async fn todays_equity_is_readable_back_through_the_dashboard_query() {
        // The risk page reads this series; a mark the breaker wrote and the
        // page cannot see is a silent disagreement between two screens.
        let store = Store::new(":memory:").await.unwrap();
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        store.record_equity(day, dec!(100)).await.unwrap();
        store.record_equity(day, dec!(97.5)).await.unwrap();

        let rows = store.get_daily_equity().await.unwrap();
        assert_eq!(rows.len(), 1, "one row per day, not one per cycle");
        assert_eq!(rows[0].starting_equity, "100");
        assert_eq!(
            rows[0].closing_equity.as_deref(),
            Some("97.5"),
            "the latest reading is what the page shows as current"
        );
    }

    /// Insert a closed trade with explicit open and close times.
    #[cfg(test)]
    async fn seed_closed(store: &Store, opened: &str, closed: &str, pnl: &str) {
        sqlx::query(
            "INSERT INTO trades (cycle, venue_id, market_id, symbol, asset_class, direction,
                                 side, entry_price, size, edge_at_entry, claude_fair_value,
                                 confidence, kelly_raw, kelly_adjusted, status, pnl,
                                 created_at, closed_at)
             VALUES (1,'alpaca','BTC/USD','BTC/USD','crypto_spot','LONG','BUY','100','1',
                     '0.1','0.6','0.8','0','0','CLOSED', ?, ?, ?)",
        )
        .bind(pnl)
        .bind(opened)
        .bind(closed)
        .execute(store.pool())
        .await
        .unwrap();
    }

    /// A streak is about the order results *arrived* in, not the order the
    /// positions were opened in.
    ///
    /// Venue trades record their close in `closed_at`; only the legacy
    /// prediction path writes `resolved_at`. Ordering by `created_at` as the
    /// fallback sorted by entry time, so a long-held loser that closed most
    /// recently was buried under a position opened later and closed sooner —
    /// and the losing streak read zero while the account was on a losing run.
    #[tokio::test]
    async fn a_losing_streak_is_ordered_by_when_positions_closed() {
        let store = Store::new(":memory:").await.unwrap();

        // Opened first, closed last, and lost.
        seed_closed(&store, "2026-09-19T09:00:00Z", "2026-09-21T09:00:00Z", "-5").await;
        // Opened later, closed sooner, and won.
        seed_closed(&store, "2026-09-20T09:00:00Z", "2026-09-20T10:00:00Z", "10").await;

        assert_eq!(
            store.consecutive_losses().await.unwrap(),
            1,
            "the most recent close was a loss, so the streak is one"
        );
    }

    #[tokio::test]
    async fn a_win_arriving_last_ends_the_streak() {
        let store = Store::new(":memory:").await.unwrap();
        seed_closed(&store, "2026-09-19T09:00:00Z", "2026-09-19T10:00:00Z", "-5").await;
        seed_closed(&store, "2026-09-20T09:00:00Z", "2026-09-20T10:00:00Z", "-3").await;
        seed_closed(&store, "2026-09-21T09:00:00Z", "2026-09-21T10:00:00Z", "2").await;
        assert_eq!(store.consecutive_losses().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn consecutive_losses_counts_back_to_the_first_non_loss() {
        let store = Store::new(":memory:").await.unwrap();
        seed_closed(&store, "2026-09-18T09:00:00Z", "2026-09-18T10:00:00Z", "-1").await;
        seed_closed(&store, "2026-09-19T09:00:00Z", "2026-09-19T10:00:00Z", "7").await;
        seed_closed(&store, "2026-09-20T09:00:00Z", "2026-09-20T10:00:00Z", "-2").await;
        seed_closed(&store, "2026-09-21T09:00:00Z", "2026-09-21T10:00:00Z", "-3").await;
        assert_eq!(
            store.consecutive_losses().await.unwrap(),
            2,
            "the win two closes back ends the count"
        );
    }

    /// A flat close is not a loss. Counting it as one would trip the breaker
    /// on a run of break-even exits, which is not a losing streak.
    #[tokio::test]
    async fn a_flat_close_ends_the_streak() {
        let store = Store::new(":memory:").await.unwrap();
        seed_closed(&store, "2026-09-20T09:00:00Z", "2026-09-20T10:00:00Z", "-4").await;
        seed_closed(&store, "2026-09-21T09:00:00Z", "2026-09-21T10:00:00Z", "0").await;
        assert_eq!(store.consecutive_losses().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn no_closes_is_no_streak() {
        let store = Store::new(":memory:").await.unwrap();
        assert_eq!(store.consecutive_losses().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn lost_polymarket_bets_are_not_a_stock_losing_streak() {
        let store = Store::new(":memory:").await.unwrap();
        seed_closed(&store, "2026-01-02T09:00:00Z", "2026-01-02T10:00:00Z", "-1").await;
        for i in 0..12 {
            sqlx::query(
                "INSERT INTO trades (cycle, venue_id, market_id, symbol, direction, side, entry_price, size,
                                     edge_at_entry, claude_fair_value, confidence, kelly_raw, kelly_adjusted,
                                     status, pnl, resolved_at)
                 VALUES (1, 'polymarket', ?, ?, 'YES', 'BUY', '0.25', '8', '0', '0.25', '0', '0', '0',
                         'RESOLVED_LOSS', '-2', '2026-01-03T10:00:00Z')",
            )
            .bind(format!("0x{i}"))
            .bind(format!("0x{i}:YES"))
            .execute(store.pool())
            .await
            .unwrap();
        }
        assert_eq!(
            store.consecutive_losses().await.unwrap(),
            1,
            "only the stock loss counts"
        );
    }

    /// `max_trades_per_day` is only a limit if the count can see the rows.
    ///
    /// Neither insert sets `created_at`; both rely on the column default
    /// `datetime('now')`, which writes zone-less UTC as `YYYY-MM-DD HH:MM:SS`.
    /// The count compares `date(created_at)` against a `NaiveDate`. A format
    /// mismatch between those two would not error — it would silently return
    /// zero forever, and the trade-count breaker would never bind.
    #[tokio::test]
    async fn todays_trades_are_counted_through_the_real_insert_path() {
        let store = Store::new(":memory:").await.unwrap();
        let today = Utc::now().date_naive();

        assert_eq!(store.count_trades_opened_on(today).await.unwrap(), 0);

        store
            .insert_trade(&TradeRecord {
                id: None,
                cycle: 1,
                market_id: "0xabc".to_string(),
                market_question: Some("Will it rain?".to_string()),
                direction: "YES".to_string(),
                entry_price: "0.40".to_string(),
                size: "10".to_string(),
                quantity: None,
                edge_at_entry: "0.1".to_string(),
                claude_fair_value: "0.5".to_string(),
                confidence: "0.8".to_string(),
                kelly_raw: "0.05".to_string(),
                kelly_adjusted: "0.02".to_string(),
                status: "OPEN".to_string(),
                pnl: None,
                created_at: None,
                resolved_at: None,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(
            store.count_trades_opened_on(today).await.unwrap(),
            0,
            "a Polymarket bet must not use up the stock books' daily cap"
        );

        // A venue trade (here Alpaca) opened now through the column default.
        sqlx::query(
            "INSERT INTO trades (cycle, venue_id, market_id, symbol, asset_class, direction,
                                 side, entry_price, size, edge_at_entry, claude_fair_value,
                                 confidence, kelly_raw, kelly_adjusted, status)
             VALUES (1,'alpaca','NVDA','NVDA','equity','LONG','BUY','100','1',
                     '0.1','0.6','0.8','0','0','OPEN')",
        )
        .execute(store.pool())
        .await
        .unwrap();

        assert_eq!(
            store.count_trades_opened_on(today).await.unwrap(),
            1,
            "a stock trade opened today must be visible to the daily cap"
        );
        assert_eq!(
            store
                .count_trades_opened_on(today - chrono::Duration::days(1))
                .await
                .unwrap(),
            0,
            "and must not be counted against yesterday"
        );
    }

    #[tokio::test]
    async fn venue_equity_daily_count_counts_that_venues_stock_entries_both_ways() {
        let store = Store::new(":memory:").await.unwrap();
        let today = Utc::now().date_naive();
        let trade =
            |venue_id: &str, symbol: &str, asset_class: &str, side: &str| VenueTradeRecord {
                cycle: 1,
                venue_id: venue_id.to_string(),
                symbol: symbol.to_string(),
                asset_class: asset_class.to_string(),
                display_name: Some(symbol.to_string()),
                side: side.to_string(),
                entry_price: "100".to_string(),
                quantity: "1".to_string(),
                notional: "100".to_string(),
                avg_fill_price: Some("100".to_string()),
                edge_at_entry: "0".to_string(),
                fair_value: "0.5".to_string(),
                confidence: "0.5".to_string(),
                risk_pct: "0.01".to_string(),
                stop_pct: "0.03".to_string(),
                status: "OPEN".to_string(),
                stop_price: Some("97".to_string()),
                target_price: None,
                horizon_hours: Some(24),
                client_order_id: None,
                venue_order_id: None,
                strategy: None,
            };

        for row in [
            trade("alpaca", "AAPL", "equity", "BUY"),
            trade("alpaca", "BTC/USD", "crypto_spot", "BUY"),
            trade("alpaca", "MSFT", "equity", "SELL"),
            trade("coinbase", "AAPL", "equity", "BUY"),
        ] {
            store.insert_venue_trade(&row).await.unwrap();
        }

        // AAPL long and MSFT short; crypto and the other venue do not count.
        assert_eq!(
            store
                .count_venue_equity_entries_opened_on("alpaca", today)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .count_venue_equity_entries_opened_on("coinbase", today)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn trades_opened_on_another_day_are_not_counted_today() {
        let store = Store::new(":memory:").await.unwrap();
        seed_closed(&store, "2026-01-02T09:00:00Z", "2026-01-02T10:00:00Z", "-1").await;
        let that_day = chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
        assert_eq!(store.count_trades_opened_on(that_day).await.unwrap(), 1);
        assert_eq!(
            store
                .count_trades_opened_on(Utc::now().date_naive())
                .await
                .unwrap(),
            0
        );
    }

    #[cfg(test)]
    async fn seed_open(store: &Store, cost: &str, unrealized: Option<&str>) {
        sqlx::query(
            "INSERT INTO trades (cycle, venue_id, market_id, symbol, asset_class, direction,
                                 side, entry_price, size, edge_at_entry, claude_fair_value,
                                 confidence, kelly_raw, kelly_adjusted, status, unrealized_pnl)
             VALUES (1,'alpaca','BTC/USD','BTC/USD','crypto_spot','LONG','BUY', ?, '1',
                     '0.1','0.6','0.8','0','0','OPEN', ?)",
        )
        .bind(cost)
        .bind(unrealized)
        .execute(store.pool())
        .await
        .unwrap();
    }

    /// Equity must move when an open position loses, or the daily-loss and
    /// drawdown breakers only ever see *realised* P&L.
    ///
    /// Cash plus cost basis does not move: buying $10 of something takes $10
    /// out of cash and puts $10 of exposure on, and if it halves, neither
    /// number changes. This is the term that makes the difference.
    #[tokio::test]
    async fn unrealized_pnl_sums_across_open_positions() {
        let store = Store::new(":memory:").await.unwrap();
        seed_open(&store, "10", Some("-6")).await;
        seed_open(&store, "20", Some("2.5")).await;
        assert_eq!(store.total_unrealized_pnl().await.unwrap(), dec!(-3.5));
    }

    /// A position the mark pass has not reached yet contributes nothing —
    /// the honest answer, rather than a guess at what it might be worth.
    #[tokio::test]
    async fn an_unmarked_position_contributes_nothing_to_unrealized_pnl() {
        let store = Store::new(":memory:").await.unwrap();
        seed_open(&store, "10", None).await;
        seed_open(&store, "10", Some("-4")).await;
        assert_eq!(store.total_unrealized_pnl().await.unwrap(), dec!(-4));
    }

    #[tokio::test]
    async fn closed_positions_are_excluded_from_unrealized_pnl() {
        let store = Store::new(":memory:").await.unwrap();
        seed_open(&store, "10", Some("-4")).await;
        seed_closed(
            &store,
            "2026-09-20T09:00:00Z",
            "2026-09-20T10:00:00Z",
            "-99",
        )
        .await;
        assert_eq!(
            store.total_unrealized_pnl().await.unwrap(),
            dec!(-4),
            "a closed position's P&L is realised, and is already in the cash balance"
        );
    }

    #[tokio::test]
    async fn no_open_positions_is_zero_not_an_error() {
        let store = Store::new(":memory:").await.unwrap();
        assert_eq!(store.total_unrealized_pnl().await.unwrap(), Decimal::ZERO);
    }

    /// Slippage is signed so positive always means it cost us, whichever side
    /// the order was. Averaging raw price-minus-mid across mixed sides makes
    /// a book bleeding on both look like excellent execution.
    #[tokio::test]
    async fn a_buy_above_the_mid_and_a_sell_below_it_both_read_as_cost() {
        let store = Store::new(":memory:").await.unwrap();
        let at = Utc::now();
        let oid = seed_order(&store, "BUY", Some("100")).await;

        // Bought at 101 against a mid of 100: paid 100 bps up.
        store
            .record_fill(
                oid,
                None,
                dec!(1),
                dec!(101),
                None,
                Some(dec!(100)),
                "BUY",
                None,
                at,
            )
            .await
            .unwrap();
        // Sold at 99 against a mid of 100: gave up 100 bps.
        store
            .record_fill(
                oid,
                None,
                dec!(1),
                dec!(99),
                None,
                Some(dec!(100)),
                "SELL",
                None,
                at,
            )
            .await
            .unwrap();

        let fills = store.get_fills(10).await.unwrap();
        let mut bps: Vec<&str> = fills
            .iter()
            .map(|f| f.slippage_bps.as_deref().unwrap())
            .collect();
        bps.sort();
        // Exact, not a prefix. `starts_with("100")` is also true of "10000",
        // so a 100x error in the bps scale — the unit the ≤10/≤30 criteria
        // are stated in — would have passed unnoticed.
        assert_eq!(
            bps,
            vec!["100.00", "100.00"],
            "both sides must read exactly +100 bps"
        );
    }

    #[tokio::test]
    async fn price_improvement_reads_negative() {
        let store = Store::new(":memory:").await.unwrap();
        let oid = seed_order(&store, "BUY", Some("100")).await;
        store
            .record_fill(
                oid,
                None,
                dec!(1),
                dec!(99.5),
                None,
                Some(dec!(100)),
                "BUY",
                None,
                Utc::now(),
            )
            .await
            .unwrap();
        let fills = store.get_fills(10).await.unwrap();
        assert_eq!(fills[0].slippage_bps.as_deref(), Some("-50.00"));
    }

    /// An order placed before `mid_at_submit` existed carries no slippage.
    /// Recording zero would claim perfect execution on every historical fill.
    #[tokio::test]
    async fn a_fill_without_a_mid_carries_no_slippage_rather_than_zero() {
        let store = Store::new(":memory:").await.unwrap();
        let oid = seed_order(&store, "BUY", None).await;
        store
            .record_fill(
                oid,
                None,
                dec!(1),
                dec!(101),
                None,
                None,
                "BUY",
                None,
                Utc::now(),
            )
            .await
            .unwrap();
        let fills = store.get_fills(10).await.unwrap();
        assert_eq!(fills[0].slippage_bps, None);
        assert_eq!(fills[0].mid_at_submit, None);
    }

    #[tokio::test]
    async fn a_zero_mid_does_not_divide_by_zero() {
        let store = Store::new(":memory:").await.unwrap();
        let oid = seed_order(&store, "BUY", Some("0")).await;
        store
            .record_fill(
                oid,
                None,
                dec!(1),
                dec!(101),
                None,
                Some(Decimal::ZERO),
                "BUY",
                None,
                Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(store.get_fills(10).await.unwrap()[0].slippage_bps, None);
    }

    #[tokio::test]
    async fn time_to_fill_is_measured_from_submission() {
        let store = Store::new(":memory:").await.unwrap();
        let oid = seed_order(&store, "BUY", Some("100")).await;
        let submitted = "2026-09-21T10:00:00+00:00";
        let filled = DateTime::parse_from_rfc3339("2026-09-21T10:00:03+00:00")
            .unwrap()
            .with_timezone(&Utc);
        store
            .record_fill(
                oid,
                None,
                dec!(1),
                dec!(100),
                None,
                Some(dec!(100)),
                "BUY",
                Some(submitted),
                filled,
            )
            .await
            .unwrap();
        assert_eq!(
            store.get_fills(10).await.unwrap()[0].time_to_fill_ms,
            Some(3000)
        );
    }

    /// Clock skew between this host and the venue can put a fill "before" its
    /// own submission. A negative duration is not a fast fill.
    #[tokio::test]
    async fn a_negative_time_to_fill_is_discarded() {
        let store = Store::new(":memory:").await.unwrap();
        let oid = seed_order(&store, "BUY", Some("100")).await;
        let submitted = "2026-09-21T10:00:05+00:00";
        let filled = DateTime::parse_from_rfc3339("2026-09-21T10:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        store
            .record_fill(
                oid,
                None,
                dec!(1),
                dec!(100),
                None,
                Some(dec!(100)),
                "BUY",
                Some(submitted),
                filled,
            )
            .await
            .unwrap();
        assert_eq!(store.get_fills(10).await.unwrap()[0].time_to_fill_ms, None);
    }

    #[cfg(test)]
    async fn seed_order(store: &Store, side: &str, mid: Option<&str>) -> i64 {
        store
            .insert_order(&OrderRecord {
                id: None,
                client_order_id: format!("c-{side}-{:?}-{}", mid, uuid::Uuid::new_v4()),
                venue_order_id: Some("v1".to_string()),
                venue_id: "alpaca".to_string(),
                symbol: "BTC/USD".to_string(),
                side: side.to_string(),
                intent: "ENTRY".to_string(),
                trade_id: None,
                limit_price: Some("100".to_string()),
                qty: "1".to_string(),
                filled_qty: "0".to_string(),
                avg_fill_price: None,
                state: "PENDING".to_string(),
                reject_reason: None,
                cycle: Some(1),
                mid_at_submit: mid.map(str::to_string),
                submitted_at: None,
                updated_at: None,
                expires_at: None,
            })
            .await
            .unwrap()
    }
}
