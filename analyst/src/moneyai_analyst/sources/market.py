"""Quant agent input: India VIX, Nifty 50 and daily OHLCV (Yahoo Finance), plus
opt-in NSE FII/DII flows and delivery percentage from the NSE bhavcopy."""

from __future__ import annotations

import csv
import io
from datetime import date

import httpx
import pandas as pd

VIX_TICKER = "^INDIAVIX"
NIFTY_TICKER = "^NSEI"


def yahoo_symbol(nse_symbol: str) -> str:
    return f"{nse_symbol}.NS"  # Yahoo keeps NSE punctuation: M&M.NS, BAJAJ-AUTO.NS


def download_daily(tickers: list[str], period: str = "2y") -> dict[str, pd.DataFrame]:
    """Daily OHLCV per ticker. Missing tickers are omitted, never filled."""
    import yfinance as yf  # optional dependency: pip install .[data]

    raw = yf.download(tickers, period=period, interval="1d", group_by="ticker", auto_adjust=True, progress=False, threads=True)
    return split_download(raw, tickers)


def split_download(raw: pd.DataFrame, tickers: list[str]) -> dict[str, pd.DataFrame]:
    """Per-ticker frames from a yfinance download, whether its columns are a
    (ticker, field) MultiIndex (any count with group_by="ticker") or flat."""
    out: dict[str, pd.DataFrame] = {}
    for t in tickers:
        if isinstance(raw.columns, pd.MultiIndex):
            if t not in raw.columns.get_level_values(0):
                continue
            df = raw[t]
        elif len(tickers) == 1:
            df = raw
        else:
            continue
        if "Close" not in df.columns:
            continue
        df = df.dropna(subset=["Close"])
        if len(df) >= 30:
            out[t] = df
    return out


def parse_fii_dii(payload: list[dict]) -> tuple[float | None, float | None, str | None]:
    """Parse NSE ``/api/fiidiiTradeReact`` rows into (fii_net_cr, dii_net_cr, date)."""
    fii = dii = None
    as_of = None
    for row in payload or []:
        cat = (row.get("category") or "").upper()
        try:
            net = float(str(row.get("netValue", "")).replace(",", ""))
        except ValueError:
            continue
        if "FII" in cat or "FPI" in cat:
            fii = net
        elif "DII" in cat:
            dii = net
        as_of = row.get("date") or as_of
    return fii, dii, as_of


def fetch_fii_dii(timeout_s: float) -> tuple[float | None, float | None, str | None]:
    headers = {"User-Agent": "Mozilla/5.0", "Accept": "application/json", "Referer": "https://www.nseindia.com/"}
    with httpx.Client(headers=headers, timeout=timeout_s, follow_redirects=True) as client:
        client.get("https://www.nseindia.com/")  # sets the session cookies NSE requires
        resp = client.get("https://www.nseindia.com/api/fiidiiTradeReact")
        resp.raise_for_status()
        return parse_fii_dii(resp.json())


def parse_bhavcopy_delivery(text: str) -> dict[str, float]:
    """Delivery % per EQ-series symbol from ``sec_bhavdata_full_DDMMYYYY.csv``."""
    out: dict[str, float] = {}
    reader = csv.DictReader(io.StringIO(text))
    reader.fieldnames = [f.strip() for f in reader.fieldnames or []]
    for row in reader:
        row = {k: (v or "").strip() for k, v in row.items() if k}
        if row.get("SERIES") != "EQ":
            continue
        try:
            out[row["SYMBOL"]] = float(row["DELIV_PER"])
        except (KeyError, ValueError):
            continue
    return out


def fetch_bhavcopy_delivery(day: date, timeout_s: float) -> dict[str, float]:
    url = f"https://nsearchives.nseindia.com/products/content/sec_bhavdata_full_{day.strftime('%d%m%Y')}.csv"
    resp = httpx.get(url, timeout=timeout_s, headers={"User-Agent": "Mozilla/5.0"})
    resp.raise_for_status()
    return parse_bhavcopy_delivery(resp.text)


def completed_sessions(df: pd.DataFrame, today: date, session_open: bool) -> pd.DataFrame:
    """Drop today's still-forming daily bar so features only use closed sessions."""
    if session_open and len(df) and pd.Timestamp(df.index[-1]).date() >= today:
        return df.iloc[:-1]
    return df
