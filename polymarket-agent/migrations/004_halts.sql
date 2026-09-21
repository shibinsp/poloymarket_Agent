-- Halts, so that stopping the agent survives restarting it.
--
-- The `HALT` file already persists across a restart, which is most of why it
-- exists. Nothing else did. A drawdown breaker that halts until a human
-- resumes, and then silently resumes itself because the operator restarted
-- the process to look at the logs, is not a breaker — and "restart it and see"
-- is the first thing anyone does.
--
-- One row per halt. `cleared_at` is NULL while it is in force, so the halt to
-- reinstate on startup is the newest row with a NULL there.
CREATE TABLE IF NOT EXISTS halts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- halt_file | api | signal | circuit_breaker | reconciliation
    source TEXT NOT NULL,
    -- rest_of_day clears itself at the UTC rollover; until_resume does not.
    scope TEXT NOT NULL CHECK (scope IN ('rest_of_day', 'until_resume')),
    detail TEXT,
    raised_at TEXT NOT NULL,
    -- The UTC day the halt was raised, as YYYY-MM-DD. Stored rather than
    -- derived from raised_at at read time: this is the value the rest-of-day
    -- expiry compares against, and re-deriving it on every comparison is how
    -- a timezone slip gets in.
    day TEXT NOT NULL,
    cleared_at TEXT,
    cleared_by TEXT,
    -- A halt reinstated after a restart carries its original timestamp, and
    -- the loop deliberately re-runs the side effects each time it starts —
    -- re-cancelling resting orders after a crash is worth doing. Without this
    -- constraint that also wrote a fresh row on every restart, so a flapping
    -- process turned one halt into a hundred rows and made the trail useless
    -- for the one question it exists to answer: when did this start?
    UNIQUE (source, raised_at)
);

CREATE INDEX IF NOT EXISTS idx_halts_active ON halts(cleared_at, raised_at);
