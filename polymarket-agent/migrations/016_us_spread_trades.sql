-- The U.S. credit-spread book (paper): a same-day (0DTE) bear call spread on
-- SPY and QQQ, sold at 11:00 New York and bought back by 15:50. One row per
-- spread; prices are per share of the spread (x100 a contract).
CREATE TABLE IF NOT EXISTS us_spread_trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    day TEXT NOT NULL,
    underlying TEXT NOT NULL,
    short_contract TEXT NOT NULL,
    long_contract TEXT NOT NULL,
    short_strike TEXT NOT NULL,
    long_strike TEXT NOT NULL,
    underlying_price TEXT NOT NULL,
    qty INTEGER NOT NULL,
    -- the credit asked for when selling the spread
    limit_credit TEXT NOT NULL,
    client_order_id TEXT NOT NULL,
    venue_order_id TEXT,
    -- PENDING (entry sent), OPEN, CLOSING (exit sent), CLOSED, CANCELLED
    status TEXT NOT NULL,
    entry_credit TEXT,
    entry_at TEXT NOT NULL,
    exit_client_order_id TEXT,
    exit_order_id TEXT,
    exit_tries INTEGER NOT NULL DEFAULT 0,
    exit_debit TEXT,
    exit_reason TEXT,
    exit_at TEXT,
    pnl TEXT
);
CREATE INDEX IF NOT EXISTS idx_us_spread_trades_day ON us_spread_trades(day);
