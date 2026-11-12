"""Filings RAG: retrieve the most relevant passages from a company's recent
filings and transcripts to give the classifier background.

BM25 keeps it dependency-free and deterministic. A vector store can replace
``retrieve`` later if golden-set accuracy shows it helps."""

from __future__ import annotations

import math
import re
from collections import Counter
from datetime import datetime
from pathlib import Path

from .schema import SourceItem
from .sources import clean_text, stable_id
from .store import Store

_TOKEN = re.compile(r"[a-z0-9]+")
_STOP = set("the a an of and or to in for on with by at from is are was were be as that this it its".split())


def tokenize(text: str) -> list[str]:
    return [t for t in _TOKEN.findall(text.lower()) if t not in _STOP and len(t) > 1]


def chunk(text: str, size: int = 800, overlap: int = 100) -> list[str]:
    text = clean_text(text)
    if len(text) <= size:
        return [text] if text else []
    out, i = [], 0
    while i < len(text):
        out.append(text[i : i + size])
        i += size - overlap
    return out


def bm25_rank(query: str, docs: list[str], k1: float = 1.5, b: float = 0.75) -> list[tuple[int, float]]:
    q = tokenize(query)
    toks = [tokenize(d) for d in docs]
    if not q or not docs:
        return []
    n = len(docs)
    avgdl = sum(len(t) for t in toks) / n or 1.0
    df = Counter(term for t in toks for term in set(t))
    scores = []
    for i, t in enumerate(toks):
        tf = Counter(t)
        s = 0.0
        for term in q:
            if term not in tf:
                continue
            idf = math.log(1 + (n - df[term] + 0.5) / (df[term] + 0.5))
            s += idf * tf[term] * (k1 + 1) / (tf[term] + k1 * (1 - b + b * len(t) / avgdl))
        scores.append((i, s))
    return sorted((x for x in scores if x[1] > 0), key=lambda x: -x[1])


def retrieve(store: Store, symbol: str, query: str, k: int = 2, chars: int = 600) -> list[SourceItem]:
    passages: list[tuple[str, str, str, int, str]] = []
    for doc_id, title, text, published in store.documents_for(symbol):
        for j, c in enumerate(chunk(text)):
            passages.append((doc_id, title, published, j, c))
    ranked = bm25_rank(query, [p[4] for p in passages])[:k]
    out = []
    for idx, _score in ranked:
        doc_id, title, published, j, c = passages[idx]
        out.append(
            SourceItem(
                id=f"{doc_id}#{j}",
                kind="filing",
                source="rag",
                title=title,
                body=c[:chars],
                published_at=datetime.fromisoformat(published),
                symbols=(symbol,),
            )
        )
    return out


def ingest_file(store: Store, path: Path, symbol: str, title: str, published_at: datetime) -> str:
    """Add a text document (results release, concall transcript) for ``symbol``.
    Convert PDFs to text first (e.g. ``pdftotext``)."""
    text = path.read_text(errors="ignore")
    doc_id = stable_id("doc", symbol, title, published_at.isoformat())
    store.add_document(doc_id, symbol, title, text, str(path), published_at)
    return doc_id
