-- The NSE intraday paper book, apart from the cash sandbox: shorts and MIS
-- charges do not fit india_sandbox_orders' long-only delivery accounting.
CREATE TABLE IF NOT EXISTS india_intraday_trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    day TEXT NOT NULL,
    symbol TEXT NOT NULL,
    is_buy INTEGER NOT NULL,
    qty INTEGER NOT NULL,
    entry_price TEXT NOT NULL,
    stop_price TEXT NOT NULL,
    capital TEXT NOT NULL,
    entry_at TEXT NOT NULL,
    exit_price TEXT,
    exit_reason TEXT,
    charges TEXT,
    pnl TEXT,
    exit_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_india_intraday_day ON india_intraday_trades(day);
