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

CREATE TABLE IF NOT EXISTS confidence_calibration (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    market_id TEXT NOT NULL,
    claude_confidence TEXT NOT NULL,
    fair_value TEXT NOT NULL,
    market_price_at_entry TEXT NOT NULL,
    actual_outcome TEXT,
    forecast_correct BOOLEAN,
    resolved BOOLEAN NOT NULL DEFAULT 0,
    created_at TEXT DEFAULT (datetime('now')),
    resolved_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_calibration_resolved ON confidence_calibration(resolved);
CREATE INDEX IF NOT EXISTS idx_calibration_market ON confidence_calibration(market_id);

CREATE TABLE IF NOT EXISTS valuation_cache (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    condition_id TEXT NOT NULL UNIQUE,
    probability TEXT NOT NULL,
    confidence TEXT NOT NULL,
    reasoning_summary TEXT,
    key_factors TEXT,
    data_quality TEXT NOT NULL,
    time_sensitivity TEXT NOT NULL,
    cached_at TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_valuation_cache_condition ON valuation_cache(condition_id);
