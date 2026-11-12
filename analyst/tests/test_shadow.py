import sqlite3
from datetime import datetime, timedelta
from zoneinfo import ZoneInfo

import pytest

from moneyai_analyst.schema import AnalystContext
from moneyai_analyst.shadow import RoundTrip, evaluate, intraday_costs, load_fills, pair_round_trips

IST = ZoneInfo("Asia/Kolkata")
T0 = datetime(2026, 11, 12, 10, 0, tzinfo=IST)


@pytest.mark.parametrize("value, expected", [(100_000, 83), (500_000, 223)])
def test_costs_match_architecture_doc_table(value, expected):
    # Architecture doc section 8: Rs 1L round trip ~ Rs 83, Rs 5L ~ Rs 223.
    assert intraday_costs(value, value) == pytest.approx(expected, abs=1.5)


def test_stt_is_charged_on_sell_side_only():
    assert intraday_costs(100_000, 0) < intraday_costs(0, 100_000)


def ctx(exclude=(), mult=1.0, per=None, at=T0 - timedelta(minutes=5)):
    return AnalystContext(run_id="r", published_at=at, expires_at=at + timedelta(minutes=20), exclude=list(exclude),
                          size_multiplier=mult, symbol_size_multipliers=per or {}, schema_version="1",
                          prompt_version="p", model="m")


def test_excluded_losing_trade_is_positive_value():
    trips = [RoundTrip("INFY", T0, 10, -500.0)]
    rep = evaluate(trips, [ctx(exclude=["INFY"])])
    assert rep.excluded == 1 and rep.value == pytest.approx(500.0)


def test_multiplier_scales_result():
    trips = [RoundTrip("TCS", T0, 10, 1000.0)]
    rep = evaluate(trips, [ctx(mult=0.5, per={"TCS": 0.5})])
    assert rep.restricted_net == pytest.approx(250.0) and rep.value == pytest.approx(-750.0)


def test_expired_context_has_no_effect():
    trips = [RoundTrip("INFY", T0, 10, -500.0)]
    rep = evaluate(trips, [ctx(exclude=["INFY"], at=T0 - timedelta(minutes=30))])
    assert rep.trades_with_context == 0 and rep.value == 0


def test_fifo_pairing_and_engine_db_read(tmp_path):
    db = tmp_path / "engine.db"
    con = sqlite3.connect(db)
    con.execute("""CREATE TABLE india_sandbox_orders (id TEXT, client_order_id TEXT, venue_order_id TEXT, symbol TEXT,
        side TEXT, order_type TEXT, quantity INTEGER, price TEXT, trigger_price TEXT, status TEXT,
        fill_quantity INTEGER, average_fill_price TEXT, rejection_reason TEXT, created_at TEXT, updated_at TEXT)""")
    rows = [
        ("1", "BUY", 10, "100", "ACKNOWLEDGED", "2026-11-12T04:30:00Z"),
        ("2", "SELL", 4, "110", "ACKNOWLEDGED", "2026-11-12T05:00:00Z"),
        ("3", "SELL", 6, "90", "ACKNOWLEDGED", "2026-11-12T05:30:00Z"),
        ("4", "BUY", 5, "100", "REJECTED", "2026-11-12T06:00:00Z"),
        ("5", "BUY", 7, "100", "ACKNOWLEDGED", "2026-11-12T06:10:00Z"),  # acknowledged, nothing filled
    ]
    for oid, side, q, px, st, ts in rows:
        filled = 0 if oid == "5" else q
        con.execute("INSERT INTO india_sandbox_orders VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    (oid, oid, None, "SBIN", side, "LIMIT", q, px, None, st, filled, px, None, ts, ts))
    con.commit()
    con.close()
    fills = load_fills(str(db))
    assert [f.qty for f in fills] == [10, 4, 6]  # the unfilled order contributes nothing
    trips = pair_round_trips(fills)
    assert [(t.qty, round(t.net + intraday_costs(t.qty * 100, t.qty * (110 if t.qty == 4 else 90)), 2)) for t in trips] == [(4, 40.0), (6, -60.0)]
