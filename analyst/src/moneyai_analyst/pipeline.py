"""The five-step run: gather -> filter -> classify -> compose -> validate/publish.

Only step 3 may call a model. Any failure in steps 1, 4 or 5, or a classify step
that produces nothing usable, is a *fallback*: nothing new is published and the
engine keeps running on its default rules."""

from __future__ import annotations

import asyncio
import re
import uuid
from dataclasses import dataclass, field
from datetime import datetime, time, timedelta
from zoneinfo import ZoneInfo

from . import PROMPT_VERSION, SCHEMA_VERSION
from .classify import KeywordClassifier, LlmClassifier, RejectedOutput
from .compose import compose
from .config import Config
from .forecast import vol_band
from .llm import LlmClient, LlmError
from .rag import retrieve
from .regime import compute_regime
from .schema import AnalystContext, Classification, Regime, SourceItem, VolBand
from .sources import filings, market, news
from .store import Store, publish_context
from .universe import Universe, load_universe

IST = ZoneInfo("Asia/Kolkata")
LOOKBACK = timedelta(hours=18)
SESSION_CLOSE = time(15, 30)


def lookback_start(now: datetime) -> datetime:
    """Earlier of 18 hours ago and the previous weekday's close, so Monday's
    pre-market run still sees Saturday's results and Sunday's news.
    (Exchange holidays are not modelled; extend via an events calendar.)"""
    local = now.astimezone(IST)
    day = local.date() - timedelta(days=1)
    while day.weekday() >= 5:
        day -= timedelta(days=1)
    prev_close = datetime.combine(day, SESSION_CLOSE, IST)
    return min(now - LOOKBACK, prev_close)


@dataclass
class Gathered:
    items: list[SourceItem] = field(default_factory=list)
    regime: Regime = field(default_factory=Regime)
    vol_bands: list[VolBand] = field(default_factory=list)
    unusual_volume: dict[str, tuple[float, str]] = field(default_factory=dict)
    errors: list[str] = field(default_factory=list)


@dataclass
class RunResult:
    run_id: str
    status: str  # published | recorded (dry run) | fallback
    context: AnalystContext | None
    classifications: list[Classification]
    rejected: list[str]
    errors: list[str]


# -- step 1: gather ---------------------------------------------------------------


def gather(cfg: Config, universe: Universe, now: datetime, jsonl_inputs: list[str] | None = None) -> Gathered:
    g = Gathered()
    s = cfg.sources
    today = now.astimezone(IST).date()
    if s.rss_feeds:
        items, errs = news.fetch_rss(s.rss_feeds, universe, cfg.llm.body_chars, s.http_timeout_s)
        g.items += items
        g.errors += errs
    if s.bse_announcements:
        try:
            g.items += filings.fetch_bse_announcements(universe, cfg.llm.body_chars, s.http_timeout_s, today)
        except Exception as exc:
            g.errors.append(f"bse: {exc}")
    if s.events_calendar:
        try:
            g.items += filings.load_events_calendar(s.events_calendar, universe, today)
        except Exception as exc:
            g.errors.append(f"calendar: {exc}")
    for path in jsonl_inputs or []:
        g.items += news.load_jsonl(path, universe, cfg.llm.body_chars)
    if s.ignore_title_patterns:
        noise = re.compile("|".join(f"(?:{p})" for p in s.ignore_title_patterns), re.IGNORECASE)
        g.items = [i for i in g.items if not noise.search(i.title)]
    if s.market_data:
        _gather_market(cfg, universe, g, now)
    return g


def _gather_market(cfg: Config, universe: Universe, g: Gathered, now: datetime) -> None:
    local = now.astimezone(IST)
    session_open = local.time() < SESSION_CLOSE
    fii = dii = None
    as_of = None
    if cfg.sources.nse_fii_dii:
        try:
            fii, dii, as_of = market.fetch_fii_dii(cfg.sources.http_timeout_s)
        except Exception as exc:
            g.errors.append(f"fii_dii: {exc}")
    try:
        tickers = [market.VIX_TICKER, market.NIFTY_TICKER] + [market.yahoo_symbol(s) for s in sorted(universe.symbols)]
        frames = market.download_daily(tickers)
    except Exception as exc:
        g.errors.append(f"market data: {exc}")
        g.regime = compute_regime(None, None, fii, dii, as_of)
        return
    frames = {t: market.completed_sessions(df, local.date(), session_open) for t, df in frames.items()}
    g.regime = compute_regime(frames.get(market.VIX_TICKER), frames.get(market.NIFTY_TICKER), fii, dii, as_of)
    for sym in sorted(universe.symbols):
        df = frames.get(market.yahoo_symbol(sym))
        if df is None:
            continue
        last = df.index[-1].date().isoformat()
        source_id = f"market:{sym}:{last}"
        band = vol_band(sym, df, source_id)
        if band:
            g.vol_bands.append(band)
        vol = df["Volume"].astype(float)
        avg = float(vol.iloc[-21:-1].mean())
        if avg > 0 and float(vol.iloc[-1]) / avg >= 2.0:
            g.unusual_volume[sym] = (round(float(vol.iloc[-1]) / avg, 2), source_id)


# -- step 2: filter -------------------------------------------------------------------

_NORM = re.compile(r"[^a-z0-9]+")


