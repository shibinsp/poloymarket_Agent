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

# Only legal-form suffixes are stripped. Words such as "India" or "Corporation"
# are kept because removing them turns names into generic words ("Coal India" ->
# "Coal", "Life Insurance Corporation of India" -> "Life Insurance").
_LEGAL = re.compile(r"\b(limited|ltd\.?|the)\b|[.,]", re.IGNORECASE)
# Single-word names that are also ordinary words; only the ticker tags these.
_GENERIC_WORDS = {
    "eternal", "coal", "life", "state", "bank", "power", "steel", "cement", "energy",
    "gas", "oil", "motors", "industries", "finance", "info", "titan",
}
_TRAILING = {"co", "company", "corp", "corporation", "ltd", "limited"}

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
            # Tickers are matched case-sensitively: "ETERNAL" is a ticker, "eternal" is a word.
            self._patterns.append((re.compile(rf"(?<![\w&-]){re.escape(c.symbol)}(?![\w&-])"), c.symbol))
            for alias in _name_aliases(c):
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


def _name_aliases(c: Company) -> set[str]:
    """Company-name aliases: the name without its legal suffix, plus a shorter
    stem without a trailing "Industries" when that stem is
    still at least two words or a non-generic single word."""
    words = re.sub(r"\s+", " ", _LEGAL.sub(" ", c.name)).strip().split()
    while len(words) > 1 and words[-1].lower() in _TRAILING:
        words.pop()
    full = " ".join(words)
    aliases = {full} if len(full) >= 4 else set()
    if len(words) >= 2 and words[-1].lower() in {"industries", "inds"}:
        stem = " ".join(words[:-1])
        if len(stem.split()) >= 2 or (len(stem) >= 4 and stem.lower() not in _GENERIC_WORDS):
            aliases.add(stem)
    return {a for a in aliases if a.lower() not in _GENERIC_WORDS}


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
