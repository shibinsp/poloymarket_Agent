import asyncio
import json
from datetime import timedelta

from moneyai_analyst.classify import KeywordClassifier
from moneyai_analyst.config import Config
from moneyai_analyst.llm import LlmError
from moneyai_analyst.pipeline import lookback_start, prefilter, run_once
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


def test_model_outage_uses_keyword_baseline_not_fewer_restrictions(tmp_path, universe, now):
    feed = write_jsonl(tmp_path / "in.jsonl", [
        {"id": "n1", "title": "SEBI orders probe into State Bank of India disclosures", "published_at": "2026-11-12T08:00:00+05:30"},
        {"id": "n2", "title": "Infosys shares trade flat", "published_at": "2026-11-12T08:01:00+05:30"},
    ])
    cfg = offline_cfg(tmp_path)
    res = asyncio.run(run_once(cfg, now=now, universe=universe, classifier=AlwaysFails(), jsonl_inputs=[feed]))
    assert res.status == "published"
    assert res.context.exclude == ["SBIN"]  # same as the offline baseline would publish
    assert all(c.classifier == "keyword-v1 (fallback)" for c in res.classifications)
    assert any(e.startswith("degraded: keyword baseline used for 2/2") for e in res.errors)


class SometimesFails:
    name = "fake:flaky"

    async def classify(self, item, background, today, use_cache=True):
        if item.id == "sev3":
            raise LlmError("timeout")
        return KeywordClassifier().classify(item)


def test_partial_outage_keeps_the_failed_items_restrictions(tmp_path, universe, now):
    feed = write_jsonl(tmp_path / "in.jsonl", [
        {"id": "sev3", "title": "SEBI orders forensic audit of Infosys", "published_at": "2026-11-12T08:00:00+05:30"},
        {"id": "ok", "title": "Reliance board approves bonus issue", "published_at": "2026-11-12T08:01:00+05:30"},
    ])
    res = asyncio.run(run_once(offline_cfg(tmp_path), now=now, universe=universe, classifier=SometimesFails(), jsonl_inputs=[feed]))
    assert res.context.exclude == ["INFY"] and res.context.watchlist == ["RELIANCE"]


class SaysNothingMatters:
    name = "fake:dismissive"

    async def classify(self, item, background, today, use_cache=True):
        c = KeywordClassifier().classify(item)
        return c.model_copy(update={"severity": 0, "classifier": self.name})


def test_calendar_events_exclude_even_when_the_model_disagrees(tmp_path, universe, now):
    cal = tmp_path / "events.csv"
    cal.write_text("symbol,date,event_type,description\nINFY,2026-11-12,results,Q2 results\nTCS,2026-11-13,results,tomorrow\n")
    cfg = offline_cfg(tmp_path)
    cfg.sources.events_calendar = str(cal)
    res = asyncio.run(run_once(cfg, now=now, universe=universe, classifier=SaysNothingMatters()))
    assert res.context.exclude == ["INFY"]


def test_dry_run_is_recorded_but_not_counted_as_published(tmp_path, universe, now):
    feed = write_jsonl(tmp_path / "in.jsonl", [
        {"id": "n1", "title": "SEBI orders probe into Infosys", "published_at": "2026-11-12T08:00:00+05:30"},
    ])
    cfg = offline_cfg(tmp_path)
    res = asyncio.run(run_once(cfg, now=now, universe=universe, classifier=KeywordClassifier(), jsonl_inputs=[feed], publish=False))
    assert res.status == "recorded" and res.context.exclude == ["INFY"]
    assert not (cfg.state_path / "context.json").exists()
    assert Store(cfg.state_path).contexts() == []


def test_failed_universe_load_records_fallback_with_no_context(tmp_path, now):
    cfg = offline_cfg(tmp_path)
    cfg.sources.universe_url = "http://127.0.0.1:9/nope.csv"
    cfg.sources.http_timeout_s = 0.5
    asyncio.run(run_once(cfg, now=now, classifier=KeywordClassifier()))
    assert Store(cfg.state_path).db.execute("SELECT status, context FROM runs").fetchone() == ("fallback", None)
    assert not (cfg.state_path / "context.json").exists()


def test_rag_failure_on_one_item_does_not_sink_the_run(tmp_path, universe, now, monkeypatch):
    import moneyai_analyst.pipeline as pl

    def broken(store, symbol, query):
        if symbol == "RELIANCE":
            raise ValueError("bad published_at in documents table")
        return []
    monkeypatch.setattr(pl, "retrieve", broken)
    feed = write_jsonl(tmp_path / "in.jsonl", [
        {"id": "r", "title": "Reliance board approves bonus issue", "published_at": "2026-11-12T08:00:00+05:30"},
        {"id": "i", "title": "SEBI orders probe into Infosys", "published_at": "2026-11-12T08:01:00+05:30"},
    ])
    res = asyncio.run(run_once(offline_cfg(tmp_path), now=now, universe=universe, classifier=SaysNothingMatters(), jsonl_inputs=[feed]))
    assert res.status == "published"
    assert any(r.startswith("r: error: ValueError") for r in res.rejected)


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


def test_monday_premarket_sees_weekend_news(universe):
    from datetime import datetime
    from zoneinfo import ZoneInfo
    ist = ZoneInfo("Asia/Kolkata")
    monday = datetime(2026, 11, 16, 8, 45, tzinfo=ist)
    items = [
        make_item("sat", "Reliance Q2 results: profit falls", published_at=datetime(2026, 11, 14, 17, 0, tzinfo=ist)),
        make_item("fri_am", "Reliance old news", published_at=datetime(2026, 11, 13, 11, 0, tzinfo=ist)),
    ]
    assert [i.id for i in prefilter(items, universe, monday, 30)] == ["sat"]


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
    assert res.status == "published"
    assert res.context.exclude == ["INFY", "RELIANCE"]  # the failed item is backfilled by the baseline
    assert any(r.startswith("bad: error: AttributeError") for r in res.rejected)


def test_lookback_start_midweek_is_eighteen_hours(now):
    assert lookback_start(now) == min(now - timedelta(hours=18), now.replace(day=11, hour=15, minute=30))
