-- US Market Testing Isolation: Backtest runs and intraday market limits.
--
-- Keeps US strategy historical backtests strictly segregated and records
-- multi-currency portfolio snapshots and emergency stop states.

CREATE TABLE IF NOT EXISTS us_backtests (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    config_json TEXT NOT NULL,
    results_json TEXT NOT NULL,
    status TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_us_backtests_created_at ON us_backtests(created_at DESC);
