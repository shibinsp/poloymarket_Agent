-- US Paper Trading Audit Trail
--
-- Records simulated paper execution orders for US equities (SPY, QQQ, AAPL, etc.)
-- keeping them strictly segregated from live trading.

CREATE TABLE IF NOT EXISTS us_paper_orders (
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

CREATE INDEX IF NOT EXISTS idx_us_paper_orders_client_id ON us_paper_orders(client_order_id);
