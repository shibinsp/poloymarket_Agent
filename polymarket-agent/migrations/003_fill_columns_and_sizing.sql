-- Two corrections to 002, as a new migration rather than an edit to it.
--
-- 002 is already applied on every database built from master, and
-- `sqlx::migrate!` validates checksums, so changing it in place makes the
-- agent refuse to start with "migration 2 was previously applied but has been
-- modified". Once a migration has shipped, the only honest way to fix it is a
-- new one.

-- 1. 002's backfill copied `size` into `quantity` and `entry_price` into
--    `avg_fill_price` for every carried-over row. Those columns mean "what
--    actually filled", and for these rows nothing ever did: the old execution
--    path marked a trade Filled the moment an order id came back. Asserting a
--    fill that was never confirmed is the exact thing 002 splits the columns
--    apart to stop doing, and it makes slippage over historical rows measure
--    zero by construction rather than reporting itself as unknown.
--
--    Scoped to prediction_binary, which is every row 002 carried across and
--    every row the legacy path writes. Venue trades record a real ack and must
--    not be touched.
UPDATE trades
SET quantity = NULL,
    avg_fill_price = NULL
WHERE asset_class = 'prediction_binary';

-- 2. Continuous assets are not sized by Kelly — they use ATR volatility
--    targeting — so there was nowhere to record how they *were* sized, and the
--    risk fraction and stop distance were being written into `kelly_raw` and
--    `kelly_adjusted`. That silently reports a Kelly fraction to anything
--    reading those columns, including /api/trades, and is unrecoverable
--    afterwards because nothing records which rows meant which.
ALTER TABLE trades ADD COLUMN risk_pct TEXT;
ALTER TABLE trades ADD COLUMN stop_pct TEXT;
