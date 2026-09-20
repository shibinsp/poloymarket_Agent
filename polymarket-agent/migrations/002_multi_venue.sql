-- Multi-venue support.
--
-- SQLite cannot alter a CHECK constraint, and `trades.direction` is pinned to
-- ('YES','NO') while `status` has no CLOSED state — neither of which describes
-- an equity or crypto trade. So the table is rebuilt and the existing rows are
-- carried across.
--
-- Legacy rows are all Polymarket, and in that model every entry was a *buy* of
-- the named outcome token, so `direction` maps to side='BUY' with the outcome
-- preserved in `symbol` as '{market_id}:{YES|NO}' — the same instrument symbol
-- the venue adapter now produces.

CREATE TABLE IF NOT EXISTS trades_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cycle INTEGER NOT NULL,
    venue_id TEXT NOT NULL DEFAULT 'polymarket',
    market_id TEXT NOT NULL,
    symbol TEXT NOT NULL,
    asset_class TEXT NOT NULL DEFAULT 'prediction_binary'
        CHECK (asset_class IN ('prediction_binary', 'crypto_spot', 'equity')),
    market_question TEXT,
    -- Kept for backwards compatibility with existing readers; new code uses side.
    direction TEXT NOT NULL,
    side TEXT NOT NULL DEFAULT 'BUY' CHECK (side IN ('BUY', 'SELL')),
    entry_price TEXT NOT NULL,
    size TEXT NOT NULL,
    quantity TEXT,
    avg_fill_price TEXT,
    edge_at_entry TEXT NOT NULL,
    claude_fair_value TEXT NOT NULL,
    confidence TEXT NOT NULL,
    kelly_raw TEXT NOT NULL,
    kelly_adjusted TEXT NOT NULL,
    -- PENDING/PARTIAL exist because an order id is not a fill.
    status TEXT DEFAULT 'OPEN' CHECK (status IN (
        'PENDING', 'PARTIAL', 'OPEN', 'FILLED', 'CLOSED',
        'RESOLVED_WIN', 'RESOLVED_LOSS', 'CANCELLED'
    )),
    pnl TEXT,
    realized_pnl TEXT,
    fees TEXT,
    mark_price TEXT,
    unrealized_pnl TEXT,
    marked_at TEXT,
    close_reason TEXT,
    closed_at TEXT,
    venue_order_id TEXT,
    client_order_id TEXT,
    exit_order_id TEXT,
    stop_price TEXT,
    target_price TEXT,
    horizon_hours INTEGER,
    max_hold_until TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    resolved_at TEXT
);

INSERT INTO trades_new (
    id, cycle, venue_id, market_id, symbol, asset_class, market_question,
    direction, side, entry_price, size, quantity, avg_fill_price,
    edge_at_entry, claude_fair_value, confidence, kelly_raw, kelly_adjusted,
    status, pnl, realized_pnl, created_at, resolved_at
)
SELECT
    id,
    cycle,
    'polymarket',
    market_id,
    market_id || ':' || direction,
    'prediction_binary',
    market_question,
    direction,
    'BUY',
    entry_price,
    size,
    size,
    entry_price,
    edge_at_entry,
    claude_fair_value,
    confidence,
    kelly_raw,
    kelly_adjusted,
    status,
    pnl,
    pnl,
    created_at,
    resolved_at
FROM trades;

DROP TABLE trades;
ALTER TABLE trades_new RENAME TO trades;

CREATE INDEX IF NOT EXISTS idx_trades_status ON trades(status);
CREATE INDEX IF NOT EXISTS idx_trades_market_id ON trades(market_id);
CREATE INDEX IF NOT EXISTS idx_trades_venue_symbol ON trades(venue_id, symbol);
CREATE INDEX IF NOT EXISTS idx_trades_client_order_id ON trades(client_order_id);

