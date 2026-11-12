"""Long-history check of the live spread book (11 Nov 2026). Research only.

  python -I research/pipeline/spread_history.py [download|run|all]

Live rule (src/us/spreads.rs): at 11:00 New York, for SPY and QQQ, sell the
first same-day call strike at least 0.6% above the price, buy the call $2
higher, for a credit; no trade if the credit is under $0.05; buy back at 15:50.

Data: Alpaca SIP 5-minute bars for SPY/QQQ and real 1-minute option trade bars
for the strikes around the money, Feb 2024 - Nov 2026 (Alpaca's option history
starts in Feb 2024). Option bars are trades, not quotes, so each leg is
charged half a spread: max(3% of its price, $0.02), against us; double that
for the 2x-cost gate. A leg with no trade near 15:50 is valued at its
intrinsic value plus $0.01.
Neighbours: 0.4% / 0.8% away, $1 / $3 wide. Holdout: the last 25% of days."""
import json, math, sys, time, urllib.parse, urllib.request
from collections import defaultdict
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor
import numpy as np

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "research" / "data" / "spreads"
OUT = ROOT / "reports" / "research"
START = "2024-02-01"
END = "2026-11-11"
OTMS = (0.004, 0.006, 0.008)
WIDTHS = (1, 2, 3)

env = {}
for line in (ROOT / ".env").read_text(encoding="utf-8").splitlines():
    if "=" in line and not line.lstrip().startswith("#"):
        k, v = line.split("=", 1); env[k.strip()] = v.strip().strip('"').strip("'")
HDR = {"APCA-API-KEY-ID": env["ALPACA_API_KEY_ID"], "APCA-API-SECRET-KEY": env["ALPACA_API_SECRET_KEY"]}


def http(url):
    for a in range(5):
        try:
            return json.load(urllib.request.urlopen(urllib.request.Request(url, headers=HDR), timeout=60))
        except Exception:  # noqa: BLE001
            time.sleep(2 * (a + 1))
    return None


def ny_open(d):
    mar = date(d.year, 3, 8); mar += timedelta(days=(6 - mar.weekday()) % 7)
    nov = date(d.year, 11, 1); nov += timedelta(days=(6 - nov.weekday()) % 7)
    return datetime(d.year, d.month, d.day, 13 if mar <= d < nov else 14, 30, tzinfo=timezone.utc)


def underlying(sym):
    f = DATA / f"{sym}_5m.json"
    if f.exists():
        return json.loads(f.read_text())
    rows, tok = [], None
    while True:
        q = dict(timeframe="5Min", start=f"{START}T00:00:00Z", end=f"{END}T00:00:00Z", feed="sip", limit=10000, adjustment="raw")
        if tok:
            q["page_token"] = tok
        r = http(f"https://data.alpaca.markets/v2/stocks/{sym}/bars?" + urllib.parse.urlencode(q))
        rows += [[b["t"], b["o"], b["h"], b["l"], b["c"]] for b in (r or {}).get("bars") or []]
        tok = (r or {}).get("next_page_token")
        if not tok:
            break
    f.write_text(json.dumps(rows))
    return rows


