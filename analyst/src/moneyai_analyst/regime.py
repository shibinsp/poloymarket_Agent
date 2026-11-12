"""Market regime from India VIX, Nifty trend and FII/DII flows (pure functions)."""

from __future__ import annotations

import pandas as pd

from .config import ComposeConfig
from .schema import Bias, Regime


def _last_date(df: pd.DataFrame) -> str:
    return pd.Timestamp(df.index[-1]).date().isoformat()


def compute_regime(
    vix: pd.DataFrame | None,
    nifty: pd.DataFrame | None,
    fii_net_cr: float | None = None,
    dii_net_cr: float | None = None,
    flows_as_of: str | None = None,
) -> Regime:
    r = Regime(fii_net_cr=fii_net_cr, dii_net_cr=dii_net_cr)
    ids: list[str] = []
    if vix is not None and len(vix) >= 2:
        close = vix["Close"].astype(float)
        r.india_vix = round(float(close.iloc[-1]), 2)
        r.india_vix_change_pct = round(float(close.iloc[-1] / close.iloc[-2] - 1) * 100, 2)
        ids.append(f"market:INDIAVIX:{_last_date(vix)}")
    if nifty is not None and len(nifty) >= 21:
        close = nifty["Close"].astype(float)
        r.nifty_return_pct = round(float(close.iloc[-1] / close.iloc[-2] - 1) * 100, 2)
        r.nifty_above_sma20 = bool(close.iloc[-1] > close.iloc[-20:].mean())
        ids.append(f"market:NIFTY50:{_last_date(nifty)}")
    if fii_net_cr is not None or dii_net_cr is not None:
        ids.append(f"market:FIIDII:{flows_as_of or 'latest'}")
    r.source_ids = ids
    return r


def regime_bias(r: Regime, cfg: ComposeConfig) -> tuple[Bias, list[str]]:
    """Return the bias and the human-readable triggers. Missing data never
    raises the bias and never lowers it; it simply contributes nothing."""
    risk_off: list[str] = []
    cautious: list[str] = []
    if r.india_vix is not None:
        if r.india_vix >= cfg.vix_risk_off:
            risk_off.append(f"India VIX {r.india_vix} >= {cfg.vix_risk_off}")
        elif r.india_vix >= cfg.vix_cautious:
            cautious.append(f"India VIX {r.india_vix} >= {cfg.vix_cautious}")
    if r.india_vix_change_pct is not None and r.india_vix_change_pct >= cfg.vix_jump_risk_off_pct:
        risk_off.append(f"India VIX jumped {r.india_vix_change_pct}%")
    if r.nifty_return_pct is not None:
        if r.nifty_return_pct <= cfg.nifty_drop_risk_off_pct:
            risk_off.append(f"Nifty fell {r.nifty_return_pct}%")
        elif r.nifty_return_pct <= cfg.nifty_drop_cautious_pct:
            cautious.append(f"Nifty fell {r.nifty_return_pct}%")
    if r.fii_net_cr is not None and r.fii_net_cr <= cfg.fii_sell_cautious_cr:
        cautious.append(f"FII net sold Rs {abs(r.fii_net_cr):,.0f} cr")
    if risk_off:
        return Bias.RISK_OFF, risk_off + cautious
    if cautious:
        return Bias.CAUTIOUS, cautious
    return Bias.NEUTRAL, []


def bias_multiplier(bias: Bias, cfg: ComposeConfig) -> float:
    m = {
        Bias.NEUTRAL: cfg.multiplier_neutral,
        Bias.CAUTIOUS: cfg.multiplier_cautious,
        Bias.RISK_OFF: cfg.multiplier_risk_off,
    }[bias]
    # Restrict-only: a misconfigured multiplier above 1 is clamped, never honoured.
    return max(0.0, min(1.0, m))
