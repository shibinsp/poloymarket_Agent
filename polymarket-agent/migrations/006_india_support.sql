-- Indian Market Testing Isolation: Backtest runs and sandbox audit trail.
--
-- Keeps Indian strategy historical simulations and Upstox Sandbox test data
-- strictly segregated from live/paper US stocks, crypto, and Polymarket.

CREATE TABLE IF NOT EXISTS india_backtests (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    config_json TEXT NOT NULL,
    results_json TEXT NOT NULL,
    status TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_india_backtests_created_at ON india_backtests(created_at DESC);

CREATE TABLE IF NOT EXISTS india_sandbox_orders (
    id TEXT PRIMARY KEY,
    client_order_id TEXT NOT NULL UNIQUE,
    venue_order_id TEXT,
    symbol TEXT NOT NULL,
    side TEXT NOT NULL,
    order_type TEXT NOT NULL,
    quantity INTEGER NOT NULL,
    price TEXT NOT NULL,
    trigger_price TEXT,
    status TEXT NOT NULL,
    fill_quantity INTEGER NOT NULL DEFAULT 0,
    average_fill_price TEXT,
    rejection_reason TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_india_sandbox_orders_client_id ON india_sandbox_orders(client_order_id);
