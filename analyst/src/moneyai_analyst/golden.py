"""Golden-set evaluation for the classifier (DEV_PLAN.md section 5 quality targets).

Golden file: JSONL, one object per line::

    {"id": "...", "title": "...", "body": "...", "published_at": "2026-11-12T09:05:00+05:30",
     "symbols": ["RELIANCE"], "label": {"event_type": "results", "severity": 2}}

Items are split deterministically by id hash: 75% ``tune`` (look at these while
improving prompts), 25% ``holdout`` (score the final version once)."""

from __future__ import annotations

import asyncio
import hashlib
import json
import time
from dataclasses import dataclass
from datetime import date, datetime
from pathlib import Path

from .classify import KeywordClassifier, RejectedOutput
from .llm import LlmError
from .schema import Classification, EventType, SourceItem
from .sources import clean_text, parse_ts
from .universe import Universe


def split_of(item_id: str) -> str:
    return "holdout" if int(hashlib.sha256(item_id.encode()).hexdigest(), 16) % 4 == 0 else "tune"


@dataclass
class GoldenItem:
    item: SourceItem
    event_type: EventType
    severity: int


def load_golden(path: str | Path, universe: Universe, split: str, body_chars: int) -> list[GoldenItem]:
    out = []
    for line in Path(path).read_text().splitlines():
        if not line.strip():
            continue
        d = json.loads(line)
        if split != "all" and split_of(d["id"]) != split:
            continue
        title = clean_text(d["title"])
        body = clean_text(d.get("body", ""), body_chars)
        symbols = tuple(sorted(s for s in d.get("symbols", []) if s in universe)) or universe.tag(f"{title} {body}")
        item = SourceItem(
            id=d["id"], kind=d.get("kind", "news"), source="golden", title=title, body=body,
            published_at=parse_ts(d["published_at"]), symbols=symbols,
        )
        out.append(GoldenItem(item, EventType(d["label"]["event_type"]), int(d["label"]["severity"])))
    return out


@dataclass
class GoldenReport:
    n: int
    valid: int
    event_correct: int
    severity_correct: int
    exclusion_agree: int
    stable: int
    stability_checked: int
    p95_latency_s: float
    model_failures: int

    def as_dict(self) -> dict:
        n = max(self.n, 1)
        return {
            "items": self.n,
            "valid_schema_rate": round(self.valid / n, 4),
            "event_type_accuracy": round(self.event_correct / n, 4),
            "severity_accuracy": round(self.severity_correct / n, 4),
            "exclusion_decision_accuracy": round(self.exclusion_agree / n, 4),
            "repeat_stability": round(self.stable / max(self.stability_checked, 1), 4) if self.stability_checked else None,
            "p95_latency_s": round(self.p95_latency_s, 3),
            "model_failures": self.model_failures,
            "targets": {"valid_schema_rate": 0.99, "event_type_accuracy": 0.90, "repeat_stability": 0.95, "p95_latency_s": 30},
        }


async def evaluate(golden: list[GoldenItem], classifier, today: date, exclude_severity: int = 2, repeat: bool = False) -> GoldenReport:
    async def run(g: GoldenItem, use_cache: bool) -> tuple[Classification | None, float, bool]:
        t0 = time.perf_counter()
        try:
            if isinstance(classifier, KeywordClassifier):
                c = classifier.classify(g.item)
            else:
                c = await classifier.classify(g.item, [], today, use_cache=use_cache)
            return c, time.perf_counter() - t0, False
        except RejectedOutput:
            return None, time.perf_counter() - t0, False
        except LlmError:
            return None, time.perf_counter() - t0, True

    first = await asyncio.gather(*(run(g, True) for g in golden))
    second = await asyncio.gather(*(run(g, False) for g in golden)) if repeat else []
    valid = ev = sev = excl = 0
    failures = 0
    lat = sorted(t for _, t, _ in first)
    for g, (c, _, failed) in zip(golden, first):
        failures += failed
        if c is None:
            continue
        valid += 1
        ev += c.event_type == g.event_type
        sev += c.severity == g.severity
        excl += (c.severity >= exclude_severity) == (g.severity >= exclude_severity)
    stable = checked = 0
    for (a, _, _), (b, _, _) in zip(first, second):
        if a is not None and b is not None:
            checked += 1
            stable += (a.event_type, a.severity) == (b.event_type, b.severity)
    p95 = lat[min(len(lat) - 1, int(0.95 * len(lat)))] if lat else 0.0
    return GoldenReport(len(golden), valid, ev, sev, excl, stable, checked, p95, failures)
