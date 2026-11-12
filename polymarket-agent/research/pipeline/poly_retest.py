"""Polymarket full re-test (11 Nov 2026). Research only.

  python -I research/pipeline/poly_retest.py <poly_full_dir>

Sample: EVERY resolved binary market whose scheduled end fell between 15 Aug
and 8 Nov 2026, any size (the old test used only markets over ~$98k final
volume, which is why the live underdog book failed).

Every rule uses only what is known at the moment of the bet:
  price of each side then, hours until the scheduled end (or until the game
  starts for matches), market type from the question, and how many price
  updates the market had in the 24 h before (activity). Final volume is not
  used anywhere.
A bet buys the chosen side at its price + 1c (2c for the double-cost gate),
holds to resolution: won -> 1/paid - 1, lost -> -1. One bet per event.

Gates: >=100 bets, both halves profitable, >=60% of 2-week blocks profitable,
profitable at double costs, >=60% of neighbours profitable, walk-forward by
block, deflated Sharpe (luck bar), holdout = markets ending after 18 Oct,
Monte Carlo 95%."""
import json, math, re, sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from statistics import NormalDist
import numpy as np

SRC = Path(sys.argv[1])
ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "reports" / "research"
N = NormalDist()
HOLD_FROM = datetime(2026, 10, 18, tzinfo=timezone.utc).timestamp()


def ts(s):
    if not s:
        return None
    s = s.replace("Z", "+00:00").replace(" ", "T")
    try:
        d = datetime.fromisoformat(s)
    except ValueError:
        d = datetime.fromisoformat(s.split("+")[0])
    if d.tzinfo is None:
        d = d.replace(tzinfo=timezone.utc)
    return d.timestamp()


def kind(m):
    q = (m["q"] or "")
    ql = q.lower()
    sr = (m.get("series") or "").lower()
    if re.search(r"\b(m15|m25|w15|w35|w50|w75|w100|itf|challenger)\b", ql) or re.match(r"^[a-z .'-]+( \d)?:\s", ql) and " vs" in ql and m.get("gs"):
        # "Monastir: A vs B", "M15 Antalya: ..." small tennis / minor events
        return "minor tennis" if re.search(r"(m15|m25|w15|w35|w50|w75|w100|itf|challenger|\d:)", ql) or "tennis" in sr else "match"
    if any(k in ql for k in ("counter-strike", "cs2", "league of legends", "lol:", "dota", "valorant", "(bo1)", "(bo3)", "(bo5)")) or any(k in sr for k in ("counter-strike", "lol", "dota", "valorant", "esports")):
        return "esports"
    if any(k in ql for k in ("bitcoin", "ethereum", "btc", "eth ", "solana", "xrp", "up or down", "crypto")):
        return "crypto"
    if m.get("gs") or " vs" in ql:
        return "match"
    if any(k in ql for k in ("election", "president", "trump", "senate", "governor", "minister", "parliament", "vote")):
        return "politics"
    return "other"


rows = []
for f in sorted(SRC.glob("m_*.json")):
    for m in json.loads(f.read_text()):
        hf = SRC / "h" / f"{m['tok'][:40]}.json"
        if not hf.exists():
            continue
        h = json.loads(hf.read_text())
        if len(h) < 3:
            continue
        end, closed = ts(m["end"]), ts(m["closed"]) or ts(m["end"])
        if end is None:
            continue
        t_ = np.array([x["t"] for x in h], float)
        p_ = np.array([float(x["p"]) for x in h], float)
        rows.append(dict(end=end, closed=closed, gs=ts(m.get("gs")), created=ts(m.get("created")), yes=bool(m["yes"]),
                         ev=m.get("ev") or m["id"], kind=kind(m), t=t_, p=p_))
print("markets with history:", len(rows), flush=True)
kinds = sorted({r["kind"] for r in rows})
print({k: sum(r["kind"] == k for r in rows) for k in kinds}, flush=True)


