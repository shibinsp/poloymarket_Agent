from datetime import datetime
from zoneinfo import ZoneInfo

import numpy as np
import pandas as pd
import pytest

from moneyai_analyst.cli import next_run_at
from moneyai_analyst.config import ComposeConfig
from moneyai_analyst.forecast import direction_features, ewma_vol, vol_band, vol_multiplier, walk_forward_direction
from moneyai_analyst.schema import VolBand
from moneyai_analyst.rag import bm25_rank, chunk
from moneyai_analyst.regime import bias_multiplier, compute_regime, regime_bias
from moneyai_analyst.schema import Bias, Regime
from moneyai_analyst.sources.market import completed_sessions, parse_bhavcopy_delivery, parse_fii_dii, yahoo_symbol

IST = ZoneInfo("Asia/Kolkata")


def frame(closes, volume=1_000_000):
    idx = pd.bdate_range("2024-01-01", periods=len(closes))
    c = pd.Series(closes, index=idx, dtype=float)
    return pd.DataFrame({"Open": c, "High": c * 1.01, "Low": c * 0.99, "Close": c, "Volume": volume}, index=idx)


# -- regime ------------------------------------------------------------------------------------


@pytest.mark.parametrize(
    "regime, expected",
    [
        (Regime(), Bias.NEUTRAL),  # missing data contributes nothing
        (Regime(india_vix=16.9), Bias.NEUTRAL),
        (Regime(india_vix=17.0), Bias.CAUTIOUS),
        (Regime(india_vix=22.0), Bias.RISK_OFF),
        (Regime(india_vix=14.0, india_vix_change_pct=15.0), Bias.RISK_OFF),
        (Regime(nifty_return_pct=-1.0), Bias.CAUTIOUS),
        (Regime(nifty_return_pct=-2.0), Bias.RISK_OFF),
        (Regime(fii_net_cr=-3000.0), Bias.CAUTIOUS),
        (Regime(fii_net_cr=-2999.0), Bias.NEUTRAL),
    ],
)
def test_regime_thresholds(regime, expected):
    assert regime_bias(regime, ComposeConfig())[0] == expected


def test_bias_multiplier_never_exceeds_one():
    assert bias_multiplier(Bias.NEUTRAL, ComposeConfig(multiplier_neutral=2.0)) == 1.0


def test_compute_regime_reads_frames():
    vix = frame([15.0] * 25 + [18.0])
    nifty = frame([100.0] * 25 + [97.0])
    r = compute_regime(vix, nifty)
    assert r.india_vix == 18.0 and r.india_vix_change_pct == 20.0 and r.nifty_return_pct == -3.0
    assert r.nifty_above_sma20 is False and len(r.source_ids) == 2


# -- volatility bands ------------------------------------------------------------------------------


def test_ewma_vol_tracks_recent_variance():
    rng = np.random.default_rng(0)
    calm = pd.Series(rng.normal(0, 0.01, 300))
    shock = pd.concat([calm, pd.Series(rng.normal(0, 0.04, 30))], ignore_index=True)
    assert ewma_vol(calm) == pytest.approx(0.01, rel=0.35)
    assert ewma_vol(shock) > 2 * ewma_vol(calm)


def test_vol_band_flags_volatility_spike():
    rng = np.random.default_rng(1)
    rets = np.concatenate([rng.normal(0, 0.01, 300), rng.normal(0, 0.04, 20)])
    band = vol_band("TCS", frame(100 * np.exp(np.cumsum(rets))), "src")
    assert band is not None and band.ratio > 2.0 and band.low < band.high


@pytest.mark.parametrize("configured, expected", [(3.0, 1.0), (-0.5, 0.0), (0.75, 0.75)])
def test_vol_multiplier_is_clamped_to_unit_range(configured, expected):
    band = VolBand(symbol="TCS", forecast_daily_vol=0.03, baseline_daily_vol=0.018, ratio=1.6, low=1, high=2, source_id="s")
    assert vol_multiplier(band, ComposeConfig(vol_cut1_multiplier=configured)) == expected


def test_vol_band_needs_history():
    assert vol_band("TCS", frame([100.0] * 50), "src") is None


# -- direction research: time-ordered, purged -------------------------------------------------------


