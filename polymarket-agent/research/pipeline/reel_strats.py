"""Test the four strategies from the "italian_founder" reel (11 Nov 2026) with
their exact rules, nearby settings (+-20%, as the reel itself advises) and our
gates. Research only; prices only.

  python -I research/pipeline/reel_strats.py <d20_daily_dir>

1 Morning range breakout USDJPY: range 03:00-06:00 broker time (UTC+2, +3 in
  summer); at 06:00 buy stop at the high and sell stop at the low; stop at the
  other side; first fill cancels the other; flat 18:00. Yahoo hourly, 2 years.
2 Turnaround Tuesday, Dow 30 (DIA): if Friday's close is below the 25-day SMA,
  buy Monday's open, sell Tuesday's close. Daily 2001-2026.
3 Daily long bias, NASDAQ (QQQ): buy every open, sell the same day's close.
4 ORB NASDAQ (QQQ 5-min SIP 2016-2026): range 09:30-09:45; 09:45-15:00 enter
  on a 5-min close beyond it (one trade a day); stop beyond the other side +
  0.25 x ATR(14, 5-min); target 2 x range height from the breakout level;
  flat 15:55. A bar that touches both stop and target counts as a stop.
Costs: 2 bp a side on the ETFs, 1 pip (0.01 yen) round trip plus 0.3 pip
slippage a side on USDJPY. Returns are % of the position per trade.
Holdout: the last 25% of each test's trades (the reel's "keep the last 20-30%")."""
import json, math, sys, time, urllib.parse, urllib.request
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from statistics import NormalDist
import numpy as np

D20 = Path(sys.argv[1])
ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "research" / "data"
OUT = ROOT / "reports" / "research"
N = NormalDist()


def http(url, h=None):
    for a in range(4):
        try:
            return json.load(urllib.request.urlopen(urllib.request.Request(url, headers=h or {"User-Agent": "Mozilla/5.0"}), timeout=60))
        except Exception:  # noqa: BLE001
            time.sleep(2 * (a + 1))
    raise RuntimeError("download failed: " + url.split("?")[0])


def daily(sym):
    rows = json.loads((D20 / f"{sym}.json").read_text())
    return [r[0] for r in rows], np.array([r[1:5] for r in rows], float)


# ------------------------------------------------------------------ gates for a list of (date, return)
def gates(trades, trades2, neighbours):
    t = sorted(trades)
    r = np.array([x for _, x in t])
    n = len(r)
    if n < 10:
        return dict(n=n, verdict="FAIL", why="too few trades")
    cut = int(n * 0.75)
    R, Hd = r[:cut], r[cut:]
    mid = len(R) // 2
    yrs = sorted({d[:4] for d, _ in t[:cut]})
    by = np.array([sum(x for d, x in t[:cut] if d[:4] == y) for y in yrs])
    sd = R.std()
    sr = R.mean() / sd if sd > 0 else 0
    r2 = np.array([x for _, x in sorted(trades2)][:cut])
    nb = [np.sum([x for _, x in sorted(v)][: int(len(v) * 0.75)]) for v in neighbours if len(v) >= 10]
    rng = np.random.default_rng(3)
    mc = float((rng.choice(r, (5000, n)).sum(1) > 0).mean())
    # luck bar for the number of versions tried here (exact + neighbours)
    k = max(2, len(neighbours) + 1)
    e = 0.5772156649
    srs = [sr] + [np.mean(v_) / np.std(v_) if np.std(v_) > 0 else 0 for v_ in [np.array([x for _, x in sorted(v)][: int(len(v) * 0.75)]) for v in neighbours if len(v) >= 10]]
    bar = np.std(srs) * ((1 - e) * N.inv_cdf(1 - 1 / k) + e * N.inv_cdf(1 - 1 / (k * math.e))) if len(srs) > 1 else 0
    zc = (R - R.mean()) / sd if sd > 0 else R * 0
    den = math.sqrt(max(1 - (zc ** 3).mean() * sr + ((zc ** 4).mean() - 1) / 4 * sr ** 2, 1e-9))
    dsr = N.cdf((sr - bar) * math.sqrt(len(R) - 1) / den)
    g = {
        "1 100+ trades": n >= 100,
        "2 both halves +": R[:mid].sum() > 0 and R[mid:].sum() > 0,
        "3 60% of years +": (by > 0).mean() >= 0.6 if len(by) else False,
        "4 profit at 2x costs": r2.sum() > 0,
        "5 nudged settings 60% +": (np.mean([x > 0 for x in nb]) >= 0.6) if nb else False,
        "7 beats luck bar": dsr >= 0.95,
        "8 holdout (last 25%) +": Hd.sum() > 0,
        "9 Monte Carlo 95%": mc >= 0.95,
    }
    wins, losses = R[R > 0].sum(), -R[R < 0].sum()
    eq = np.cumsum(r)
    return dict(n=n, win=(r > 0).mean(), avg=r.mean() * 100, total=r.sum() * 100, research=R.sum() * 100, holdout=Hd.sum() * 100,
                pf=wins / losses if losses > 0 else float("inf"), dd=float((eq - np.maximum.accumulate(eq)).min() * 100),
                years_up=f"{int((by > 0).sum())}/{len(by)}", nb_pos=f"{sum(x > 0 for x in nb)}/{len(nb)}", mc=mc, dsr=dsr,
                gates=g, verdict="PASS" if all(g.values()) else "FAIL", why=", ".join(k for k, v in g.items() if not v) or "none",
                first=t[0][0], last=t[-1][0], holdout_from=t[cut][0] if cut < n else "-")


