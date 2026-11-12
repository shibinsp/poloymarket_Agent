"""SQLite state: audit of every run, the LLM cache and the filings corpus.

This is the analyst's own database (``state/analyst.db``). It is append-only for
runs and classifications; the engine's database is only ever read."""

from __future__ import annotations

import json
import os
import sqlite3
import tempfile
from datetime import datetime
from pathlib import Path

from .schema import AnalystContext, Classification

SCHEMA = """
CREATE TABLE IF NOT EXISTS runs (
    run_id TEXT PRIMARY KEY,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    status TEXT NOT NULL,              -- published | fallback
    model TEXT, prompt_version TEXT, schema_version TEXT,
    n_items INTEGER, n_classified INTEGER, n_rejected INTEGER,
    errors TEXT,                       -- JSON list
    context TEXT                       -- JSON AnalystContext, NULL on fallback
);
CREATE TABLE IF NOT EXISTS classifications (
    run_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    title TEXT, source TEXT, url TEXT,
    result TEXT NOT NULL,              -- JSON Classification
    PRIMARY KEY (run_id, item_id)
);
CREATE TABLE IF NOT EXISTS llm_cache (
    key TEXT PRIMARY KEY,
    response TEXT NOT NULL,
    model TEXT, prompt_version TEXT,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS documents (
    id TEXT PRIMARY KEY,
    symbol TEXT NOT NULL,
    title TEXT NOT NULL,
    text TEXT NOT NULL,
    source TEXT,
    published_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_documents_symbol ON documents(symbol, published_at);
"""


class Store:
    def __init__(self, state_dir: Path):
        state_dir.mkdir(parents=True, exist_ok=True)
        self.state_dir = state_dir
        self.db = sqlite3.connect(state_dir / "analyst.db")
        self.db.executescript(SCHEMA)

    # -- LLM cache -----------------------------------------------------------
    def cache_get(self, key: str) -> str | None:
        row = self.db.execute("SELECT response FROM llm_cache WHERE key = ?", (key,)).fetchone()
        return row[0] if row else None

    def cache_put(self, key: str, response: str, model: str, prompt_version: str) -> None:
        self.db.execute(
            "INSERT OR REPLACE INTO llm_cache VALUES (?, ?, ?, ?, ?)",
            (key, response, model, prompt_version, datetime.now().isoformat()),
        )
        self.db.commit()

    # -- documents (RAG corpus) ------------------------------------------------
    def add_document(self, doc_id: str, symbol: str, title: str, text: str, source: str, published_at: datetime) -> None:
        self.db.execute(
            "INSERT OR REPLACE INTO documents VALUES (?, ?, ?, ?, ?, ?)",
            (doc_id, symbol, title, text, source, published_at.isoformat()),
        )
        self.db.commit()

    def documents_for(self, symbol: str, limit: int = 20) -> list[tuple[str, str, str, str]]:
        return self.db.execute(
            "SELECT id, title, text, published_at FROM documents WHERE symbol = ? ORDER BY published_at DESC LIMIT ?",
            (symbol, limit),
        ).fetchall()

    # -- runs --------------------------------------------------------------------
    def record_run(
        self,
        run_id: str,
        started_at: datetime,
        status: str,
        model: str,
        prompt_version: str,
        schema_version: str,
        n_items: int,
        classifications: list[tuple[Classification, dict]],
        n_rejected: int,
        errors: list[str],
        context: AnalystContext | None,
    ) -> None:
        self.db.execute(
            "INSERT INTO runs VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                run_id, started_at.isoformat(), datetime.now(started_at.tzinfo).isoformat(), status,
                model, prompt_version, schema_version, n_items, len(classifications), n_rejected,
                json.dumps(errors), context.model_dump_json() if context else None,
            ),
        )
        self.db.executemany(
            "INSERT INTO classifications VALUES (?, ?, ?, ?, ?, ?)",
            [(run_id, c.item_id, meta.get("title"), meta.get("source"), meta.get("url"), c.model_dump_json())
             for c, meta in classifications],
        )
        self.db.commit()

    def contexts(self) -> list[AnalystContext]:
        rows = self.db.execute("SELECT context FROM runs WHERE context IS NOT NULL ORDER BY started_at").fetchall()
        return [AnalystContext.model_validate_json(r[0]) for r in rows]


def publish_context(state_dir: Path, ctx: AnalystContext) -> Path:
    """Atomically write ``context.json`` for the engine to read."""
    target = state_dir / "context.json"
    fd, tmp = tempfile.mkstemp(dir=state_dir, prefix=".context.", suffix=".json")
    with os.fdopen(fd, "w") as fh:
        fh.write(ctx.model_dump_json(indent=2))
    os.replace(tmp, target)
    return target


def load_active_context(path: Path, now: datetime) -> AnalystContext | None:
    """Reference reader for consumers: invalid or expired context means 'no context'."""
    try:
        ctx = AnalystContext.model_validate_json(path.read_text())
    except Exception:
        return None
    return ctx if ctx.is_active(now) else None
