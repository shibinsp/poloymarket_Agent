"""News agent input: RSS feeds (Economic Times, Moneycontrol, NSE/BSE feeds...)."""

from __future__ import annotations

import calendar
import json
from datetime import UTC, datetime
from pathlib import Path

import feedparser
import httpx

from ..schema import SourceItem
from ..universe import Universe
from . import clean_text, parse_ts, stable_id


def parse_feed(raw: bytes | str, feed_url: str, universe: Universe, body_chars: int) -> list[SourceItem]:
    parsed = feedparser.parse(raw)
    items: list[SourceItem] = []
    for e in parsed.entries:
        title = clean_text(e.get("title", ""))
        if not title:
            continue
        body = clean_text(e.get("summary", "") or e.get("description", ""), body_chars)
        link = e.get("link")
        ts = e.get("published_parsed") or e.get("updated_parsed")
        published = datetime.fromtimestamp(calendar.timegm(ts), UTC) if ts else datetime.now(UTC)
        items.append(
            SourceItem(
                id=stable_id("news", link or "", title),
                kind="news",
                source=feed_url,
                title=title,
                body=body,
                url=link,
                published_at=published,
                symbols=universe.tag(f"{title} {body}"),
            )
        )
    return items


def fetch_rss(urls: list[str], universe: Universe, body_chars: int, timeout_s: float) -> tuple[list[SourceItem], list[str]]:
    items: list[SourceItem] = []
    errors: list[str] = []
    for url in urls:
        try:
            resp = httpx.get(url, timeout=timeout_s, headers={"User-Agent": "Mozilla/5.0"}, follow_redirects=True)
            resp.raise_for_status()
            items.extend(parse_feed(resp.content, url, universe, body_chars))
        except Exception as exc:  # one bad feed must not sink the run
            errors.append(f"rss {url}: {exc}")
    return items, errors


def load_jsonl(path: str | Path, universe: Universe, body_chars: int) -> list[SourceItem]:
    """Replay / golden-set input: one JSON object per line with
    id, title, body, published_at, source[, url, symbols]."""
    items = []
    for line in Path(path).read_text().splitlines():
        if not line.strip():
            continue
        d = json.loads(line)
        title = clean_text(d["title"])
        body = clean_text(d.get("body", ""), body_chars)
        symbols = tuple(sorted(s for s in d.get("symbols", []) if s in universe)) or universe.tag(f"{title} {body}")
        items.append(
            SourceItem(
                id=d.get("id") or stable_id("news", d.get("url", ""), title),
                kind=d.get("kind", "news"),
                source=d.get("source", "jsonl"),
                title=title,
                body=body,
                url=d.get("url"),
                published_at=parse_ts(d["published_at"]),
                symbols=symbols,
            )
        )
    return items