# ------------------------------------------------------------------ 2. Turnaround Tuesday
def turnaround(sym, sma=25, cost=0.0002):
    d, a = daily(sym)
    o, c = a[:, 0], a[:, 3]
    wd = [date.fromisoformat(x).weekday() for x in d]
    out = []
    for i in range(sma, len(d) - 1):
        if wd[i] != 0 or wd[i + 1] != 1:
            continue
        m = c[i - sma:i].mean()                     # 25 closes up to Friday
        if c[i - 1] < m:
            out.append((d[i], c[i + 1] / o[i] - 1 - 2 * cost))
    return out


# ------------------------------------------------------------------ 3. Daily long bias
def long_bias(sym, cost=0.0002):
    d, a = daily(sym)
    return [(d[i], a[i, 3] / a[i, 0] - 1 - 2 * cost) for i in range(len(d))]


# ------------------------------------------------------------------ 4. ORB NASDAQ on QQQ 5-min
def qqq5():
    f = DATA / "qqq_5m.json"
    if f.exists():
        return json.loads(f.read_text())
    env = {}
    for line in (ROOT / ".env").read_text(encoding="utf-8").splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            k, v = line.split("=", 1); env[k.strip()] = v.strip().strip('"').strip("'")
    h = {"APCA-API-KEY-ID": env["ALPACA_API_KEY_ID"], "APCA-API-SECRET-KEY": env["ALPACA_API_SECRET_KEY"]}
    rows, tok = [], None
    while True:
        q = dict(timeframe="5Min", start="2016-01-01T00:00:00Z", end="2026-11-11T00:00:00Z", feed="sip", limit=10000, adjustment="split")
        if tok:
            q["page_token"] = tok
        r = http("https://data.alpaca.markets/v2/stocks/QQQ/bars?" + urllib.parse.urlencode(q), h)
        rows += [[b["t"], b["o"], b["h"], b["l"], b["c"]] for b in r.get("bars") or []]
        tok = r.get("next_page_token")
        if not tok:
            break
    DATA.mkdir(parents=True, exist_ok=True)
    f.write_text(json.dumps(rows))
    return rows


def ny_open_utc(d):
    mar = date(d.year, 3, 8); mar += timedelta(days=(6 - mar.weekday()) % 7)
    nov = date(d.year, 11, 1); nov += timedelta(days=(6 - nov.weekday()) % 7)
    return datetime(d.year, d.month, d.day, 13 if mar <= d < nov else 14, 30, tzinfo=timezone.utc)


