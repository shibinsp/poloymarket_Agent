import asyncio
import json
from datetime import date

import pytest

from moneyai_analyst.classify import KeywordClassifier, LlmClassifier, RejectedOutput, validate_classification
from moneyai_analyst.llm import LlmError, extract_json
from moneyai_analyst.schema import EventType
from moneyai_analyst.store import Store

from conftest import make_item

TODAY = date(2026, 11, 12)


def good(item_id="n1", **over):
    d = {"item_id": item_id, "event_type": "results", "severity": 2, "sentiment": -0.4,
         "symbols": ["RELIANCE"], "summary": "s", "evidence_ids": [item_id]}
    d.update(over)
    return d


# -- validation ------------------------------------------------------------------------------


def test_valid_output_passes():
    item = make_item("n1", "Reliance Q2 results")
    c = validate_classification(good(), item, {"n1"}, "t")
    assert c.event_type == EventType.RESULTS


@pytest.mark.parametrize(
    "raw, why",
    [
        (good(item_id="other", evidence_ids=["n1"]), "answer is about a different item"),
        (good(evidence_ids=["n1", "ghost"]), "unknown evidence"),
        (good(evidence_ids=["bg1"]), "must cite the item itself"),
        (good(symbols=["TCS"]), "invented symbol"),
        (good(severity=4), "severity out of range"),
        (good(sentiment=1.5), "sentiment out of range"),
        (good(event_type="moon"), "label not in enum"),
        ({k: v for k, v in good().items() if k != "evidence_ids"}, "no evidence"),
        (good(extra_field=1), "extra field"),
    ],
)
def test_invalid_output_is_rejected(raw, why):
    item = make_item("n1", "Reliance Q2 results")
    with pytest.raises(RejectedOutput):
        validate_classification(raw, item, {"n1", "bg1"}, "t")


def test_extract_json_accepts_fence_and_rejects_prose():
    assert extract_json('```json\n{"a": 1}\n```') == {"a": 1}
    with pytest.raises(ValueError):
        extract_json("Sure! Here is the JSON: {\"a\": 1}")
    with pytest.raises(ValueError):
        extract_json("[1, 2]")


# -- keyword baseline ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "title, etype, sev",
    [
        ("SEBI orders forensic audit of Reliance accounts", EventType.REGULATORY, 3),
        ("Reliance Q2 results: net profit rises 12%", EventType.RESULTS, 2),
        ("Promoter pledges more Reliance shares", EventType.PLEDGE, 2),
        ("Block deal: 1 crore Reliance shares change hands", EventType.BLOCK_DEAL, 2),
        ("Reliance board approves 1:1 bonus issue", EventType.CORPORATE_ACTION, 1),
        ("Reliance shares trade flat", EventType.OTHER, 0),
    ],
)
def test_keyword_rules(title, etype, sev):
    c = KeywordClassifier().classify(make_item("k", title))
    assert (c.event_type, c.severity) == (etype, sev)
    assert c.evidence_ids == ["k"]


def test_calendar_item_is_exclusion_grade():
    c = KeywordClassifier().classify(make_item("cal", "RELIANCE: results on 2026-11-12", kind="calendar"))
    assert c.event_type == EventType.RESULTS and c.severity == 2


# -- LLM classifier with a fake client ------------------------------------------------------------


class FakeClient:
    def __init__(self, replies):
        self.replies = list(replies)
        self.calls = 0
        self.model_id = "fake:model"

    async def complete(self, system, user, max_tokens):
        self.calls += 1
        r = self.replies.pop(0)
        if isinstance(r, Exception):
            raise r
        return r


def test_llm_classifier_validates_and_caches(tmp_path):
    client = FakeClient([json.dumps(good())])
    clf = LlmClassifier(client, Store(tmp_path), 300, 2)
    item = make_item("n1", "Reliance Q2 results")
    a = asyncio.run(clf.classify(item, [], TODAY))
    b = asyncio.run(clf.classify(item, [], TODAY))
    assert a == b and client.calls == 1  # second answer came from the cache


def test_llm_prose_is_rejected_not_repaired(tmp_path):
    clf = LlmClassifier(FakeClient(["The event is results, severity 2."]), Store(tmp_path), 300, 2)
    with pytest.raises(RejectedOutput):
        asyncio.run(clf.classify(make_item("n1", "x"), [], TODAY))


def test_llm_rejected_output_is_not_cached(tmp_path):
    client = FakeClient([json.dumps(good(symbols=["TCS"])), json.dumps(good())])
    clf = LlmClassifier(client, Store(tmp_path), 300, 2)
    item = make_item("n1", "x")
    with pytest.raises(RejectedOutput):
        asyncio.run(clf.classify(item, [], TODAY))
    assert asyncio.run(clf.classify(item, [], TODAY)).symbols == ["RELIANCE"]
    assert client.calls == 2


def test_llm_failure_propagates_as_llm_error(tmp_path):
    clf = LlmClassifier(FakeClient([LlmError("timeout")]), Store(tmp_path), 300, 2)
    with pytest.raises(LlmError):
        asyncio.run(clf.classify(make_item("n1", "x"), [], TODAY))


# -- the real client against a mocked HTTP server ---------------------------------------------------

import httpx

from moneyai_analyst.config import LlmConfig
from moneyai_analyst.llm import LlmClient


def client_returning(payload, seen=None):
    def handler(request):
        if seen is not None:
            seen.append(json.loads(request.content))
        return httpx.Response(200, json=payload)
    cfg = LlmConfig(provider="openai_compatible", retries=0, extra_body={"chat_template_kwargs": {"enable_thinking": False}})
    return LlmClient(cfg, "k", httpx.AsyncClient(transport=httpx.MockTransport(handler)))


def test_null_content_from_reasoning_model_is_an_llm_error():
    c = client_returning({"choices": [{"message": {"content": None, "reasoning_content": "thinking..."}}]})
    with pytest.raises(LlmError):
        asyncio.run(c.complete("s", "u", 10))


def test_extra_body_is_sent_but_cannot_override_core_fields():
    seen = []
    c = client_returning({"choices": [{"message": {"content": "{}"}}]}, seen)
    c.cfg.extra_body["temperature"] = 0.9
    asyncio.run(c.complete("s", "u", 10))
    assert seen[0]["chat_template_kwargs"] == {"enable_thinking": False}
    assert seen[0]["temperature"] == 0.0


def test_llm_latency_excludes_queueing(tmp_path):
    class Slow(FakeClient):
        async def complete(self, system, user, max_tokens):
            await asyncio.sleep(0.05)
            return json.dumps(good(item_id=user.split('"id": "')[-1].split('"')[0]))

    clf = LlmClassifier(Slow([]), None, 300, 1)  # concurrency 1: items queue
    items = [make_item(f"n{i}", "x") for i in range(6)]

    async def go():
        return await asyncio.gather(*(clf.classify(i, [], TODAY) for i in items))
    asyncio.run(go())
    assert len(clf.latencies) == 6 and max(clf.latencies) < 0.2  # queueing would reach ~0.3s
