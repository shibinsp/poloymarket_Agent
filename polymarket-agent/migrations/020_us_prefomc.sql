-- US pre-FOMC drift paper book, 11 Nov 2026. Simulated inside the agent; no
-- orders are sent.
CREATE TABLE IF NOT EXISTS us_prefomc_trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol TEXT NOT NULL,
    qty TEXT NOT NULL,
    fomc_day TEXT NOT NULL,
    entry_day TEXT NOT NULL,
    entry_price TEXT NOT NULL,
    entry_at TEXT NOT NULL,
    exit_day TEXT,
    exit_price TEXT,
    exit_at TEXT,
    pnl TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_us_prefomc_meeting ON us_prefomc_trades(symbol, fomc_day);