def orb_days(rows):
    days = {}
    for t, o, h, l, c in rows:
        dt = datetime.fromisoformat(t.replace("Z", "+00:00"))
        op = ny_open_utc(dt.date())
        s = int((dt - op).total_seconds() // 300)
        if 0 <= s < 78:
            days.setdefault(dt.date().isoformat(), {})[s] = (o, h, l, c)
    return {d: v for d, v in days.items() if len(v) >= 70}


def orb(days, rng_bars=3, atr_k=0.25, tgt=2.0, last_slot=65, cost=0.0002):
    out = []
    prev = []                                       # last bars for ATR(14)
    for d in sorted(days):
        b = days[d]
        sl = sorted(b)
        bars = [b[s] for s in sl]
        trs = []
        pc = None
        for (o, h, l, c) in (prev + bars):
            trs.append(max(h - l, abs(h - pc), abs(l - pc)) if pc is not None else h - l)
            pc = c
        RH = max(b[s][1] for s in sl if s < rng_bars)
        RL = min(b[s][2] for s in sl if s < rng_bars)
        height = RH - RL
        trade = None
        for k, s in enumerate(sl):
            if s < rng_bars or s > last_slot:
                continue
            o, h, l, c = b[s]
            if c > RH or c < RL:
                atr = np.mean(trs[len(prev) + k - 13:len(prev) + k + 1])
                side = 1 if c > RH else -1
                entry = c
                stop = RL - atr_k * atr if side > 0 else RH + atr_k * atr
                target = RH + tgt * height if side > 0 else RL - tgt * height
                exit_ = None
                for s2 in sl:
                    if s2 <= s:
                        continue
                    o2, h2, l2, c2 = b[s2]
                    hit_stop = l2 <= stop if side > 0 else h2 >= stop
                    hit_tgt = h2 >= target if side > 0 else l2 <= target
                    if hit_stop:
                        exit_ = min(stop, o2) if side > 0 else max(stop, o2); break
                    if hit_tgt:
                        exit_ = target; break
                    if s2 >= 77:
                        exit_ = c2; break
                if exit_ is None:
                    exit_ = b[sl[-1]][3]
                trade = (d, side * (exit_ / entry - 1) - 2 * cost)
                break
        if trade:
            out.append(trade)
        prev = bars[-14:]
    return out


# ------------------------------------------------------------------ 1. USDJPY morning range
def usdjpy():
    f = DATA / "usdjpy_1h.json"
    if f.exists():
        return json.loads(f.read_text())
    r = http("https://query1.finance.yahoo.com/v8/finance/chart/USDJPY=X?interval=60m&range=730d")["chart"]["result"][0]
    q = r["indicators"]["quote"][0]
    rows = [[t, o, h, l, c] for t, o, h, l, c in zip(r["timestamp"], q["open"], q["high"], q["low"], q["close"]) if None not in (o, h, l, c)]
    DATA.mkdir(parents=True, exist_ok=True)
    f.write_text(json.dumps(rows))
    return rows


def broker_hour(ts):
    dt = datetime.fromtimestamp(ts, timezone.utc)
    # broker time = UTC+2, UTC+3 during US/EU summer time (last Sun Mar - last Sun Oct)
    y = dt.year
    mar = date(y, 3, 31); mar -= timedelta(days=(mar.weekday() + 1) % 7)
    octo = date(y, 10, 31); octo -= timedelta(days=(octo.weekday() + 1) % 7)
    off = 3 if mar <= dt.date() < octo else 2
    b = dt + timedelta(hours=off)
    return b.date().isoformat(), b.hour


def morning_range(rows, start=3, end=6, close=18, spread=0.01, slip=0.003):
    days = {}
    for t, o, h, l, c in rows:
        d, hr = broker_hour(t)
        days.setdefault(d, {})[hr] = (o, h, l, c)
    out = []
    for d in sorted(days):
        b = days[d]
        if not all(hh in b for hh in range(start, end)):
            continue
        RH = max(b[hh][1] for hh in range(start, end)); RL = min(b[hh][2] for hh in range(start, end))
        pos = None
        for hh in range(end, close):
            if hh not in b:
                continue
            o, h, l, c = b[hh]
            if pos is None:
                up, dn = h >= RH, l <= RL
                if up and dn:
                    # both triggered in one hour: assume the worse case (filled, then stopped)
                    out.append((d, -(RH - RL) / RH - (spread + 2 * slip) / RH)); pos = "done"; break
                if up:
                    pos = (1, max(RH, o), RL)
                elif dn:
                    pos = (-1, min(RL, o), RH)
                if pos and pos != "done":
                    side, entry, stop = pos
                    if (side > 0 and l <= stop) or (side < 0 and h >= stop):
                        out.append((d, side * (stop / entry - 1) - (spread + 2 * slip) / entry)); pos = "done"; break
                continue
            side, entry, stop = pos
            if (side > 0 and l <= stop) or (side < 0 and h >= stop):
                ex = min(stop, o) if side > 0 else max(stop, o)
                out.append((d, side * (ex / entry - 1) - (spread + 2 * slip) / entry)); pos = "done"; break
        if pos and pos != "done":
            side, entry, stop = pos
            last = max(hh for hh in b if hh < close)
            out.append((d, side * (b[last][3] / entry - 1) - (spread + 2 * slip) / entry))
    return out


def main():
    R = []
    # 2
    base = turnaround("DIA"); base2 = turnaround("DIA", cost=0.0004)
    nb = [turnaround("DIA", s) for s in (20, 30)] + [turnaround(x) for x in ("SPY", "QQQ", "IWM")]
    R.append(("2. Turnaround Tuesday (DIA, Dow 30)", "Friday close below 25-day SMA: buy Monday open, sell Tuesday close", gates(base, base2, nb)))
    d, a = daily("DIA")
    hold_dia = (a[-1, 3] / a[0, 3]) ** (252 / len(d)) - 1
    # 3
    base = long_bias("QQQ"); base2 = long_bias("QQQ", 0.0004)
    nb = [long_bias(x) for x in ("SPY", "DIA", "IWM")]
    R.append(("3. Daily long bias (QQQ, NASDAQ)", "buy every open, sell the same close", gates(base, base2, nb)))
    d, a = daily("QQQ")
    oc = np.prod(a[:, 3] / a[:, 0]) ** (252 / len(d)) - 1
    on = np.prod(a[1:, 0] / a[:-1, 3]) ** (252 / len(d)) - 1
    # 4
    print("downloading QQQ 5-min 2016-2026 ...", flush=True)
    days = orb_days(qqq5())
    print("QQQ days:", len(days), flush=True)
    base = orb(days); base2 = orb(days, cost=0.0004)
    nb = [orb(days, rng_bars=rb) for rb in (2, 4)] + [orb(days, atr_k=k) for k in (0.2, 0.3)] + [orb(days, tgt=t) for t in (1.6, 2.4)] + [orb(days, last_slot=ls) for ls in (53, 71)]
    R.append(("4. ORB NASDAQ (QQQ 5-min)", "range 9:30-9:45, 5-min close beyond it until 15:00, stop other side + 0.25 ATR, target 2x range, flat 15:55", gates(base, base2, nb)))
    # 1
    rows = usdjpy()
    base = morning_range(rows); base2 = morning_range(rows, spread=0.02, slip=0.006)
    nb = [morning_range(rows, start=s) for s in (2, 4)] + [morning_range(rows, end=e) for e in (5, 7)] + [morning_range(rows, close=c) for c in (16, 20)]
    R.append(("1. Morning range breakout (USDJPY)", "range 03:00-06:00 broker time, stop orders both sides, stop other side, flat 18:00", gates(base, base2, nb)))

    L = ["# The reel's four strategies, tested\n", "Exact rules from the reel, after costs. Holdout = the last 25% of each test's trades. Settings nudged about 20% for the neighbour gate (gate 6, walk-forward, is not used here).\n",
         "| Strategy | Trades | Dates | Win % | Avg per trade | Profit factor | Total | Worst fall | Holdout | Years up | Nudged settings + | Luck score | Monte Carlo | Verdict | Gates failed |",
         "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for name, rule, g in R:
        if g.get("verdict") == "FAIL" and "gates" not in g:
            L.append(f"| {name} | {g['n']} | | | | | | | | | | | | FAIL | {g['why']} |"); continue
        L.append(f"| {name} | {g['n']} | {g['first']} to {g['last']} | {g['win']:.1%} | {g['avg']:+.3f}% | {g['pf']:.2f} | {g['total']:+.1f}% | {g['dd']:.1f}% | {g['holdout']:+.1f}% (from {g['holdout_from']}) | {g['years_up']} | {g['nb_pos']} | {g['dsr']:.2f} | {g['mc']:.0%} | **{g['verdict']}** | {g['why']} |")
    L.append(f"\nFor scale: holding DIA made {hold_dia:+.1%} a year. QQQ open-to-close (rule 3) compounded {oc:+.1%} a year; QQQ close-to-next-open (overnight) {on:+.1%} a year.")
    L.append("Profit factor = money won / money lost. Totals are the sum of per-trade % returns on the position (not compounded, no leverage).")
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "reel_strats_report.md").write_text("\n".join(L) + "\n", encoding="utf-8")
    print("\n".join(L))


if __name__ == "__main__":
    main()