def by_day(rows):
    out = defaultdict(dict)
    for t, o, h, l, c in rows:
        dt = datetime.fromisoformat(t.replace("Z", "+00:00"))
        s = int((dt - ny_open(dt.date())).total_seconds() // 300)
        if 0 <= s < 78:
            out[dt.date().isoformat()][s] = (o, h, l, c)
    return out


def occ(u, d, k):
    return f"{u}{d[2:4]}{d[5:7]}{d[8:10]}C{int(round(k * 1000)):08d}"


def options_day(u, d, p11):
    f = DATA / f"{u}_{d}.json"
    if f.exists():
        return json.loads(f.read_text())
    k0 = math.ceil(p11 * 1.003)
    ks = list(range(k0, math.ceil(p11 * 1.008) + 4))
    syms = [occ(u, d, k) for k in ks]
    op = ny_open(date.fromisoformat(d))
    q = dict(symbols=",".join(syms), timeframe="1Min", start=(op + timedelta(minutes=85)).strftime("%Y-%m-%dT%H:%M:%SZ"),
             end=(op + timedelta(minutes=390)).strftime("%Y-%m-%dT%H:%M:%SZ"), limit=10000)
    bars, tok = {}, None
    while True:
        if tok:
            q["page_token"] = tok
        r = http("https://data.alpaca.markets/v1beta1/options/bars?" + urllib.parse.urlencode(q))
        if r is None:
            return None
        for s, bs in (r.get("bars") or {}).items():
            bars.setdefault(s, []).extend([[b["t"], b["o"], b["c"], b["v"]] for b in bs])
        tok = r.get("next_page_token")
        if not tok:
            break
    f.write_text(json.dumps(bars))
    return bars


def download():
    DATA.mkdir(parents=True, exist_ok=True)
    jobs = []
    for u in ("SPY", "QQQ"):
        days = by_day(underlying(u))
        print(u, "days", len(days), flush=True)
        for d, b in sorted(days.items()):
            if 17 in b:
                jobs.append((u, d, b[17][3]))      # 10:55 bar close = price at 11:00
    done = 0
    with ThreadPoolExecutor(6) as ex:
        for _ in ex.map(lambda j: options_day(*j), jobs):
            done += 1
            if done % 200 == 0:
                print("option days", done, "of", len(jobs), flush=True)
    print("download done", len(jobs), flush=True)


def price_at(bars, t, window_min, field):
    """first bar at or after t within window: field 1=open 2=close"""
    for ts, o, c, v in bars:
        bt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
        if t <= bt <= t + timedelta(minutes=window_min):
            return o if field == 1 else c
    return None


def replay(otm=0.006, width=2, cost_mult=1.0, min_credit=0.05):
    hs = lambda p: max(0.03 * p, 0.02) * cost_mult
    out = []
    for u in ("SPY", "QQQ"):
        days = by_day(underlying(u))
        for d, b in sorted(days.items()):
            if 17 not in b or 75 not in b:
                continue
            f = DATA / f"{u}_{d}.json"
            if not f.exists():
                continue
            bars = json.loads(f.read_text())
            if not bars:
                continue
            p = b[17][3]
            K = math.ceil(p * (1 + otm)); KL = K + width
            sa, la = bars.get(occ(u, d, K)), bars.get(occ(u, d, KL))
            if not sa or not la:
                continue
            op = ny_open(date.fromisoformat(d))
            t_in, t_out = op + timedelta(minutes=90), op + timedelta(minutes=380)
            s_in, l_in = price_at(sa, t_in, 10, 1), price_at(la, t_in, 10, 1)
            if s_in is None or l_in is None:
                continue
            credit = (s_in - hs(s_in)) - (l_in + hs(l_in))
            if credit < min_credit:
                continue
            S = b[76][0] if 76 in b else b[75][3]      # underlying near 15:50
            s_out, l_out = price_at(sa, t_out, 5, 1), price_at(la, t_out, 5, 1)
            s_out = s_out if s_out is not None else max(0.0, S - K) + 0.01
            l_out = l_out if l_out is not None else max(0.0, S - KL)
            debit = min(max((s_out + hs(s_out)) - max(l_out - hs(l_out), 0.0), 0.0), width)
            out.append((d, u, 100 * (credit - debit), credit, debit))
    return out


def run():
    base = replay()
    by_date = defaultdict(float)
    for d, u, pnl, c, db in base:
        by_date[d] += pnl
    days = sorted(by_date)
    v = np.array([by_date[d] for d in days])
    cut = int(len(v) * 0.75)
    R, H = v[:cut], v[cut:]
    mid = len(R) // 2
    months = defaultdict(float)
    for d in days[:cut]:
        months[d[:7]] += by_date[d]
    mpos = np.mean([x > 0 for x in months.values()])
    x2 = replay(cost_mult=2.0)
    c2 = sum(p for d, u, p, c, db in x2 if d < days[cut])
    nbs = {}
    for o in OTMS:
        for w in WIDTHS:
            if (o, w) == (0.006, 2):
                continue
            r = replay(o, w)
            nbs[(o, w)] = sum(p for d, u, p, c, db in r if d < days[cut])
    # walk-forward over the settings grid by month
    grid = {(0.006, 2): base, **{k: replay(*k) for k in nbs}}
    gm = {k: defaultdict(float) for k in grid}
    for k, rows in grid.items():
        for d, u, p, c, db in rows:
            gm[k][d[:7]] += p
    allm = sorted({m for g in gm.values() for m in g})
    wf = 0.0
    for i in range(1, len(allm)):
        best = max(grid, key=lambda k: sum(gm[k][m] for m in allm[:i]))
        wf += gm[best][allm[i]]
    rng = np.random.default_rng(9)
    mc = float((rng.choice(v, (5000, len(v))).sum(1) > 0).mean())
    eq = np.cumsum(v)
    dd = float((eq - np.maximum.accumulate(eq)).min())
    worst = sorted(base, key=lambda x: x[2])[:8]
    trades = len(base)
    win = np.mean([p > 0 for _, _, p, _, _ in base])
    sr = R.mean() / R.std() * math.sqrt(252) if R.std() > 0 else 0
    gates = {"1 100+ trades": trades >= 100, "2 both halves +": R[:mid].sum() > 0 and R[mid:].sum() > 0,
             "3 60% of months +": mpos >= 0.6, "4 profit at 2x costs": c2 > 0,
             "5 nearby settings 60% +": np.mean([x > 0 for x in nbs.values()]) >= 0.6, "6 walk-forward +": wf > 0,
             "8 holdout +": H.sum() > 0, "9 Monte Carlo 95%": mc >= 0.95}
    L = ["# Spread book, long history (real option prices, Feb 2024 - Nov 2026)\n",
         "Live rule: 11:00 NY sell the first call >= 0.6% above SPY/QQQ, buy $2 higher, skip credits under $0.05, buy back 15:50. Each leg charged max(3%, $0.02) against us.\n",
         f"- Spreads: **{trades}** on {len(days)} days ({days[0]} to {days[-1]}), {win:.0%} won",
         f"- Total **${v.sum():+,.0f}** per 1 spread each of SPY and QQQ; per trading day ${v.mean():+.2f}; yearly Sharpe {sr:.2f}",
         f"- Research ({days[0]} to {days[cut-1]}): ${R.sum():+,.0f}, halves ${R[:mid].sum():+,.0f} / ${R[mid:].sum():+,.0f}, months up {mpos:.0%}",
         f"- **Holdout** ({days[cut]} to {days[-1]}): ${H.sum():+,.0f} over {len(H)} days",
         f"- At double costs (research): ${c2:+,.0f}",
         f"- Worst day ${v.min():+,.0f}; worst fall from a peak ${dd:+,.0f}; Monte Carlo {mc:.0%}",
         f"- Walk-forward over the 9 settings: ${wf:+,.0f}\n",
         "| Gate | Result |\n|---|---|"] + [f"| {k} | {'pass' if ok else '**FAIL**'} |" for k, ok in gates.items()]
    L.append("\n### Nearby settings (research period)\n\n| Distance | Width | Research P&L |\n|---|---|---|")
    L.append(f"| 0.6% | $2 (live) | ${R.sum():+,.0f} |")
    for (o, w), s in sorted(nbs.items()):
        L.append(f"| {o:.1%} | ${w} | ${s:+,.0f} |")
    L.append("\n### By year\n\n| Year | Spreads | P&L |\n|---|---|---|")
    for y in sorted({d[:4] for d in days}):
        L.append(f"| {y} | {sum(1 for d, *_ in base if d[:4] == y)} | ${sum(p for d, u, p, c, db in base if d[:4] == y):+,.0f} |")
    L.append("\n### Worst 8 spreads\n\n| Day | Fund | P&L | Credit | Paid back |\n|---|---|---|---|---|")
    for d, u, p, c, db in worst:
        L.append(f"| {d} | {u} | ${p:+.0f} | ${c:.2f} | ${db:.2f} |")
    L.append(f"\nVerdict: **{'PASS' if all(gates.values()) else 'FAIL'}** (failed: {', '.join(k for k, ok in gates.items() if not ok) or 'none'}).")
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "spread_history_report.md").write_text("\n".join(L) + "\n", encoding="utf-8")
    print("\n".join(L))


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "all"
    if cmd in ("download", "all"):
        download()
    if cmd in ("run", "all"):
        run()