def bets(sel, lo, hi, H, anchor="end", act_min=0, move=None, cost=0.01):
    """move: None | ('up'|'down', thr) = the chosen side's price change over the 24 h before"""
    out = {}
    for r in rows:
        if not sel(r):
            continue
        ref = r["end"] if anchor == "end" else r["gs"]
        if ref is None:
            continue
        t = ref - H * 3600
        if t >= r["closed"] or (r["created"] and t <= r["created"]):
            continue
        i = np.searchsorted(r["t"], t, "right") - 1
        if i < 0 or t - r["t"][i] > 6 * 3600:
            continue
        act = int(((r["t"] > t - 86400) & (r["t"] <= t)).sum())
        if act < act_min:
            continue
        p = r["p"][i]
        j = np.searchsorted(r["t"], t - 86400, "right") - 1
        p0 = r["p"][j] if j >= 0 else np.nan
        for price, won, prev in ((p, r["yes"], p0), (1 - p, not r["yes"], 1 - p0)):
            if not (lo <= price < hi):
                continue
            if move:
                d = price - prev
                if not np.isfinite(d) or (move[0] == "up" and d < move[1]) or (move[0] == "down" and d > -move[1]):
                    continue
            if r["ev"] in out:
                continue
            paid = min(price + cost, 0.99)
            out[r["ev"]] = (t, (1 / paid - 1) if won else -1.0)
    return sorted(out.values())


