from datetime import timedelta

import pytest
from pydantic import ValidationError

from moneyai_analyst.compose import InvalidContext, compose, validate_context
from moneyai_analyst.config import ComposeConfig
from moneyai_analyst.schema import AnalystContext, Bias, Classification, EventType, Reason, Regime, VolBand

from conftest import make_item


def ctx_kwargs(now, **over):
    base = dict(run_id="r", published_at=now, expires_at=now + timedelta(minutes=20),
                schema_version="1", prompt_version="p", model="m")
    base.update(over)
    return base


# -- schema: restrict-only by construction --------------------------------------------


def test_size_multiplier_above_one_is_rejected(now):
    with pytest.raises(ValidationError):
        AnalystContext(**ctx_kwargs(now, size_multiplier=1.01))


def test_per_symbol_multiplier_above_one_is_rejected(now):
    with pytest.raises(ValidationError):
        AnalystContext(**ctx_kwargs(now, symbol_size_multipliers={"TCS": 1.5}))


def test_negative_multiplier_is_rejected(now):
    with pytest.raises(ValidationError):
        AnalystContext(**ctx_kwargs(now, symbol_size_multipliers={"TCS": -0.1}))


def test_mode_cannot_be_enforce(now):
    with pytest.raises(ValidationError):
        AnalystContext(**ctx_kwargs(now, mode="enforce"))


def test_expiry_must_follow_publication(now):
    with pytest.raises(ValidationError):
        AnalystContext(**ctx_kwargs(now, expires_at=now))


def test_symbol_cannot_be_watched_and_excluded(now):
    with pytest.raises(ValidationError):
        AnalystContext(**ctx_kwargs(now, watchlist=["TCS"], exclude=["TCS"]))


def test_is_active_window(now):
    c = AnalystContext(**ctx_kwargs(now))
    assert c.is_active(now)
    assert c.is_active(now + timedelta(minutes=19, seconds=59))
    assert not c.is_active(now + timedelta(minutes=20))
    assert not c.is_active(now - timedelta(seconds=1))


def test_reason_requires_a_source():
    with pytest.raises(ValidationError):
        Reason(text="x", source_ids=[])


# -- compose ------------------------------------------------------------------------------


def cls(item_id, etype, sev, symbols=("RELIANCE",), sentiment=0.0):
    return Classification(item_id=item_id, event_type=etype, severity=sev, sentiment=sentiment,
                          symbols=list(symbols), evidence_ids=[item_id])


def run_compose(universe, now, items, classifications, regime=None, bands=(), cfg=None, unusual=None):
    return compose("r1", now, universe, {i.id: i for i in items}, classifications,
                   regime or Regime(), list(bands), cfg or ComposeConfig(), "test", unusual)


def test_severity_two_excludes_and_one_watches(universe, now):
    items = [make_item("a", "Reliance Q2 profit falls"), make_item("b", "TCS investor day", ("TCS",))]
    ctx = run_compose(universe, now, items, [cls("a", EventType.RESULTS, 2), cls("b", EventType.OTHER, 1, ("TCS",))])
    assert ctx.exclude == ["RELIANCE"]
    assert ctx.watchlist == ["TCS"]
    assert ctx.reasons[0].source_ids == ["a"]


def test_severity_below_threshold_does_not_exclude(universe, now):
    items = [make_item("a", "Reliance minor update")]
    ctx = run_compose(universe, now, items, [cls("a", EventType.RESULTS, 1)])
    assert ctx.exclude == []


def test_macro_news_never_excludes_a_stock(universe, now):
    items = [make_item("a", "RBI hikes repo rate, banks in focus", ("HDFCBANK",))]
    ctx = run_compose(universe, now, items, [cls("a", EventType.MACRO, 3, ("HDFCBANK",))])
    assert ctx.exclude == []


def test_excluded_symbol_is_dropped_from_watchlist(universe, now):
    items = [make_item("a", "x"), make_item("b", "y")]
    ctx = run_compose(universe, now, items, [cls("a", EventType.OTHER, 1), cls("b", EventType.REGULATORY, 3)])
    assert ctx.exclude == ["RELIANCE"] and ctx.watchlist == []


def test_risk_off_regime_halves_size(universe, now):
    regime = Regime(india_vix=24.0, source_ids=["market:INDIAVIX:2026-11-11"])
    ctx = run_compose(universe, now, [], [], regime=regime)
    assert ctx.bias == Bias.RISK_OFF and ctx.size_multiplier == 0.5
    assert ctx.reasons[0].source_ids == ["market:INDIAVIX:2026-11-11"]


def test_high_volatility_band_cuts_symbol_size(universe, now):
    band = VolBand(symbol="TCS", forecast_daily_vol=0.04, baseline_daily_vol=0.015, ratio=2.67,
                   low=1, high=2, source_id="market:TCS:2026-11-11")
    ctx = run_compose(universe, now, [], [], bands=[band])
    assert ctx.symbol_size_multipliers == {"TCS": 0.5}


def test_misconfigured_multiplier_above_one_is_clamped(universe, now):
    cfg = ComposeConfig(multiplier_neutral=1.5, vol_cut1_multiplier=3.0)
    band = VolBand(symbol="TCS", forecast_daily_vol=0.03, baseline_daily_vol=0.018, ratio=1.6,
                   low=1, high=2, source_id="s")
    ctx = run_compose(universe, now, [], [], bands=[band], cfg=cfg)
    assert ctx.size_multiplier == 1.0
    assert ctx.symbol_size_multipliers == {}  # clamped to 1.0 -> no cut recorded


def test_unusual_volume_adds_to_watchlist(universe, now):
    ctx = run_compose(universe, now, [], [], unusual={"INFY": (3.2, "market:INFY:2026-11-11")})
    assert ctx.watchlist == ["INFY"]


def test_validate_rejects_unknown_source_ids(universe, now):
    ctx = AnalystContext(**ctx_kwargs(now, reasons=[Reason(text="x", source_ids=["made-up"])]))
    with pytest.raises(InvalidContext):
        validate_context(ctx, universe, {"real"})


def test_validate_rejects_symbols_outside_universe(universe, now):
    ctx = AnalystContext(**ctx_kwargs(now, exclude=["ADANIPORTS"]))
    with pytest.raises(InvalidContext):
        validate_context(ctx, universe, set())
