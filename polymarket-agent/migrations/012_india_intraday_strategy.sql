-- More than one NSE intraday strategy can run at once, each its own book:
-- rows before this change all belong to the opening-range breakout.
ALTER TABLE india_intraday_trades ADD COLUMN strategy TEXT NOT NULL DEFAULT 'india_orb';
CREATE INDEX IF NOT EXISTS idx_india_intraday_strategy ON india_intraday_trades(strategy);