def block(t):
    return int((t - datetime(2026, 8, 8, tzinfo=timezone.utc).timestamp()) // (14 * 86400))


V = []
BANDS = [(0.03, 0.10), (0.10, 0.20), (0.20, 0.30), (0.30, 0.40), (0.40, 0.50), (0.50, 0.60), (0.60, 0.70), (0.70, 0.80), (0.80, 0.90), (0.90, 0.97)]
HS = [1, 6, 24, 72, 168]
SEL = {"all": lambda r: True, **{k: (lambda k: lambda r: r["kind"] == k)(k) for k in kinds}}
for sname, sel in SEL.items():
    for lo, hi in BANDS:
        for H in HS:
            for act_min, alab in ((0, "any activity"), (6, "6+ price updates in the last day")):
                for move, mlab in ((None, ""), (("up", 0.03), ", side rose 3c+ in the last day"), (("down", 0.03), ", side fell 3c+ in the last day")):
                    b = bets(sel, lo, hi, H, "end", act_min, move)
                    b2 = bets(sel, lo, hi, H, "end", act_min, move, cost=0.02)
                    side = "underdog" if hi <= 0.5 else "favourite"
                    V.append(dict(name=f"{sname} {int(lo*100)}-{int(hi*100)}c {H}h act{act_min} {move}", b=b, b2=b2,
                                  group=f"{sname} {side} act{act_min} {move[0] if move else 'none'}",
                                  desc=f"{sname}: buy the side priced {int(lo*100)}-{int(hi*100)}c, {H} h before the scheduled end, {alab}{mlab}"))
    # matches: time from the game start, which the live bot ignored
    if sname in ("all", "match", "minor tennis", "esports"):
        for lo, hi in BANDS:
            for H in (1, 3, 12):
                b = bets(lambda r: sel(r) and r["gs"] is not None, lo, hi, H, "gs")
                b2 = bets(lambda r: sel(r) and r["gs"] is not None, lo, hi, H, "gs", cost=0.02)
                V.append(dict(name=f"{sname} {int(lo*100)}-{int(hi*100)}c gs-{H}h", b=b, b2=b2,
                              group=f"{sname} {'underdog' if hi <= 0.5 else 'favourite'} gamestart",
                              desc=f"{sname}: buy the side priced {int(lo*100)}-{int(hi*100)}c, {H} h before the game starts"))
print("variants:", len(V), flush=True)

# ------------------------------------------------------------------ gates
blocks = sorted({block(t) for v in V for t, _ in v["b"]})
for v in V:
    b = v["b"]
    rs = np.array([x for _, x in b]) if b else np.zeros(0)
    tt = np.array([t for t, _ in b]) if b else np.zeros(0)
    res = tt < HOLD_FROM
    R = rs[res]
    v["n"] = int(res.sum())
    mid = len(R) // 2
    v["tot"], v["h1"], v["h2"] = R.sum(), R[:mid].sum(), R[mid:].sum()
    v["avg"] = R.mean() if len(R) else 0
    v["win"] = (R > 0).mean() if len(R) else 0
    by = np.array([R[np.array([block(t) for t in tt[res]]) == k].sum() if len(R) else 0 for k in blocks])
    v["by"] = by
    v["bpos"] = (by[[i for i, k in enumerate(blocks) if k < block(HOLD_FROM)]] > 0).mean() if len(by) else 0
    r2 = np.array([x for t, x in v["b2"] if t < HOLD_FROM])
    v["c2"] = r2.sum() if len(r2) else 0
    v["hold"], v["hold_n"] = rs[~res].sum(), int((~res).sum())
    sd = R.std() if len(R) > 1 else 0
    v["sr"] = R.mean() / sd if sd > 0 else 0
    z = (R - R.mean()) / sd if sd > 0 else R * 0
    v["sk"], v["ku"] = (z ** 3).mean() if len(z) else 0, (z ** 4).mean() if len(z) else 3
    v["R"] = R
ok = [v for v in V if v["n"] >= 100]
srs = np.array([v["sr"] for v in ok])
e = 0.5772156649
bar = lambda k: math.sqrt(srs.var()) * ((1 - e) * N.inv_cdf(1 - 1 / k) + e * N.inv_cdf(1 - 1 / (k * math.e)))
groups = defaultdict(list)
for v in V:
    groups[v["group"]].append(v)
res_blocks = [i for i, k in enumerate(blocks) if k < block(HOLD_FROM)]
for g in groups.values():
    g5 = np.mean([x["tot"] > 0 for x in g if x["n"] >= 30] or [0]) >= 0.6
    wf = 0.0
    for k in res_blocks[1:]:
        best = max(g, key=lambda x: x["by"][:k].sum())
        wf += best["by"][k]
    for x in g:
        x["g5"], x["g6"] = bool(g5), wf > 0
# effective trials: correlation of per-block results
M = np.array([v["by"][res_blocks] for v in ok])
M = M[M.std(1) > 0]
Z = (M - M.mean(1, keepdims=True)) / M.std(1, keepdims=True)
ev = np.linalg.svd(Z, compute_uv=False) ** 2
neff = max(2, int(round(ev.sum() ** 2 / (ev ** 2).sum())))
B_all, B_eff = bar(len(ok)), bar(neff)
rng = np.random.default_rng(5)
for v in V:
    n = max(v["n"], 2)
    den = math.sqrt(max(1 - v["sk"] * v["sr"] + (v["ku"] - 1) / 4 * v["sr"] ** 2, 1e-9))
    v["dsr"] = N.cdf((v["sr"] - B_all) * math.sqrt(n - 1) / den)
    v["dsr_eff"] = N.cdf((v["sr"] - B_eff) * math.sqrt(n - 1) / den)
    v["g1"], v["g2"], v["g3"], v["g4"] = v["n"] >= 100, v["h1"] > 0 and v["h2"] > 0, v["bpos"] >= 0.6, v["c2"] > 0
    v["g7"], v["g8"] = v["dsr"] >= 0.95, v["hold"] > 0 and v["hold_n"] >= 20
    base = v["g1"] and v["g2"] and v["g3"] and v["g4"] and v["g5"] and v["g6"] and v["g8"]
    v["mc"] = float((rng.choice(v["R"], (5000, len(v["R"]))).sum(1) > 0).mean()) if base else 0
    v["g9"] = v["mc"] >= 0.95
    v["pass"] = base and v["g7"] and v["g9"]
    v["pass_soft"] = base and v["dsr_eff"] >= 0.95 and v["g9"]

funnel, alive = [], [True] * len(V)
for k in ("g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8", "g9"):
    alive = [a and v[k] for a, v in zip(alive, V)]
    funnel.append((k, sum(v[k] for v in V), sum(alive)))

# ------------------------------------------------------------------ report
L = ["# Polymarket full re-test\n", f"Every resolved market with a scheduled end from 15 Aug to 8 Nov 2026, any size: {len(rows):,} markets.",
     "Rules use only what was known when the bet would be placed. Holdout: markets ending after 18 Oct.\n",
     "Market types: " + ", ".join(f"{k} {sum(r['kind'] == k for r in rows):,}" for k in kinds) + "\n",
     f"{len(V)} rule variants. Luck bar: Sharpe per bet {B_all:.3f} (strict), {B_eff:.3f} (softer, about {neff} distinct ideas).\n",
     "| Gate (on top of the ones before) | Pass alone | Still standing |", "|---|---|---|"]
GN = ["1. At least 100 bets", "2. Both halves profitable", "3. 60% of 2-week blocks profitable", "4. Profitable at 2c cost",
      "5. 60% of neighbours profitable", "6. Walk-forward profitable", "7. Beats the luck bar", "8. Holdout profitable (20+ bets)", "9. Monte Carlo 95%"]
for (k, a, b), g in zip(funnel, GN):
    L.append(f"| {g} | {a} | {b} |")
P = [v for v in V if v["pass"]]
PS = [v for v in V if v["pass_soft"] and not v["pass"]]
L.append(f"\n**Pass every gate: {len(P)}. With the softer luck bar only: {len(PS)}.**\n")
fmt = lambda v: f"| {v['desc']} | {v['n']} | {v['win']:.0%} | {v['avg']*100:+.1f}% | {v['h1']:+.1f} / {v['h2']:+.1f} | {v['bpos']:.0%} | {v['c2']:+.1f} | {v['hold_n']} | {v['hold']:+.1f} | {v['dsr']:.2f} | {v['mc']:.0%} |"
head = "| Rule | Bets | Won | Avg per bet | Halves ($ per $1 bets) | Blocks up | At 2c | Holdout bets | Holdout | Luck score | Monte Carlo |\n|---|---|---|---|---|---|---|---|---|---|---|"
if P or PS:
    L.append("### Candidates\n\n" + head)
    L += [fmt(v) for v in (P + PS)[:20]]
L.append("\n### Closest rules (pass gates 1-6, best research Sharpe)\n\n" + head)
near = sorted([v for v in V if v["g1"] and v["g2"] and v["g3"] and v["g4"] and v["g5"] and v["g6"]], key=lambda v: -v["sr"])[:15]
L += [fmt(v) for v in near] or ["| none | | | | | | | | | | |"]
L.append("\n### The live underdog rule on the full sample\n\n" + head)
for v in V:
    if v["name"] in ("all 20-30c 168h act0 None", "minor tennis 20-30c 168h act0 None", "match 20-30c 168h act0 None", "other 20-30c 168h act0 None"):
        L.append(fmt(v))
L.append("\n### Average result per bet by market type and price (168 h before the end, any activity)\n\n| Type | " + " | ".join(f"{int(lo*100)}-{int(hi*100)}c" for lo, hi in BANDS) + " |\n|---|" + "---|" * len(BANDS))
for s in SEL:
    cells = []
    for lo, hi in BANDS:
        v = next(x for x in V if x["name"] == f"{s} {int(lo*100)}-{int(hi*100)}c 168h act0 None")
        cells.append(f"{v['avg']*100:+.0f}% ({v['n']})" if v["n"] >= 20 else "-")
    L.append(f"| {s} | " + " | ".join(cells) + " |")
OUT.mkdir(parents=True, exist_ok=True)
(OUT / "poly_retest_report.md").write_text("\n".join(L) + "\n", encoding="utf-8")
print("\n".join(L[:40]))
print("report:", OUT / "poly_retest_report.md")
