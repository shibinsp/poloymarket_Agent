-- The post-mortem of each closed strategy trade: how far it went for and
-- against us, what the index did meanwhile, and the one failure it is filed
-- under. Written once per trade, after it closes; see src/review.rs.
CREATE TABLE IF NOT EXISTS trade_reviews (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    market TEXT NOT NULL,            -- 'IN' or 'US'
    strategy TEXT NOT NULL,
    trade_ref INTEGER NOT NULL,      -- india_intraday_trades.id or trades.id
    symbol TEXT NOT NULL,
    side TEXT NOT NULL,              -- 'LONG' or 'SHORT'
    day TEXT NOT NULL,               -- the market's local session date
    entry_at TEXT NOT NULL,
    exit_at TEXT NOT NULL,
    entry_price TEXT NOT NULL,
    stop_price TEXT NOT NULL,
    exit_price TEXT NOT NULL,
    exit_reason TEXT,
    pnl TEXT NOT NULL,
    charges TEXT,
    minutes_held INTEGER NOT NULL,
    mfe_r TEXT,                      -- best move in our favour, in units of risk
    mae_r TEXT,                      -- worst move against us, in units of risk
    index_move_pct TEXT,             -- the index over the trade, + = our way
    label TEXT NOT NULL,
    lesson TEXT NOT NULL,
    reviewed_at TEXT NOT NULL,
    UNIQUE (market, strategy, trade_ref)
);
CREATE INDEX IF NOT EXISTS idx_trade_reviews_day ON trade_reviews(day);
