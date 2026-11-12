-- The U.S. options book (paper): a call or put bought on each ORB / burst
-- stock entry and sold when that stock trade closes. Its own $500 ledger,
-- apart from `trades`, which the venue exit pass would try to manage.
CREATE TABLE IF NOT EXISTS us_option_trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    day TEXT NOT NULL,
    stock_trade_id INTEGER NOT NULL UNIQUE,
    underlying TEXT NOT NULL,
    contract TEXT NOT NULL,
    is_call INTEGER NOT NULL,
    qty INTEGER NOT NULL,
    limit_price TEXT NOT NULL,
    client_order_id TEXT NOT NULL,
    venue_order_id TEXT,
    -- PENDING (entry sent), OPEN, CLOSING (exit sent), CLOSED, CANCELLED
    status TEXT NOT NULL,
    entry_price TEXT,
    entry_at TEXT NOT NULL,
    exit_client_order_id TEXT,
    exit_order_id TEXT,
    exit_price TEXT,
    exit_reason TEXT,
    exit_at TEXT,
    pnl TEXT
);
CREATE INDEX IF NOT EXISTS idx_us_option_trades_day ON us_option_trades(day);
