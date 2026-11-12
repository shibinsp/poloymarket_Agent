"""Backlog ideas 1 and 2 (11 Nov 2026). Research only.

1. Pre-FOMC drift: buy SPY at the close the day before a Fed decision, sell at
   the decision day's close. Neighbours: QQQ, DIA, IWM; entry 2 days before.
   Placebo: the same hold on the same weekday one week later.
2. India Turnaround: if NIFTY's Friday close is below its 25-day average, buy
   Monday's open, sell Tuesday's close (index proxy for NIFTYBEES, 5 bp a side).
   Neighbours: SMA 20/30, BANKNIFTY. Placebo: the same rule Wed -> Thu.

  python -I research/pipeline/ideas/fomc_and_india_turnaround.py <scratchpad_dir>
"""
import json, sys
from datetime import date, timedelta
from pathlib import Path
import numpy as np

S = Path(sys.argv[1])
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.argv = [sys.argv[0], str(S / "d20")]
import reel_strats as RS  # noqa: E402  (gates())

fomc = set(json.loads((S / "fomc_dates.json").read_text()))


def load(folder, sym):
    rows = json.loads((S / folder / f"{sym}.json").read_text())
    return [r[0] for r in rows], np.array([r[1:5] for r in rows], float)


def pre_fomc(sym, days_before=1, cost=0.0002, shift_days=0):
    d, a = load("d20", sym)
    idx = {x: i for i, x in enumerate(d)}
    out = []
    for f in sorted(fomc):
        f2 = (date.fromisoformat(f) + timedelta(days=shift_days)).isoformat()
        if f2 not in idx:
            continue
        i = idx[f2]
        if i - days_before < 0:
            continue
        out.append((f2, a[i, 3] / a[i - days_before, 3] - 1 - 2 * cost))
    return out


def india_turn(sym="NSEI", sma=25, cost=0.0005, buy_wd=0):
    d, a = load("din", sym)
    o, c = a[:, 0], a[:, 3]
    wd = [date.fromisoformat(x).weekday() for x in d]
    out = []
    for i in range(sma, len(d) - 1):
        if wd[i] != buy_wd or wd[i + 1] != buy_wd + 1:
            continue
        if c[i - 1] < c[i - sma:i].mean():
            out.append((d[i], c[i + 1] / o[i] - 1 - 2 * cost))
    return out


def line(name, g, extra=""):
    if "gates" not in g:
        return f"| {name} | {g['n']} | | | | | | | | | FAIL | {g['why']} |"
    return (f"| {name} | {g['n']} | {g['win']:.1%} | {g['avg']:+.3f}% | {g['pf']:.2f} | {g['total']:+.1f}% | {g['holdout']:+.1f}% | {g['years_up']} | "
            f"{g['nb_pos']} | {g['dsr']:.2f} / {g['mc']:.0%} | **{g['verdict']}** | {g['why']}{extra} |")


L = ["| Idea | Trades | Win | Avg | PF | Total | Holdout | Years up | Neighbours + | Luck / MC | Verdict | Gates failed / notes |", "|---|---|---|---|---|---|---|---|---|---|---|---|"]
b = pre_fomc("SPY")
g = RS.gates(b, pre_fomc("SPY", cost=0.0004), [pre_fomc(s) for s in ("QQQ", "DIA", "IWM")] + [pre_fomc("SPY", 2)])
r = np.sort([x for _, x in b])
pl = pre_fomc("SPY", shift_days=7)
L.append(line("1. Pre-FOMC drift, SPY close->close", g,
              f"; without best 3: {r[:-3].sum()*100:+.1f}%; placebo (a week later): {np.mean([x for _, x in pl])*100:+.3f}%/trade over {len(pl)}"))
b = india_turn()
g = RS.gates(b, india_turn(cost=0.001), [india_turn(sma=20), india_turn(sma=30), india_turn("NSEBANK")])
r = np.sort([x for _, x in b])
pl = india_turn(buy_wd=2)
L.append(line("2. India Turnaround, NIFTY Mon open->Tue close", g,
              f"; without best 3: {r[:-3].sum()*100:+.1f}%; placebo (Wed->Thu): {np.mean([x for _, x in pl])*100:+.3f}%/trade over {len(pl)}"))
print("\n".join(L))
