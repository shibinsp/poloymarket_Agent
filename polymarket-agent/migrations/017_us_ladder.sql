-- Capital ladder for the US books (9 Nov 2026): each real US trade, once
-- closed, re-sized at every starting amount. units = shares (fractional),
-- contracts or spreads; 0 means the amount could not pay for one.
CREATE TABLE IF NOT EXISTS us_ladder_trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    book TEXT NOT NULL,
    amount INTEGER NOT NULL,
    source_id INTEGER NOT NULL,
    day TEXT NOT NULL,
    symbol TEXT NOT NULL,
    units TEXT NOT NULL,
    pnl TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE (book, amount, source_id)
);
CREATE INDEX IF NOT EXISTS idx_us_ladder_day ON us_ladder_trades(day);
