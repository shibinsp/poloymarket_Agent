"""Value of restrictions: the only metric that justifies switching the analyst on.

For every round-trip trade the engine made, look up the analyst context that was
active at entry. If the stock was excluded, the restricted result is 0 (the trade
and its costs are skipped); otherwise the result is scaled by the size multiplier.

    value = sum(restricted result) - sum(actual result)

Positive over 40+ trading days means the restrictions helped. Reads the engine's
SQLite database read-only."""

from __future__ import annotations

import sqlite3
from collections import defaultdict
from dataclasses import dataclass
from datetime import datetime
from zoneinfo import ZoneInfo

from .schema import AnalystContext

IST = ZoneInfo("Asia/Kolkata")
FILLED = {"FILLED", "COMPLETE", "ACKNOWLEDGED", "PARTIALLY_FILLED", "PARTIALLYFILLED"}


def intraday_costs(buy_value: float, sell_value: float) -> float:
    """NSE intraday equity charges for one round trip (discount-broker rates).
    Brokerage: lower of Rs 20 or 0.03% per order. STT 0.025% on the sell side,
    stamp 0.003% on the buy side, NSE transaction 0.00297%, SEBI Rs 10/crore,
    GST 18% on brokerage + transaction + SEBI fees."""
    brokerage = min(20.0, 0.0003 * buy_value) + min(20.0, 0.0003 * sell_value)
    turnover = buy_value + sell_value
    exchange = 0.0000297 * turnover
    sebi = 0.000001 * turnover
    gst = 0.18 * (brokerage + exchange + sebi)
    return brokerage + 0.00025 * sell_value + 0.00003 * buy_value + exchange + sebi + gst


@dataclass
class Fill:
    symbol: str
    side: str
    qty: int
    price: float
    at: datetime


@dataclass
class RoundTrip:
    symbol: str
    entry_at: datetime
    qty: int
    net: float


def load_fills(db_path: str) -> list[Fill]:
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    rows = con.execute(
        "SELECT symbol, side, quantity, fill_quantity, price, average_fill_price, status, created_at "
        "FROM india_sandbox_orders ORDER BY created_at"
    ).fetchall()
    con.close()
    fills = []
    for sym, side, qty, fqty, price, avg, status, created in rows:
        if (status or "").upper() not in FILLED:
            continue
        q = int(fqty or 0) or int(qty or 0)
        try:
            px = float(avg or price)
        except (TypeError, ValueError):
            continue
        at = datetime.fromisoformat(created.replace("Z", "+00:00"))
        if at.tzinfo is None:
            at = at.replace(tzinfo=IST)
        if q > 0 and px > 0:
            fills.append(Fill(sym, side.upper(), q, px, at))
    return fills


def pair_round_trips(fills: list[Fill]) -> list[RoundTrip]:
    """FIFO pairing per symbol. Long and short both supported."""
    open_lots: dict[str, list[list]] = defaultdict(list)  # symbol -> [[side, qty, price, at], ...]
    trips: list[RoundTrip] = []
    for f in fills:
        lots = open_lots[f.symbol]
        remaining = f.qty
        while remaining and lots and lots[0][0] != f.side:
            lot = lots[0]
            q = min(remaining, lot[1])
            entry_side, entry_px, entry_at = lot[0], lot[2], lot[3]
            if entry_side == "BUY":
                buy, sell = entry_px * q, f.price * q
            else:
                buy, sell = f.price * q, entry_px * q
            trips.append(RoundTrip(f.symbol, entry_at, q, sell - buy - intraday_costs(buy, sell)))
            lot[1] -= q
            remaining -= q
            if lot[1] == 0:
                lots.pop(0)
        if remaining:
            lots.append([f.side, remaining, f.price, f.at])
    return trips


def active_context(contexts: list[AnalystContext], at: datetime) -> AnalystContext | None:
    active = [c for c in contexts if c.is_active(at)]
    return max(active, key=lambda c: c.published_at) if active else None


@dataclass
class ShadowReport:
    trades: int
    trades_with_context: int
    excluded: int
    resized: int
    actual_net: float
    restricted_net: float
    days: int
    by_day: dict[str, float]

    @property
    def value(self) -> float:
        return self.restricted_net - self.actual_net


def evaluate(trips: list[RoundTrip], contexts: list[AnalystContext]) -> ShadowReport:
    actual = restricted = 0.0
    with_ctx = excluded = resized = 0
    by_day: dict[str, float] = defaultdict(float)
    for t in trips:
        ctx = active_context(contexts, t.entry_at)
        r = t.net
        if ctx is not None:
            with_ctx += 1
            if t.symbol in ctx.exclude:
                r = 0.0
                excluded += 1
            else:
                m = ctx.size_multiplier * ctx.symbol_size_multipliers.get(t.symbol, 1.0)
                if m < 1.0:
                    resized += 1
                r = t.net * m
        actual += t.net
        restricted += r
        by_day[t.entry_at.astimezone(IST).date().isoformat()] += r - t.net
    return ShadowReport(len(trips), with_ctx, excluded, resized, actual, restricted, len(by_day), dict(sorted(by_day.items())))