def test_walk_forward_folds_are_time_ordered_with_gap():
    pytest.importorskip("sklearn")
    rng = np.random.default_rng(2)
    df = frame(100 * np.exp(np.cumsum(rng.normal(0, 0.01, 900))))
    df["Volume"] = rng.integers(1e5, 1e6, len(df))
    feats = direction_features(df)
    assert not feats["label"].isna().any()
    folds = walk_forward_direction(feats, train=300, test=60, gap=5)
    assert len(folds) >= 5
    starts = [f.test_start for f in folds]
    assert starts == sorted(starts)
    # On a random walk there is no edge to find; the model must not "find" a large one.
    hit = sum(f.hit_rate * f.n for f in folds) / sum(f.n for f in folds)
    assert 0.4 < hit < 0.6


def test_direction_label_is_next_day():
    df = frame([100, 101, 100, 102, 103] * 10)
    feats = direction_features(df)
    close = df["Close"]
    for ts, row in feats.iterrows():
        nxt = close.index.get_loc(ts) + 1
        assert row["label"] == int(close.iloc[nxt] > close.loc[ts])


# -- RAG ---------------------------------------------------------------------------------------------


def test_bm25_finds_the_relevant_passage():
    docs = ["Management maintained FY27 revenue guidance of 8-10 percent growth.",
            "The board declared an interim dividend of Rs 5 per share.",
            "Capex for the new refinery will be funded from internal accruals."]
    assert bm25_rank("revenue guidance cut", docs)[0][0] == 0
    assert bm25_rank("", docs) == []


def test_chunk_overlaps():
    parts = chunk("x" * 2000, size=800, overlap=100)
    assert len(parts) == 3 and all(len(p) <= 800 for p in parts)


# -- source parsers ------------------------------------------------------------------------------------


def test_parse_fii_dii():
    rows = [{"category": "DII **", "date": "11-Nov-2026", "netValue": "2,345.10"},
            {"category": "FII/FPI *", "date": "11-Nov-2026", "netValue": "-3,120.55"}]
    assert parse_fii_dii(rows) == (-3120.55, 2345.10, "11-Nov-2026")


def test_parse_bhavcopy_delivery():
    text = "SYMBOL, SERIES, DATE1, DELIV_PER\nTCS, EQ, 11-Nov-2026, 54.21\nTCS, BE, 11-Nov-2026, 99\nINFY, EQ, 11-Nov-2026, -\n"
    assert parse_bhavcopy_delivery(text) == {"TCS": 54.21}


# -- universe tagging ------------------------------------------------------------------------------------


def test_universe_tags_company_names(universe):
    assert universe.tag("Reliance Industries shares jump") == ("RELIANCE",)
    assert universe.tag("Larsen & Toubro wins order") == ("LT",)
    assert universe.tag("State Bank of India and Infosys in focus") == ("INFY", "SBIN")
    assert universe.tag("An alternative fuel startup") == ()


# -- scheduler -------------------------------------------------------------------------------------------


@pytest.mark.parametrize(
    "now, expected",
    [
        (datetime(2026, 11, 12, 7, 0, tzinfo=IST), datetime(2026, 11, 12, 8, 45, tzinfo=IST)),
        (datetime(2026, 11, 12, 8, 45, tzinfo=IST), datetime(2026, 11, 12, 9, 0, tzinfo=IST)),
        (datetime(2026, 11, 12, 15, 20, tzinfo=IST), datetime(2026, 11, 12, 15, 30, tzinfo=IST)),
        (datetime(2026, 11, 13, 16, 0, tzinfo=IST), datetime(2026, 11, 16, 8, 45, tzinfo=IST)),  # Fri -> Mon
    ],
)
def test_next_run_at(now, expected):
    assert next_run_at(now, 15) == expected


def test_yahoo_symbol_keeps_punctuation():
    assert yahoo_symbol("M&M") == "M&M.NS" and yahoo_symbol("BAJAJ-AUTO") == "BAJAJ-AUTO.NS"


def test_completed_sessions_drops_forming_bar():
    df = frame([100.0] * 30)
    last = df.index[-1].date()
    assert len(completed_sessions(df, last, session_open=True)) == 29
    assert len(completed_sessions(df, last, session_open=False)) == 30
