"""Quant forecasting agent.

Published output: **volatility bands only** (EWMA forecast of next-day volatility
against the symbol's own one-year baseline). High forecast volatility cuts
position size; it never raises it.

Direction forecasting lives in :func:`walk_forward_direction` as a research tool.
Its output is never published to the trading context: per the architecture doc,
a direction model may only become an ML *filter* after a rule strategy has
passed its gates, and only on net-of-cost labels.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

import numpy as np
import pandas as pd

from .config import ComposeConfig
from .schema import VolBand

EWMA_LAMBDA = 0.94  # RiskMetrics daily decay


def ewma_vol(returns: pd.Series, lam: float = EWMA_LAMBDA) -> float:
    r = returns.dropna().to_numpy(dtype=float)
    if len(r) < 20:
        raise ValueError("need at least 20 returns")
    var = float(np.var(r[:20]))
    for x in r[20:]:
        var = lam * var + (1 - lam) * x * x
    return math.sqrt(var)


def vol_band(symbol: str, df: pd.DataFrame, source_id: str) -> VolBand | None:
    close = df["Close"].astype(float)
    rets = np.log(close).diff().dropna()
    if len(rets) < 120:
        return None
    forecast = ewma_vol(rets)
    rolling = rets.rolling(20).std().dropna().iloc[-252:]
    baseline = float(rolling.median())
    if baseline <= 0:
        return None
    last = float(close.iloc[-1])
    return VolBand(
        symbol=symbol,
        forecast_daily_vol=round(forecast, 6),
        baseline_daily_vol=round(baseline, 6),
        ratio=round(forecast / baseline, 3),
        low=round(last * math.exp(-forecast), 2),
        high=round(last * math.exp(forecast), 2),
        source_id=source_id,
    )


def vol_multiplier(band: VolBand, cfg: ComposeConfig) -> float:
    if band.ratio >= cfg.vol_ratio_cut2:
        m = cfg.vol_cut2_multiplier
    elif band.ratio >= cfg.vol_ratio_cut1:
        m = cfg.vol_cut1_multiplier
    else:
        m = 1.0
    return max(0.0, min(1.0, m))


# ---------------------------------------------------------------------------
# Research only: direction model with purged walk-forward validation.
# ---------------------------------------------------------------------------


def _rsi(close: pd.Series, n: int = 14) -> pd.Series:
    d = close.diff()
    up = d.clip(lower=0).ewm(alpha=1 / n, adjust=False).mean()
    dn = (-d.clip(upper=0)).ewm(alpha=1 / n, adjust=False).mean()
    return 100 - 100 / (1 + up / dn.replace(0, np.nan))


def direction_features(df: pd.DataFrame, delivery_pct: pd.Series | None = None) -> pd.DataFrame:
    """Features use only data available at the close of day t; the label is the
    sign of the day t+1 return."""
    c = df["Close"].astype(float)
    h, lo, v = df["High"].astype(float), df["Low"].astype(float), df["Volume"].astype(float)
    r = np.log(c).diff()
    tr = pd.concat([h - lo, (h - c.shift()).abs(), (lo - c.shift()).abs()], axis=1).max(axis=1)
    ema12, ema26 = c.ewm(span=12, adjust=False).mean(), c.ewm(span=26, adjust=False).mean()
    feats = pd.DataFrame(
        {
            "ret1": r,
            "ret5": r.rolling(5).sum(),
            "ret20": r.rolling(20).sum(),
            "rsi14": _rsi(c),
            "macd": (ema12 - ema26) / c,
            "atr_pct": tr.rolling(14).mean() / c,
            "vol_ratio": v / v.rolling(20).mean(),
            "dist_sma20": c / c.rolling(20).mean() - 1,
        },
        index=df.index,
    )
    if delivery_pct is not None:
        feats["delivery_pct"] = delivery_pct.reindex(df.index)
    feats["label"] = (r.shift(-1) > 0).astype(int)
    feats.loc[feats.index[-1], "label"] = np.nan  # no future for the last row
    return feats.dropna()


@dataclass
class FoldResult:
    test_start: str
    test_end: str
    n: int
    hit_rate: float
    baseline_hit_rate: float


def walk_forward_direction(feats: pd.DataFrame, train: int = 500, test: int = 60, gap: int = 5) -> list[FoldResult]:
    """Rolling, time-ordered folds with a purge gap. Never shuffles."""
    try:
        from lightgbm import LGBMClassifier

        def make():
            return LGBMClassifier(
                n_estimators=300, learning_rate=0.02, max_depth=4, num_leaves=15,
                min_child_samples=50, subsample=0.8, subsample_freq=1, verbose=-1,
            )
    except ImportError:  # pragma: no cover - fallback when the ml extra is absent
        from sklearn.linear_model import LogisticRegression

        def make():
            return LogisticRegression(max_iter=1000)

    X = feats.drop(columns=["label"])
    y = feats["label"].astype(int)
    out: list[FoldResult] = []
    start = 0
    while start + train + gap + test <= len(feats):
        tr = slice(start, start + train)
        te = slice(start + train + gap, start + train + gap + test)
        model = make().fit(X.iloc[tr], y.iloc[tr])
        pred = model.predict(X.iloc[te])
        actual = y.iloc[te].to_numpy()
        majority = int(y.iloc[tr].mean() >= 0.5)
        out.append(
            FoldResult(
                test_start=str(pd.Timestamp(X.index[te][0]).date()),
                test_end=str(pd.Timestamp(X.index[te][-1]).date()),
                n=len(actual),
                hit_rate=float((pred == actual).mean()),
                baseline_hit_rate=float((actual == majority).mean()),
            )
        )
        start += test
    return out