def prefilter(items: list[SourceItem], universe: Universe, now: datetime, max_items: int) -> list[SourceItem]:
    """De-duplicate, keep recent items about universe stocks, cap the count.
    Calendar items are always kept first: they are scheduled, certain risk."""
    seen_ids: set[str] = set()
    seen_titles: set[str] = set()
    calendar, rest = [], []
    for it in sorted(items, key=lambda i: i.published_at, reverse=True):
        norm = _NORM.sub(" ", it.title.lower()).strip()
        if it.id in seen_ids or norm in seen_titles:
            continue
        seen_ids.add(it.id)
        seen_titles.add(norm)
        if not it.symbols or not set(it.symbols) <= universe.symbols:
            continue
        if it.kind == "calendar":
            calendar.append(it)
        elif lookback_start(now) <= it.published_at <= now + timedelta(minutes=5):
            rest.append(it)
    return (calendar + rest)[:max_items] if max_items > len(calendar) else calendar


# -- step 3: classify ------------------------------------------------------------------


_BASELINE = KeywordClassifier()


async def classify_all(
    items: list[SourceItem], classifier, store: Store, today, use_cache: bool = True
) -> tuple[list[Classification], list[str], int]:
    """Returns (classifications, notes, number of items the model did not answer).

    * Calendar items are always classified in code: a scheduled results day or
      ex-date is an exclusion whatever a model thinks of it.
    * An item the model fails on or answers invalidly is classified by the keyword
      baseline instead, so a partial outage never publishes fewer restrictions
      than the offline baseline would."""
    if isinstance(classifier, KeywordClassifier):
        return [classifier.classify(i) for i in items], [], 0

    async def one(item: SourceItem):
        if item.kind == "calendar":
            return _BASELINE.classify(item), None
        try:
            background = retrieve(store, item.symbols[0], item.title) if len(item.symbols) == 1 else []
            return await classifier.classify(item, background, today, use_cache=use_cache), None
        except RejectedOutput as exc:
            note = f"{item.id}: rejected: {exc}"
        except LlmError as exc:
            note = f"{item.id}: model failed: {exc}"
        except Exception as exc:  # one malformed answer or bad document must not sink the run
            note = f"{item.id}: error: {type(exc).__name__}: {exc}"
        fallback = _BASELINE.classify(item).model_copy(update={"classifier": f"{_BASELINE.name} (fallback)"})
        return fallback, f"{note}; used keyword baseline"

    results = await asyncio.gather(*(one(i) for i in items))
    notes = [n for _, n in results if n]
    return [c for c, _ in results], notes, len(notes)


# -- the run -----------------------------------------------------------------------------


def make_classifier(cfg: Config, store: Store):
    if cfg.llm.provider == "none":
        return KeywordClassifier()
    client = LlmClient(cfg.llm, cfg.api_key())
    return LlmClassifier(client, store, cfg.llm.max_tokens_classify, cfg.llm.concurrency)


async def run_once(
    cfg: Config,
    now: datetime | None = None,
    universe: Universe | None = None,
    classifier=None,
    jsonl_inputs: list[str] | None = None,
    publish: bool = True,
) -> RunResult:
    now = now or datetime.now(IST)
    run_id = f"{now.astimezone(IST).strftime('%Y%m%dT%H%M%S')}-{uuid.uuid4().hex[:6]}"
    store = Store(cfg.state_path)
    errors: list[str] = []
    classifications: list[Classification] = []
    rejected: list[str] = []
    items: list[SourceItem] = []
    ctx: AnalystContext | None = None
    model_name = "none"
    owns_classifier = classifier is None
    try:
        universe = universe or load_universe(
            cfg.state_path / "cache", cfg.sources.universe_csv, cfg.sources.universe_url,
            cfg.sources.http_timeout_s, now.astimezone(IST).date(),
        )
        classifier = classifier or make_classifier(cfg, store)
        model_name = classifier.name
        g = await asyncio.to_thread(gather, cfg, universe, now, jsonl_inputs)
        errors += g.errors
        items = prefilter(g.items, universe, now, cfg.llm.max_items_per_run)
        classifications, rejected, failures = await asyncio.wait_for(
            classify_all(items, classifier, store, now.astimezone(IST).date()), cfg.run_budget_s
        )
        if failures:
            errors.append(f"degraded: keyword baseline used for {failures}/{len(items)} items")
        ctx = compose(
            run_id, now, universe, {i.id: i for i in items}, classifications,
            g.regime, g.vol_bands, cfg.compose, model_name, g.unusual_volume,
        )
        if publish:
            publish_context(cfg.state_path, ctx)
            status = "published"
        else:
            status = "recorded"  # dry run: never counted by shadow-report
    except Exception as exc:  # fallback: engine runs on default rules
        errors.append(f"fallback: {type(exc).__name__}: {exc}")
        ctx = None
        status = "fallback"
    finally:
        if owns_classifier and hasattr(classifier, "aclose"):
            await classifier.aclose()
    meta = {i.id: {"title": i.title, "source": i.source, "url": i.url} for i in items}
    try:
        store.record_run(
            run_id, now, status, model_name, PROMPT_VERSION, SCHEMA_VERSION, len(items),
            [(c, meta.get(c.item_id, {})) for c in classifications], len(rejected), errors + rejected, ctx,
        )
    finally:
        store.close()
    return RunResult(run_id, status, ctx, classifications, rejected, errors)