-- Orders are tracked separately from trades: an order exists from the moment
-- it is submitted, whereas a trade only exists once something fills. Recording
-- them in one row is what let "the venue returned an id" masquerade as a fill.
CREATE TABLE IF NOT EXISTS orders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    client_order_id TEXT NOT NULL UNIQUE,
    venue_order_id TEXT,
    venue_id TEXT NOT NULL,
    symbol TEXT NOT NULL,
    side TEXT NOT NULL CHECK (side IN ('BUY', 'SELL')),
    intent TEXT NOT NULL DEFAULT 'ENTRY' CHECK (intent IN ('ENTRY', 'EXIT')),
    trade_id INTEGER REFERENCES trades(id),
    limit_price TEXT,
    qty TEXT NOT NULL,
    filled_qty TEXT NOT NULL DEFAULT '0',
    avg_fill_price TEXT,
    -- UNKNOWN is a real state: a timed-out request must be resolved by
    -- querying the venue before any retry, or the retry double-places.
    state TEXT NOT NULL DEFAULT 'PENDING' CHECK (state IN (
        'PENDING', 'ACCEPTED', 'PARTIALLY_FILLED', 'FILLED',
        'CANCELLED', 'EXPIRED', 'REJECTED', 'UNKNOWN'
    )),
    reject_reason TEXT,
    cycle INTEGER,
    submitted_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT,
    expires_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_orders_state ON orders(state);
CREATE INDEX IF NOT EXISTS idx_orders_venue_symbol ON orders(venue_id, symbol);

-- One row per execution, so slippage and time-to-fill can be measured rather
-- than assumed.
CREATE TABLE IF NOT EXISTS fills (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id INTEGER NOT NULL REFERENCES orders(id),
    venue_trade_id TEXT,
    qty TEXT NOT NULL,
    price TEXT NOT NULL,
    fee TEXT,
    mid_at_submit TEXT,
    slippage_bps TEXT,
    time_to_fill_ms INTEGER,
    filled_at TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_fills_order_id ON fills(order_id);

-- Start-of-day equity and high-water mark, for drawdown and daily-loss limits.
CREATE TABLE IF NOT EXISTS daily_equity (
    day TEXT PRIMARY KEY,
    starting_equity TEXT NOT NULL,
    high_water_mark TEXT NOT NULL,
    closing_equity TEXT,
    realized_pnl TEXT,
    updated_at TEXT DEFAULT (datetime('now'))
);

-- Every reconciliation run, so a mismatch leaves a trail.
CREATE TABLE IF NOT EXISTS reconciliation_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    venue_id TEXT NOT NULL,
    cycle INTEGER,
    balance_delta TEXT,
    positions_missing_locally INTEGER NOT NULL DEFAULT 0,
    positions_missing_on_venue INTEGER NOT NULL DEFAULT 0,
    qty_mismatches INTEGER NOT NULL DEFAULT 0,
    unknown_open_orders INTEGER NOT NULL DEFAULT 0,
    passed BOOLEAN NOT NULL,
    detail TEXT,
    created_at TEXT DEFAULT (datetime('now'))
);

-- Valuation cache keyed by venue+symbol+horizon rather than condition_id, so
-- two venues' "BTC" cannot collide.
CREATE TABLE IF NOT EXISTS valuation_cache_v2 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    venue_id TEXT NOT NULL,
    symbol TEXT NOT NULL,
    horizon_hours INTEGER NOT NULL DEFAULT 0,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    cached_at TEXT DEFAULT (datetime('now')),
    UNIQUE (venue_id, symbol, horizon_hours)
);

-- Calibration gains venue/asset-class context plus the fields needed to score
-- a continuous-asset forecast, where "was it on the right side of 0.5" is not
-- enough.
ALTER TABLE confidence_calibration ADD COLUMN venue_id TEXT DEFAULT 'polymarket';
ALTER TABLE confidence_calibration ADD COLUMN symbol TEXT;
ALTER TABLE confidence_calibration ADD COLUMN asset_class TEXT DEFAULT 'prediction_binary';
ALTER TABLE confidence_calibration ADD COLUMN p_up TEXT;
ALTER TABLE confidence_calibration ADD COLUMN predicted_return TEXT;
ALTER TABLE confidence_calibration ADD COLUMN realized_return TEXT;
ALTER TABLE confidence_calibration ADD COLUMN brier TEXT;
