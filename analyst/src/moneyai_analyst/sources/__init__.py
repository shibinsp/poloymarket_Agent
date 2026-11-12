"""Evidence sources. Every source returns ``SourceItem`` records with stable ids,
so every claim the analyst publishes can be traced back to one."""

import hashlib
import html
import re
from datetime import datetime
from zoneinfo import ZoneInfo

IST = ZoneInfo("Asia/Kolkata")

_TAG = re.compile(r"<[^>]+>")
_WS = re.compile(r"\s+")


def clean_text(text: str, limit: int | None = None) -> str:
    """Strip markup and collapse whitespace. News is untrusted input."""
    text = html.unescape(_TAG.sub(" ", text or ""))
    text = _WS.sub(" ", text).strip()
    return text[:limit] if limit else text


def stable_id(prefix: str, *parts: str) -> str:
    h = hashlib.sha256("\x1f".join(parts).encode()).hexdigest()[:16]
    return f"{prefix}:{h}"


def parse_ts(value: str) -> datetime:
    """ISO timestamp; a naive value is taken to be IST."""
    ts = datetime.fromisoformat(value)
    return ts if ts.tzinfo else ts.replace(tzinfo=IST)
