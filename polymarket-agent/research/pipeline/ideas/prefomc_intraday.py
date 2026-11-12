"""Backlog idea 2a (12 Nov 2026): is the pre-FOMC drift before the 14:00 statement?

Rule tested: buy QQQ at the close of the trading day before a Fed decision,
sell on the decision day at 13:55 New York (the last 5-minute bar before the
14:00 statement). Compared with holding to the decision day's close (the live
pre-FOMC book's rule) and with exits at 12:00, 13:00 and 15:00.
Data: Alpaca SIP 5-minute QQQ bars 2016-2026 (research/data/qqq_5m.json),
FOMC decision dates from the scratchpad list. Costs 2 bp a side.
Placebo: the same hold on the same weekday one week later.

  python -I research/pipeline/ideas/prefomc_intraday.py <scratchpad_dir>
"""
import json, sys
from datetime import date, timedelta
from pathlib import Path
import numpy as np

S = Path(sys.argv[1])
here = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(here))
sys.argv = [sys.argv[0], str(S / "d20")]
import reel_strats as RS  # noqa: E402

fomc = sorted(json.loads((S / "fomc_dates.json").read_text()))
days = RS.orb_days(json.loads((here.parents[1] / "research" / "data" / "qqq_5m.json").read_text()))
order = sorted(days)
idx = {d: i for i, d in enumerate(order)}
COST = 0.0002


def last_close(d):
    b = days[d]
    return b[max(b)][3]


def at_slot(d, slot):
    b = days[d]
    ks = [k for k in b if k <= slot]
    return b[max(ks)][3] if ks else None


def run(dates, exit_slot, cost=COST):
    out = []
    for f in dates:
        if f not in idx or idx[f] == 0:
            continue
        prev = order[idx[f] - 1]
        entry = last_close(prev)
        ex = last_close(f) if exit_slot is None else at_slot(f, exit_slot)
        if ex:
            out.append((f, ex / entry - 1 - 2 * cost))
    return out


# 5-minute slot index from 9:30: 12:00 -> 30, 13:00 -> 42, 13:55 bar closes 14:00 -> slot 53, 15:00 -> 66
EXITS = {"12:00": 29, "13:00": 41, "14:00 (before statement)": 53, "15:00": 65, "close": None}
f_in = [f for f in fomc if f in idx]
print(f"Fed decision days in the QQQ 5-min data: {len(f_in)} ({f_in[0]} .. {f_in[-1]})")
rows = {}
for name, slot in EXITS.items():
    rows[name] = run(f_in, slot)
placebo = [(date.fromisoformat(f) + timedelta(days=7)).isoformat() for f in f_in]
L = ["| Exit on decision day | Trades | Won | Avg per trade | Total | Holdout (last 25%) | Verdict | Gates failed |", "|---|---|---|---|---|---|---|---|"]
for name, slot in EXITS.items():
    nb = [v for k, v in rows.items() if k != name]
    g = RS.gates(rows[name], run(f_in, slot, 0.0004), nb)
    L.append(f"| {name} | {g['n']} | {g.get('win', 0):.0%} | {g.get('avg', 0):+.3f}% | {g.get('total', 0):+.1f}% | {g.get('holdout', 0):+.1f}% | {g['verdict']} | {g['why']} |")
pl = run([p for p in placebo if p in idx], 53)
L.append(f"\nPlacebo (same hold one week later, exit 14:00): {len(pl)} trades, avg {np.mean([x for _, x in pl])*100:+.3f}%")
best = rows["14:00 (before statement)"]
r = np.sort([x for _, x in best])
L.append(f"14:00 exit without best 3: {r[:-3].sum()*100:+.1f}% (of {r.sum()*100:+.1f}%); without best 10: {r[:-10].sum()*100:+.1f}%")
pre = {d: x for d, x in rows["14:00 (before statement)"]}
post = {d: x for d, x in rows["close"]}
diff = [post[d] - pre[d] for d in pre if d in post]
L.append(f"Statement-to-close part alone (close minus 14:00): avg {np.mean(diff)*100:+.3f}% per meeting, std {np.std(diff)*100:.2f}%")
L.append("Published (Lucca & Moench 2015): S&P 500 rose about 0.49% on average in the 24 h before announcements, 1994-2011, most of it before 14:00.")
print("\n".join(L))
