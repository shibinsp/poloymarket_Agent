-- The trading desk's decision cards: one per signal, with every agent's report.
CREATE TABLE IF NOT EXISTS desk_decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at TEXT NOT NULL,
    market TEXT NOT NULL,
    strategy TEXT NOT NULL,
    symbol TEXT NOT NULL,
    action TEXT NOT NULL,
    advisory_action TEXT NOT NULL,
    card TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_desk_decisions_created ON desk_decisions(created_at);
