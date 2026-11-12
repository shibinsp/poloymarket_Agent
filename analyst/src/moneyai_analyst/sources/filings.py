"""Filings agent input: BSE corporate announcements, a corporate-events calendar,
and operator-ingested documents (concall transcripts, results text) for RAG."""

from __future__ import annotations

import csv
from datetime import UTC, date, datetime, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

import httpx

from ..schema import SourceItem
from ..universe import Universe
from . import clean_text, stable_id

IST = ZoneInfo("Asia/Kolkata")
BSE_ANN_URL = "https://api.bseindia.com/BseIndiaAPI/api/AnnSubCategoryGetData/w"


def parse_bse_announcements(payload: dict, universe: Universe, body_chars: int) -> list[SourceItem]:
    items = []
    for row in payload.get("Table", []) or []:
        title = clean_text(row.get("NEWSSUB") or row.get("HEADLINE") or "")
        if not title:
            continue
        company = clean_text(row.get("SLONGNAME") or "")
        body = clean_text(row.get("HEADLINE") or "", body_chars)
        ts_raw = row.get("NEWS_DT") or row.get("DissemDT")
        try:
            published = datetime.fromisoformat(ts_raw).replace(tzinfo=IST) if ts_raw else datetime.now(UTC)
        except ValueError:
            published = datetime.now(UTC)
        attach = row.get("ATTACHMENTNAME")
        url = f"https://www.bseindia.com/xml-data/corpfiling/AttachLive/{attach}" if attach else None
        items.append(
            SourceItem(
                id=stable_id("filing", str(row.get("NEWSID") or ""), title),
                kind="filing",
                source="bse_announcements",
                title=title,
                body=body,
                url=url,
                published_at=published,
                symbols=universe.tag(f"{company} {title}"),
            )
        )
    return items


def fetch_bse_announcements(universe: Universe, body_chars: int, timeout_s: float, today: date) -> list[SourceItem]:
    d = today.strftime("%Y%m%d")
    params = {"pageno": 1, "strCat": -1, "strPrevDate": d, "strScrip": "", "strSearch": "P", "strToDate": d, "strType": "C"}
    headers = {"User-Agent": "Mozilla/5.0", "Referer": "https://www.bseindia.com/", "Accept": "application/json"}
    resp = httpx.get(BSE_ANN_URL, params=params, headers=headers, timeout=timeout_s)
    resp.raise_for_status()
    return parse_bse_announcements(resp.json(), universe, body_chars)


def load_events_calendar(path: str | Path, universe: Universe, today: date) -> list[SourceItem]:
    """Operator-maintained calendar: ``symbol,date,event_type,description``.

    Only today's events are returned. They become hard exclusions (results day,
    ex-date, record date), which is where scheduled news risk is highest."""
    items = []
    with open(path, newline="") as fh:
        for row in csv.DictReader(fh):
            sym = (row.get("symbol") or "").strip().upper()
            if sym not in universe:
                continue
            try:
                d = date.fromisoformat((row.get("date") or "").strip())
            except ValueError:
                continue
            if d != today:
                continue
            etype = (row.get("event_type") or "other").strip().lower()
            desc = clean_text(row.get("description") or etype)
            start = datetime.combine(d, datetime.min.time(), IST)
            items.append(
                SourceItem(
                    id=stable_id("calendar", sym, d.isoformat(), etype),
                    kind="calendar",
                    source=str(path),
                    title=f"{sym}: {etype} on {d.isoformat()}",
                    body=desc,
                    published_at=start - timedelta(days=1),
                    symbols=(sym,),
                )
            )
    return items
