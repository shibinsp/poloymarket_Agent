import asyncio
import json
from datetime import timedelta

from moneyai_analyst.classify import KeywordClassifier
from moneyai_analyst.config import Config
from moneyai_analyst.llm import LlmError
from moneyai_analyst.pipeline import prefilter, run_once
from moneyai_analyst.store import Store, load_active_context

from conftest import make_item


def offline_cfg(tmp_path) -> Config:
    cfg = Config(state_dir=str(tmp_path / "state"))
    cfg.sources.market_data = False
    cfg.sources.rss_feeds = []
    return cfg


def write_jsonl(path, rows):
    path.write_text("\n".join(json.dumps(r) for r in rows))
    return str(path)


def test_run_once_publishes_context_from_replay(tmp_path, universe, now):
    feed = write_jsonl(tmp_path / "in.jsonl", [
        {"id": "n1", "title": "SEBI orders probe into Infosys disclosures", "published_at": "2026-11-12T08:00:00+05:30"},
        {"id": "n2", "title": "ITC board approves bonus issue", "published_at": "2026-11-12T08:10:00+05:30"},
        {"id": "n3", "title": "Unrelated company XYZ rallies", "published_at": "2026-11-12T08:15:00+05:30"},
    ])
    cfg = offline_cfg(tmp_path)
    res = asyncio.run(run_once(cfg, now=now, universe=universe, classifier=KeywordClassifier(), jsonl_inputs=[feed]))
    assert res.status == "published", res.errors
    assert res.context.exclude == ["INFY"]
    assert res.context.watchlist == ["ITC"]
    published = load_active_context(cfg.state_path / "context.json", now + timedelta(minutes=1))
    assert published == res.context
    assert load_active_context(cfg.state_path / "context.json", now + timedelta(minutes=21)) is None
    assert Store(cfg.state_path).contexts() == [res.context]


class AlwaysFails:
    name = "fake:down"

    async def classify(self, item, background, today, use_cache=True):
        raise LlmError("model down")


def test_model_outage_falls_back_and_publishes_nothing(tmp_path, universe, now):
    feed = write_jsonl(tmp_path / "in.jsonl", [
        {"id": "n1", "title": "Reliance results today", "published_at": "2026-11-12T08:00:00+05:30"},
    ])
    cfg = offline_cfg(tmp_path)
    res = asyncio.run(run_once(cfg, now=now, universe=universe, classifier=AlwaysFails(), jsonl_inputs=[feed]))
    assert res.status == "fallback"
    assert res.context is None
    assert not (cfg.state_path / "context.json").exists()
    row = Store(cfg.state_path).db.execute("SELECT status, context FROM runs").fetchone()
    assert row == ("fallback", None)


def test_no_universe_falls_back(tmp_path, now):
    cfg = offline_cfg(tmp_path)
    cfg.sources.universe_url = "http://127.0.0.1:9/nope.csv"
    cfg.sources.http_timeout_s = 0.5
    res = asyncio.run(run_once(cfg, now=now, classifier=KeywordClassifier()))
    assert res.status == "fallback"


def test_prefilter_dedupes_and_applies_lookback(universe, now):
    items = [
        make_item("a", "Reliance results today"),
        make_item("b", "Reliance  results  today!"),  # same headline, different id
        make_item("c", "Old Reliance news", published_at=now - timedelta(days=2)),
        make_item("d", "No stock here", symbols=()),
        make_item("e", "RELIANCE: results", kind="calendar", published_at=now - timedelta(days=1)),
    ]
    kept = [i.id for i in prefilter(items, universe, now, 30)]
    assert kept == ["e", "a"]


def test_prefilter_caps_items_but_keeps_calendar(universe, now):
    items = [make_item(f"n{i}", f"Reliance item {i}") for i in range(10)]
    items.append(make_item("cal", "RELIANCE: results", kind="calendar"))
    kept = prefilter(items, universe, now, 3)
    assert len(kept) == 3 and kept[0].id == "cal"


def test_boilerplate_headlines_are_ignored(tmp_path, universe, now):
    feed = write_jsonl(tmp_path / "in.jsonl", [
        {"id": "n1", "title": "Infosys Share Price Live Updates: SEBI probe fraud", "published_at": "2026-11-12T08:00:00+05:30"},
    ])
    res = asyncio.run(run_once(offline_cfg(tmp_path), now=now, universe=universe, classifier=KeywordClassifier(), jsonl_inputs=[feed]))
    assert res.status == "published" and res.context.exclude == [] and res.classifications == []


class Explodes:
    name = "fake:explodes"

    async def classify(self, item, background, today, use_cache=True):
        if item.id == "bad":
            raise AttributeError("'NoneType' object has no attribute 'strip'")
        from moneyai_analyst.classify import KeywordClassifier
        return KeywordClassifier().classify(item)


def test_one_malformed_answer_does_not_sink_the_run(tmp_path, universe, now):
    feed = write_jsonl(tmp_path / "in.jsonl", [
        {"id": "bad", "title": "Reliance block deal", "published_at": "2026-11-12T08:00:00+05:30"},
        {"id": "ok", "title": "SEBI orders probe into Infosys", "published_at": "2026-11-12T08:01:00+05:30"},
    ])
    res = asyncio.run(run_once(offline_cfg(tmp_path), now=now, universe=universe, classifier=Explodes(), jsonl_inputs=[feed]))
    assert res.status == "published" and res.context.exclude == ["INFY"]
    assert any(r.startswith("bad: rejected") for r in res.rejected)
