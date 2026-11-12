-- US "buy a weak close" (IBS) paper book, 9 Nov 2026. Simulated inside the
-- agent at live IEX prices; no orders are sent.
CREATE TABLE IF NOT EXISTS us_ibs_trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol TEXT NOT NULL,
    qty TEXT NOT NULL,
    entry_day TEXT NOT NULL,
    entry_price TEXT NOT NULL,
    entry_ibs TEXT NOT NULL,
    entry_at TEXT NOT NULL,
    exit_day TEXT,
    exit_price TEXT,
    exit_ibs TEXT,
    exit_at TEXT,
    pnl TEXT
);
CREATE INDEX IF NOT EXISTS idx_us_ibs_open ON us_ibs_trades(exit_at);
