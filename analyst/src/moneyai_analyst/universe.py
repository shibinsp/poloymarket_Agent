"""Nifty 100 universe and company-name tagging.

The constituents list is the NSE Indices CSV (columns include ``Company Name``,
``Symbol``, ``Industry``). It is downloaded once a day and cached; with no cache
and no network the pipeline fails closed (publishes nothing).
"""

from __future__ import annotations

import csv
import io
import re
from dataclasses import dataclass
from datetime import date
from pathlib import Path

import httpx

_SUFFIXES = re.compile(
    r"\b(limited|ltd\.?|corporation|corp\.?|company|co\.?|india|of|the|and|&|\(india\))\b",
    re.IGNORECASE,
)
# Short or generic name stems that would tag unrelated headlines.
_STOP_ALIASES = {"state", "bank", "power", "life", "steel", "motors", "industries", "finance"}


@dataclass(frozen=True)
class Company:
    symbol: str
    name: str
    industry: str = ""


class Universe:
    def __init__(self, companies: list[Company]):
        if not companies:
            raise ValueError("empty universe")
        self.companies = {c.symbol: c for c in companies}
        self._patterns: list[tuple[re.Pattern[str], str]] = []
        for c in companies:
            for alias in _aliases(c):
                self._patterns.append((re.compile(rf"(?<![\w&]){re.escape(alias)}(?![\w&])", re.IGNORECASE), c.symbol))

    @property
    def symbols(self) -> set[str]:
        return set(self.companies)

    def __contains__(self, symbol: str) -> bool:
        return symbol in self.companies

    def tag(self, text: str) -> tuple[str, ...]:
        """Return the universe symbols mentioned in ``text``, in a stable order."""
        found = {sym for pat, sym in self._patterns if pat.search(text)}
        return tuple(sorted(found))

    @classmethod
    def from_csv_text(cls, text: str) -> "Universe":
        rows = csv.DictReader(io.StringIO(text.lstrip("﻿")))
        companies = []
        for r in rows:
            sym = (r.get("Symbol") or "").strip().upper()
            name = (r.get("Company Name") or "").strip()
            if sym and name:
                companies.append(Company(sym, name, (r.get("Industry") or "").strip()))
        return cls(companies)


def _aliases(c: Company) -> set[str]:
    aliases = {c.symbol}
    stem = _SUFFIXES.sub(" ", c.name)
    stem = re.sub(r"\s+", " ", stem).strip(" .,-")
    if len(stem) >= 4 and stem.lower() not in _STOP_ALIASES:
        aliases.add(stem)
    full = c.name.replace(" Ltd.", "").replace(" Limited", "").strip()
    if len(full) >= 4:
        aliases.add(full)
    # Bare symbols shorter than 3 characters ("LT") are too noisy outside ticker context.
    return {a for a in aliases if len(a) >= 3}


def load_universe(cache_dir: Path, csv_path: str | None, url: str, timeout_s: float, today: date) -> Universe:
    if csv_path:
        return Universe.from_csv_text(Path(csv_path).read_text())
    cache_dir.mkdir(parents=True, exist_ok=True)
    cached = cache_dir / f"nifty100_{today.isoformat()}.csv"
    if cached.exists():
        return Universe.from_csv_text(cached.read_text())
    try:
        resp = httpx.get(url, timeout=timeout_s, headers={"User-Agent": "Mozilla/5.0"}, follow_redirects=True)
        resp.raise_for_status()
        uni = Universe.from_csv_text(resp.text)
        cached.write_text(resp.text)
        return uni
    except Exception:
        # Fall back to the most recent cached copy, never to a hardcoded list.
        older = sorted(cache_dir.glob("nifty100_*.csv"))
        if older:
            return Universe.from_csv_text(older[-1].read_text())
        raise
