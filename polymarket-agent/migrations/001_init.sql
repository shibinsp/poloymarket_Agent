CREATE TABLE IF NOT EXISTS trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cycle INTEGER NOT NULL,
    market_id TEXT NOT NULL,
    market_question TEXT,
    direction TEXT NOT NULL CHECK (direction IN ('YES', 'NO')),
    entry_price TEXT NOT NULL,
    size TEXT NOT NULL,
    edge_at_entry TEXT NOT NULL,
    claude_fair_value TEXT NOT NULL,
    confidence TEXT NOT NULL,
    kelly_raw TEXT NOT NULL,
    kelly_adjusted TEXT NOT NULL,
    status TEXT DEFAULT 'OPEN' CHECK (status IN ('OPEN', 'FILLED', 'RESOLVED_WIN', 'RESOLVED_LOSS', 'CANCELLED')),
    pnl TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    resolved_at TEXT
);

CREATE TABLE IF NOT EXISTS cycles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cycle_number INTEGER NOT NULL UNIQUE,
    markets_scanned INTEGER,
    opportunities_found INTEGER,
    trades_placed INTEGER,
    api_cost TEXT,
    bankroll TEXT,
    unrealized_pnl TEXT,
    agent_state TEXT NOT NULL,
    duration_ms INTEGER,
    created_at TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS api_costs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider TEXT NOT NULL,
    endpoint TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cost TEXT NOT NULL,
    cycle INTEGER,
    created_at TEXT DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_trades_status ON trades(status);
CREATE INDEX IF NOT EXISTS idx_trades_market_id ON trades(market_id);
CREATE INDEX IF NOT EXISTS idx_cycles_cycle_number ON cycles(cycle_number);
CREATE INDEX IF NOT EXISTS idx_api_costs_provider ON api_costs(provider);
CREATE INDEX IF NOT EXISTS idx_api_costs_cycle ON api_costs(cycle);
