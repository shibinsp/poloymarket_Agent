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
        // `quantity` stays NULL here. It means "what actually filled", and the
        // legacy execution path records a trade the moment an order id comes
        // back, without confirming anything. Mirroring `size` into it asserts
        // a fill nobody saw — the same claim migration 003 removes from the
        // rows 002 carried across.
        .bind(Option::<String>::None)
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
            "INSERT INTO trades (cycle, venue_id, symbol, asset_class, market_id, market_question, direction, side, entry_price, size, quantity, avg_fill_price, edge_at_entry, claude_fair_value, confidence, kelly_raw, kelly_adjusted, risk_pct, stop_pct, status, stop_price, target_price, horizon_hours, client_order_id, venue_order_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        let prior_peak: Option<String> =
            sqlx::query_scalar("SELECT MAX(CAST(high_water_mark AS REAL)) FROM daily_equity")
                .fetch_optional(&self.pool)
                .await
                .context("Failed to read the prior high-water mark")?
                .flatten();
        // Read back as TEXT to avoid a float round-trip on money; the MAX
        // above is only used to pick a row, never as the value itself.
        let prior_peak: Decimal = match prior_peak {
            Some(_) => {
                let best: Option<String> = sqlx::query_scalar(
                    "SELECT high_water_mark FROM daily_equity
                     ORDER BY CAST(high_water_mark AS REAL) DESC LIMIT 1",
                )
                .fetch_optional(&self.pool)
                .await
                .context("Failed to read the prior high-water mark")?;
                match best {
                    Some(v) => parse_money(&v, "daily_equity.high_water_mark")?,
                    None => Decimal::ZERO,
                }
            }
            None => Decimal::ZERO,
        };

        let high_water_mark = prior_peak.max(equity);

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
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM trades WHERE date(created_at) = ?")
                .bind(day.to_string())
                .fetch_one(&self.pool)
                .await
                .context("Failed to count today's trades")?;
        Ok(count.max(0) as u32)
    }

    /// How many of the most recently closed positions were losses, counting
    /// back from the latest until a non-loss is reached.
    ///
    /// Ordered by when each position *closed*, not when it opened: a streak
    /// is about the order the results arrived in, and a long-held winner
    /// opened before three quick losers does not break them up.
    pub async fn consecutive_losses(&self) -> Result<u32> {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT status, pnl FROM trades
             WHERE status IN ('CLOSED', 'RESOLVED_WIN', 'RESOLVED_LOSS')
             ORDER BY COALESCE(resolved_at, created_at) DESC, id DESC
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
