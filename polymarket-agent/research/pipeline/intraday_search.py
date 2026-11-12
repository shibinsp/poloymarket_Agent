"""Intraday strategy search pipeline (research only, built 11 Nov 2026).

  python -I research/pipeline/intraday_search.py build <raw_1m_dir>   one-time: 1-minute JSON -> 5-minute cache
  python -I research/pipeline/intraday_search.py update               add new days (Alpaca SIP for US, Yahoo for India)
  python -I research/pipeline/intraday_search.py run                  search + gates + report
  python -I research/pipeline/intraday_search.py all                  update, then run

Every rule is applied to the whole universe (91 US stocks / NIFTY 50 stocks) on
5-minute bars, flat before the close. A day's result is the average return of
that day's trades per $1, after costs. Gates, in order (research days only):
  1 at least 100 trades            5 at least 60% of the rule's family profitable
  2 both halves profitable         6 family walk-forward profitable
  3 at least 60% of months up      7 deflated Sharpe >= 0.95 (luck bar)
  4 profitable at double costs     8 holdout (last 40 days) profitable
  9 Monte Carlo: 95% of 5,000 reshuffled histories end in profit
It reads prices only. It never places orders or changes the agent's settings.
Never prints keys."""
import json, math, re, sys, time, urllib.parse, urllib.request, warnings
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from statistics import NormalDist
import numpy as np

warnings.filterwarnings("ignore")
ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "research" / "data"
OUT = ROOT / "reports" / "research"
HOLDOUT_DAYS = 40
MKT = {
    # cost_rt: round trip per $1 traded. US: 5 bp slippage a side. India: MIS
    # brokerage 0.05% a side + STT 0.025% + exchange/GST/stamp + slippage.
    "us": dict(B=78, cost_rt=0.0010, label="US stocks (91)"),
    "in": dict(B=75, cost_rt=0.0018, label="India NIFTY 50 stocks"),
}
N = NormalDist()


def us_open_sec(d: date) -> int:
    """9:30 New York in seconds after 00:00 UTC (DST: 2nd Sun Mar - 1st Sun Nov)."""
    mar = date(d.year, 3, 8); mar += timedelta(days=(6 - mar.weekday()) % 7)
    nov = date(d.year, 11, 1); nov += timedelta(days=(6 - nov.weekday()) % 7)
    return (13 if mar <= d < nov else 14) * 3600 + 1800


def open_sec(mk, d):
    return us_open_sec(d) if mk == "us" else 3 * 3600 + 45 * 60  # 09:15 IST


# ---------------------------------------------------------------- data
def agg(rows, mk):
    """1-minute or 5-minute rows [ts,o,h,l,c,v] -> {daynum: (slots, o,h,l,c,v)} on 5-minute slots."""
    B = MKT[mk]["B"]
    a = np.asarray(rows, float)
    if a.ndim != 2 or len(a) == 0:
        return {}
    a = a[np.argsort(a[:, 0], kind="stable")]
    dn = np.floor(a[:, 0] / 86400).astype(np.int64)
    osec = {int(x): open_sec(mk, date(1970, 1, 1) + timedelta(days=int(x))) for x in np.unique(dn)}
    sec = a[:, 0] - dn * 86400 - np.array([osec[int(x)] for x in dn])
    slot = np.floor(sec / 300).astype(np.int64)
    keep = (slot >= 0) & (slot < B) & (a[:, 4] > 0)
    a, dn, slot = a[keep], dn[keep], slot[keep]
    if len(a) == 0:
        return {}
    key = dn * B + slot
    uk, st = np.unique(key, return_index=True)
    en = np.r_[st[1:], len(a)] - 1
    o, h, l, c, v = a[st, 1], np.maximum.reduceat(a[:, 2], st), np.minimum.reduceat(a[:, 3], st), a[en, 4], np.add.reduceat(a[:, 5], st)
    out = {}
    for d in np.unique(uk // B):
        m = uk // B == d
        out[int(d)] = (uk[m] % B, o[m], h[m], l[m], c[m], v[m])
    return out


def assemble(mk, per_sym):
    """{sym: {daynum: (...)}} -> arrays [S, D, B]"""
    B = MKT[mk]["B"]
    syms = sorted(per_sym)
    cnt = {}
    for s in syms:
        for d, x in per_sym[s].items():
            if x[0][0] == 0:
                cnt[d] = cnt.get(d, 0) + 1
    days = sorted(d for d, n in cnt.items() if n >= max(3, len(syms) // 3))
    di = {d: i for i, d in enumerate(days)}
    arr = np.full((5, len(syms), len(days), B), np.nan, np.float32)
    for si, s in enumerate(syms):
        for d, x in per_sym[s].items():
            if d in di:
                for k in range(5):
                    arr[k, si, di[d], x[0]] = x[k + 1]
    return syms, [str(date(1970, 1, 1) + timedelta(days=d)) for d in days], arr


def save(mk, syms, days, arr):
    DATA.mkdir(parents=True, exist_ok=True)
    np.savez_compressed(DATA / f"bars5_{mk}.npz", syms=np.array(syms), days=np.array(days), arr=arr)


def load(mk):
    z = np.load(DATA / f"bars5_{mk}.npz")
    return [str(x) for x in z["syms"]], [str(x) for x in z["days"]], z["arr"]


def build(raw):
    raw = Path(raw)
    for mk in MKT:
        per = {}
        for f in sorted(raw.glob(f"{mk}_*.json")):
            s = f.stem[len(mk) + 1:]
            if "INDEX" in s:
                continue
            per[s] = agg(json.loads(f.read_text()), mk)
        syms, days, arr = assemble(mk, per)
        save(mk, syms, days, arr)
        print(f"{mk}: {len(syms)} symbols, {len(days)} days {days[0]} .. {days[-1]}", flush=True)


def http(url, headers=None, timeout=60):
    err = None
    for a in range(3):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=headers or {"User-Agent": "Mozilla/5.0"}), timeout=timeout) as r:
                return json.load(r)
        except Exception as e:  # noqa: BLE001
            err = e
            time.sleep(2 * (a + 1))
    raise err


def fetch_us(syms, start):
    env = {}
    for line in (ROOT / ".env").read_text(encoding="utf-8").splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            k, v = line.split("=", 1); env[k.strip()] = v.strip().strip('"').strip("'")
    h = {"APCA-API-KEY-ID": env["ALPACA_API_KEY_ID"], "APCA-API-SECRET-KEY": env["ALPACA_API_SECRET_KEY"]}
    end = (datetime.now(timezone.utc) - timedelta(minutes=30)).strftime("%Y-%m-%dT%H:%M:%SZ")
    rows = {s: [] for s in syms}
    for i in range(0, len(syms), 10):
        tok = None
        while True:
            q = dict(symbols=",".join(syms[i:i + 10]), timeframe="5Min", start=f"{start}T00:00:00Z", end=end, feed="sip", limit=10000, adjustment="raw")
            if tok:
                q["page_token"] = tok
            r = http("https://data.alpaca.markets/v2/stocks/bars?" + urllib.parse.urlencode(q), h)
            for s, bs in (r.get("bars") or {}).items():
                rows[s] += [[datetime.fromisoformat(b["t"].replace("Z", "+00:00")).timestamp(), b["o"], b["h"], b["l"], b["c"], b["v"]] for b in bs]
            tok = r.get("next_page_token")
            if not tok:
                break
    return rows


def fetch_in(syms):
    rows = {}
    for s in syms:
        try:
            r = http(f"https://query1.finance.yahoo.com/v8/finance/chart/{urllib.parse.quote(s)}.NS?interval=5m&range=60d")["chart"]["result"][0]
            q = r["indicators"]["quote"][0]
            rows[s] = [[t, o, h, l, c, v or 0] for t, o, h, l, c, v in zip(r["timestamp"], q["open"], q["high"], q["low"], q["close"], q["volume"]) if None not in (o, h, l, c)]
        except Exception:  # noqa: BLE001
            rows[s] = []
        time.sleep(0.3)
    return rows


def update():
    today = datetime.now(timezone.utc).date()
    for mk in MKT:
        try:
            syms, days, arr = load(mk)
            start = str(date.fromisoformat(days[-1]) + timedelta(days=1))
            if start >= str(today):
                print(f"{mk}: already up to date ({days[-1]})"); continue
            raw = fetch_us(syms, start) if mk == "us" else fetch_in(syms)
            per = {s: agg(r, mk) for s, r in raw.items() if r}
            _, nd, na = assemble(mk, per)
            # only complete sessions strictly after the cache and before today
            keep = [i for i, d in enumerate(nd) if days[-1] < d < str(today)]
            if not keep:
                print(f"{mk}: no new complete days after {days[-1]}"); continue
            full = np.full((5, len(syms), len(keep), arr.shape[3]), np.nan, np.float32)
            order = sorted(per)
            for si, s in enumerate(syms):
                if s in per:
                    full[:, si] = na[:, order.index(s)][:, keep]
            save(mk, syms, days + [nd[i] for i in keep], np.concatenate([arr, full], axis=2))
            print(f"{mk}: added {len(keep)} days, now through {nd[keep[-1]]}", flush=True)
        except Exception as e:  # noqa: BLE001
            print(f"{mk}: update failed ({type(e).__name__}); using the cached data", flush=True)


# ---------------------------------------------------------------- engine
class Market:
    def __init__(self, mk):
        self.mk, self.B, self.cost = mk, MKT[mk]["B"], MKT[mk]["cost_rt"]
        self.syms, self.days, arr = load(mk)
        O, H, L, C, V = (arr[k].astype(np.float64) for k in range(5))
        S, D, B = C.shape
        self.S, self.D = S, D
        # a 5-minute slot with no trade = flat at the last close
        idx = np.where(np.isfinite(C), np.arange(B), 0)
        idx = np.maximum.accumulate(idx, axis=2)
        Cf = np.take_along_axis(C, idx, 2)
        miss = ~np.isfinite(C)
        O, H, L = (np.where(miss, Cf, x) for x in (O, H, L))
        self.O, self.H, self.L, self.C, self.V = O, H, L, Cf, np.nan_to_num(V)
        self.IDX = np.arange(B)[None, None, :]
        self.DI = np.arange(D)[None, :]
        self.Od = O[:, :, 0]
        lastC = Cf[:, :, -1]
        self.PC = np.c_[np.full(S, np.nan), lastC[:, :-1]]
        dH, dL = np.nanmax(H, 2), np.nanmin(L, 2)
        self.PDH, self.PDL = np.c_[np.full(S, np.nan), dH[:, :-1]], np.c_[np.full(S, np.nan), dL[:, :-1]]
        rng = (dH - dL) / self.Od
        v0 = self.V[:, :, 0]
        self.ADR, self.RV1 = np.full((S, D), np.nan), np.full((S, D), np.nan)
        for d in range(5, D):
            a = max(0, d - 14)
            self.ADR[:, d] = np.nanmean(rng[:, a:d], 1)
            m = np.nanmean(np.where(v0[:, a:d] > 0, v0[:, a:d], np.nan), 1)
            self.RV1[:, d] = v0[:, d] / m
        tp = (H + L + Cf) / 3
        cv = np.cumsum(self.V, 2)
        self.VW = np.where(cv > 0, np.cumsum(np.nan_to_num(tp) * self.V, 2) / np.maximum(cv, 1e-9), np.nan)
        self.Rm = np.nanmean(Cf / self.Od[:, :, None] - 1, 0)  # [D, B] equal-weight market since the open
        self.XB = B - 4                                        # bar that closes 15 minutes before the close
        self.names, self.descs, self.groups, self.gross, self.ntr = [], [], [], [], []

    def take(self, A, i):
        return np.take_along_axis(A, np.clip(i, 0, self.B - 1)[..., None], 2)[..., 0]

    def first(self, mask, lo, hi):
        m = mask & (self.IDX >= lo) & (self.IDX < hi)
        i = m.argmax(2)
        i[~m.any(2)] = -1
        return i

    def sim(self, e, side, stop=None, x=None):
        """Enter at the close of bar e, exit at the close of bar x or at the stop."""
        side = np.broadcast_to(np.asarray(side, float), e.shape)
        x = np.broadcast_to(np.asarray(self.XB if x is None else x), e.shape)
        ok = (e >= 0) & (e < x) & (x < self.B)
        ec, xc = np.clip(e, 0, self.B - 1), np.clip(x, 0, self.B - 1)
        P, X = self.take(self.C, ec), self.take(self.C, xc)
        if stop is not None:
            act = (self.IDX > ec[..., None]) & (self.IDX <= xc[..., None])
            hit = act & np.where(side[..., None] > 0, self.L <= stop[..., None], self.H >= stop[..., None])
            fi = hit.argmax(2)
            o = self.take(self.O, fi)
            sp = np.where(side > 0, np.minimum(stop, o), np.maximum(stop, o))
            X = np.where(hit.any(2), sp, X)
        r = side * (X / P - 1)
        r[~ok | ~np.isfinite(r) | (np.abs(r) > 0.3)] = np.nan
        return r, ec, side

    def add(self, fam, key, desc, res):
        r, ec, side = res
        for filt in (0, 1):
            rr = r
            if filt:
                rr = np.where(np.sign(self.Rm[self.DI, ec]) == side, r, np.nan)
            n = np.isfinite(rr).sum(0)
            self.gross.append(np.nansum(rr, 0) / np.maximum(n, 1))
            self.ntr.append(n)
            self.names.append(f"{self.mk} {fam} {key}{' +mkt' if filt else ''}")
            self.descs.append(desc + ("; only when the whole market is moving the same way" if filt else ""))
            # Neighbours = same family and same side/kind, only the numbers differ.
            # (Mixing a long rule with its mirror short makes gate 5 impossible.)
            kind = " ".join(t for t in key.split() if not any(ch.isdigit() for ch in t))
            self.groups.append(f"{self.mk} {fam} {kind}{' +mkt' if filt else ''}".replace("  ", " "))

    # ------------------------------------------------------------ families
    def families(self):
        B, XB, C, H, L = self.B, self.XB, self.C, self.H, self.L
        mins = lambda b: f"{(b + 1) * 5} min after the open"
        xs = {XB: "15 min before the close", 29: "2.5 h after the open"}
        nan = np.nan

        # A. opening-range breakout
        for r in (1, 3, 6, 12):
            RH, RL = np.nanmax(H[:, :, :r], 2), np.nanmin(L[:, :, :r], 2)
            up, dn = C > RH[..., None], C < RL[..., None]
            for last in (6, 12, 24, B):
                if last <= r:
                    continue
                fu, fd = self.first(up, r, last), self.first(dn, r, last)
                both_up = (fu >= 0) & ((fd < 0) | (fu <= fd))
                for sm, e, side in (("long", fu, np.ones_like(RH)), ("short", fd, -np.ones_like(RH)),
                                    ("both", np.where(both_up, fu, fd), np.where(both_up, 1.0, -1.0))):
                    for rv in (0, 1.5, 2.5):
                        e2 = e if rv == 0 else np.where(self.RV1 >= rv, e, -1)
                        for st, stop in (("far", np.where(side > 0, RL, RH)), ("mid", (RH + RL) / 2), ("none", None)):
                            for x in (XB, 29):
                                self.add("orb", f"r{r * 5} {sm} last{last * 5} rv{rv} stop-{st} x{x}",
                                         f"Opening-range breakout: first {r * 5} min is the range; {sm} on a 5-min close beyond it within {last * 5} min"
                                         f"{'' if rv == 0 else f', opening volume at least {rv}x normal'}; stop {st}; exit {xs[x]}",
                                         self.sim(e2, side, stop, x))
        # B. gap at the open
        gap = self.Od / self.PC - 1
        for thr in (0.005, 0.01, 0.02, 0.03):
            for dname, cond, d in (("up", gap >= thr, 1.0), ("down", gap <= -thr, -1.0)):
                for act, a in (("follow", 1.0), ("fade", -1.0)):
                    for eb in (0, 5):
                        for x in (eb + 12, 38, XB):
                            self.add("gap", f"{dname}{thr} {act} e{eb} x{x}",
                                     f"Gap {dname} of {thr:.1%} or more: {act} it, enter {mins(eb)}, exit at bar {x}",
                                     self.sim(np.where(cond, eb, -1), d * a, None, x))
        # C. first-move continuation / fade
        for n in (1, 3, 6):
            mv = C[:, :, n - 1] / self.Od - 1
            for thr in (0.005, 0.01, 0.015, 0.02):
                for dname, cond, d in (("up", mv >= thr, 1.0), ("down", mv <= -thr, -1.0)):
                    for act, a in (("follow", 1.0), ("fade", -1.0)):
                        for rv in (0, 2.5):
                            c2 = cond if rv == 0 else cond & (self.RV1 >= rv)
                            for x in (n - 1 + 12, 38, XB):
                                self.add("firstmove", f"n{n * 5} {dname}{thr} {act} rv{rv} x{x}",
                                         f"First {n * 5} min moves {dname} {thr:.1%} or more{'' if rv == 0 else ' on 2.5x volume'}: {act} it, exit at bar {x}",
                                         self.sim(np.where(c2, n - 1, -1), d * a, None, x))
        # D. distance from VWAP (in units of the stock's usual daily range)
        z = (C - self.VW) / self.VW / self.ADR[..., None]
        for k in (0.25, 0.5, 0.75):
            for dname, m, d in (("above", z > k, 1.0), ("below", z < -k, -1.0)):
                e = self.first(m, 6, B - 16)
                for act, a in (("follow", 1.0), ("fade", -1.0)):
                    for hold in (6, 12, 0):
                        x = XB if hold == 0 else np.minimum(e + hold, XB)
                        self.add("vwap", f"{dname}{k} {act} h{hold}",
                                 f"Price stretches {k} daily-ranges {dname} VWAP: {act} it, hold {'to 15 min before the close' if hold == 0 else f'{hold * 5} min'}",
                                 self.sim(e, d * a, None, x))
        # E. morning direction -> late-day trade
        for w in (6, 12):
            r1 = C[:, :, w - 1] / self.PC - 1
            for thr in (0.0, 0.005, 0.01):
                for act, a in (("follow", 1.0), ("fade", -1.0)):
                    for eb in (XB - 6, 38):
                        self.add("ampm", f"w{w * 5} thr{thr} {act} e{eb}",
                                 f"Move from yesterday's close to {w * 5} min after the open is {thr:.1%} or more: {act} it from bar {eb} to 15 min before the close",
                                 self.sim(np.where(np.abs(r1) >= max(thr, 1e-9), eb, -1), np.sign(r1) * a, None, XB))
        # F. time of day
        pts = list(range(0, XB, 6)) + [XB]
        for i, a_ in enumerate(pts):
            for b_ in pts[i + 1:]:
                for sname, sd in (("long", 1.0), ("short", -1.0)):
                    self.add("timeofday", f"{sname} {a_}-{b_}", f"Every day go {sname} from {mins(a_)} to {mins(b_)}",
                             self.sim(np.full((self.S, self.D), a_), sd, None, b_))
        # G. yesterday's high / low
        for lname, m, d, gate in (("high", C > self.PDH[..., None], 1.0, self.Od < self.PDH), ("low", C < self.PDL[..., None], -1.0, self.Od > self.PDL)):
            for last in (12, 36, B - 10):
                e = np.where(gate, self.first(m, 0, last), -1)
                P = self.take(C, e)
                for act, a in (("follow", 1.0), ("fade", -1.0)):
                    for st in ("none", "half-range"):
                        stop = None if st == "none" else P * (1 - d * a * 0.5 * self.ADR)
                        self.add("prevday", f"{lname} {act} last{last} stop-{st}",
                                 f"Price crosses yesterday's {lname} within {last * 5} min: {act} it, stop {st}, exit 15 min before the close",
                                 self.sim(e, d * a, stop, XB))
        # H. big move by a set time -> rest of day
        for t in (29, B - 25, B - 13):
            mv = C[:, :, t] / self.Od - 1
            for thr in (0.01, 0.02, 0.03):
                for dname, cond, d in (("up", mv >= thr, 1.0), ("down", mv <= -thr, -1.0)):
                    for act, a in (("follow", 1.0), ("fade", -1.0)):
                        self.add("latemove", f"t{t} {dname}{thr} {act}",
                                 f"Stock is {dname} {thr:.0%} or more from the open by {mins(t)}: {act} it until 15 min before the close",
                                 self.sim(np.where(cond, t, -1), d * a, None, XB))
        # I. sharp 30-minute move
        r6 = np.full(C.shape, nan)
        r6[:, :, 6:] = C[:, :, 6:] / C[:, :, :-6] - 1
        zz = r6 / self.ADR[..., None]
        for k in (0.3, 0.5, 0.8):
            for dname, m, d in (("drop", zz < -k, -1.0), ("jump", zz > k, 1.0)):
                e = self.first(m, 6, B - 16)
                for act, a in (("follow", 1.0), ("fade", -1.0)):
                    for hold in (6, 12):
                        self.add("burst30", f"{dname}{k} {act} h{hold}",
                                 f"A 30-min {dname} of {k} daily-ranges: {act} it for {hold * 5} min",
                                 self.sim(e, d * a, None, np.minimum(e + hold, XB)))


def stats(mk):
    t0 = time.time()
    m = Market(mk)
    m.families()
    G = np.array(m.gross); NT = np.array(m.ntr)
    traded = NT > 0
    P1 = np.where(traded, G - m.cost, 0.0)
    P2 = np.where(traded, G - 2 * m.cost, 0.0)
    days = np.array(m.days)
    D = len(days)
    res = np.arange(D) < D - HOLDOUT_DAYS
    res[:5] = False                                   # warm-up for the 14-day averages
    R = P1[:, res]
    rd = days[res]
    mid = R.shape[1] // 2
    months = sorted({d[:7] for d in rd if (np.char.startswith(rd, d[:7])).sum() >= 5})
    MP = np.stack([R[:, np.char.startswith(rd, mo)].sum(1) for mo in months], 1)
    tot, h1, h2 = R.sum(1), R[:, :mid].sum(1), R[:, mid:].sum(1)
    sd = R.std(1)
    sr = np.where(sd > 0, R.mean(1) / np.where(sd > 0, sd, 1), 0)
    zc = (R - R.mean(1, keepdims=True)) / np.where(sd > 0, sd, 1)[:, None]
    skew, kurt = (zc ** 3).mean(1), (zc ** 4).mean(1)
    ntr = NT[:, res].sum(1)
    g = dict(
        g1=ntr >= 100, g2=(h1 > 0) & (h2 > 0), g3=(MP > 0).mean(1) >= 0.6,
        g4=P2[:, res].sum(1) > 0,
    )
    groups = np.array(m.groups)
    g5, g6 = np.zeros(len(tot), bool), np.zeros(len(tot), bool)
    for grp in np.unique(groups):
        ix = np.where(groups == grp)[0]
        g5[ix] = (tot[ix] > 0).mean() >= 0.6
        wf = 0.0
        for k in range(1, len(months)):
            best = ix[MP[ix, :k].sum(1).argmax()]
            wf += MP[best, k]
        g6[ix] = wf > 0
    g.update(g5=g5, g6=g6)
    hold = P1[:, ~res & (np.arange(D) >= 5)].sum(1)
    print(f"{mk}: {len(tot):,} variants, {D} days ({days[0]} .. {days[-1]}), research {res.sum()} / holdout {HOLDOUT_DAYS}, {time.time() - t0:.0f}s", flush=True)
    return dict(mk=mk, names=m.names, descs=m.descs, groups=groups, P1=P1, res=res, days=days, months=months,
                tot=tot, h1=h1, h2=h2, mpos=(MP > 0).mean(1), cost2=P2[:, res].sum(1), sr=sr, skew=skew, kurt=kurt,
                ntr=ntr, hold=hold, g=g, T=int(res.sum()), win=np.array([(np.array(x)[res][np.array(n)[res] > 0] > m.cost).mean() if (np.array(n)[res] > 0).any() else 0 for x, n in zip(m.gross, m.ntr)]))


def luck_bar(sr_all, n):
    v = sr_all.var()
    e = 0.5772156649
    return math.sqrt(v) * ((1 - e) * N.inv_cdf(1 - 1 / n) + e * N.inv_cdf(1 - 1 / (n * math.e)))


def dsr(sr, sr0, T, skew, kurt):
    den = np.sqrt(np.maximum(1 - skew * sr + (kurt - 1) / 4 * sr ** 2, 1e-9))
    return np.array([N.cdf(x) for x in (sr - sr0) * math.sqrt(T - 1) / den])


def run():
    OUT.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(7)
    lines, summary, all_pass = [], {}, []
    now = datetime.now(timezone(timedelta(hours=5, minutes=30)))
    for mk in MKT:
        s = stats(mk)
        g = s["g"]
        ok1 = g["g1"]
        Rr = s["P1"][:, s["res"]]
        # effective number of independent trials (participation ratio of the correlation spectrum)
        ix = np.where(ok1 & (Rr.std(1) > 0))[0]
        samp = ix if len(ix) <= 2500 else rng.choice(ix, 2500, replace=False)
        Z = Rr[samp]; Z = (Z - Z.mean(1, keepdims=True)) / Z.std(1, keepdims=True)
        ev = np.linalg.svd(Z, compute_uv=False) ** 2
        neff = max(2, int(round(ev.sum() ** 2 / (ev ** 2).sum())))
        n_all = int(ok1.sum())
        bars = {"all": luck_bar(s["sr"][ok1], n_all), "eff": luck_bar(s["sr"][ok1], neff)}
        d_all = dsr(s["sr"], bars["all"], s["T"], s["skew"], s["kurt"])
        d_eff = dsr(s["sr"], bars["eff"], s["T"], s["skew"], s["kurt"])
        g["g7"] = d_all >= 0.95
        g["g8"] = s["hold"] > 0
        order = ["g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8"]
        alive = np.ones(len(s["tot"]), bool)
        funnel = []
        for k in order:
            alive = alive & g[k]
            funnel.append((k, int(g[k].sum()), int(alive.sum())))
        base = g["g1"] & g["g2"] & g["g3"] & g["g4"] & g["g5"] & g["g6"]
        # Monte Carlo on everything that clears gates 1-6 and the holdout
        cand = np.where(base & g["g8"])[0]
        mc = np.zeros(len(s["tot"]))
        use = s["P1"][:, 5:]
        for i in cand:
            mc[i] = (rng.choice(use[i], (5000, use.shape[1])).sum(1) > 0).mean()
        g["g9"] = mc >= 0.95
        final = np.where(base & g["g7"] & g["g8"] & g["g9"])[0]
        final_eff = np.where(base & (d_eff >= 0.95) & g["g8"] & g["g9"])[0]
        near = sorted(cand, key=lambda i: -s["sr"][i])[:12]
        ann = math.sqrt(252)
        def row(i):
            return dict(name=s["names"][i], desc=s["descs"][i], trades=int(s["ntr"][i]), per_day_pct=float(s["tot"][i] / s["T"] * 100),
                        total_pct=float(s["tot"][i] * 100), halves=[float(s["h1"][i] * 100), float(s["h2"][i] * 100)], months_up=float(s["mpos"][i]),
                        cost2_pct=float(s["cost2"][i] * 100), sharpe=float(s["sr"][i] * ann), dsr_all=float(d_all[i]), dsr_eff=float(d_eff[i]),
                        holdout_pct=float(s["hold"][i] * 100), mc=float(mc[i]), win=float(s["win"][i]))
        fams = {}
        for grp in np.unique(s["groups"]):
            ixg = s["groups"] == grp
            fams[grp] = (int(ixg.sum()), int((ixg & base).sum()), int((ixg & base & g["g8"]).sum()), int(np.isin(np.where(ixg)[0], final).sum()))
        # the US book's live rule, for the gate table
        live = None
        if mk == "us":
            nm = f"us orb r5 both last30 rv2.5 stop-far x{MKT['us']['B'] - 4}"
            if nm in s["names"]:
                i = s["names"].index(nm)
                live = dict(row(i), gates={k: bool(g[k][i]) for k in order + ["g9"]})
        summary[mk] = dict(variants=len(s["tot"]), days=[str(s["days"][0]), str(s["days"][-1])], research_days=s["T"], months=s["months"],
                           funnel=funnel, neff=neff, luck_bar_all=float(bars["all"] * ann), luck_bar_eff=float(bars["eff"] * ann),
                           passed=[row(i) for i in final], passed_eff=[row(i) for i in final_eff], near=[row(i) for i in near],
                           families=fams, live=live, best_sharpe=float(s["sr"][ok1].max() * ann) if n_all else 0)
        all_pass += [s["names"][i] for i in final]
    hist_f = OUT / "history.json"
    hist = json.loads(hist_f.read_text()) if hist_f.exists() else []
    hist.append(dict(at=now.strftime("%Y-%m-%d %H:%M IST"), passed=all_pass,
                     passed_eff=[r["name"] for mk in MKT for r in summary[mk]["passed_eff"]]))
    hist_f.write_text(json.dumps(hist[-60:], indent=1))
    (OUT / "intraday_latest.json").write_text(json.dumps(dict(at=hist[-1]["at"], markets=summary), indent=1))
    report(summary, hist)


GATE = {"g1": "1. At least 100 trades", "g2": "2. Both halves profitable", "g3": "3. 60% of months profitable", "g4": "4. Profitable at double costs",
        "g5": "5. 60% of its family profitable", "g6": "6. Family walk-forward profitable", "g7": "7. Beats the luck bar (deflated Sharpe)",
        "g8": "8. Holdout (last 40 days) profitable", "g9": "9. Monte Carlo 95%"}


def report(summary, hist):
    L = []
    A = L.append
    A(f"# Intraday strategy search\n\nRun: {hist[-1]['at']}. Research only: nothing here trades until you say so.\n")
    tot_v = sum(summary[mk]["variants"] for mk in MKT)
    tot_p = sum(len(summary[mk]["passed"]) for mk in MKT)
    A("## Result\n")
    A(f"- **{tot_v:,} rule variants tested** on 5-minute bars, after costs.")
    A(f"- **{tot_p} passed every gate.**" + (" None is safe to trade yet." if tot_p == 0 else " These go to paper trading first, never straight to real money."))
    for mk in MKT:
        s = summary[mk]
        A(f"- {MKT[mk]['label']}: {s['variants']:,} variants, data {s['days'][0]} to {s['days'][1]} ({s['research_days']} research days + {HOLDOUT_DAYS} holdout days).")
    for mk in MKT:
        s = summary[mk]
        A(f"\n## {MKT[mk]['label']}\n")
        A("| Gate (each applied on top of the ones before) | Pass this gate alone | Still standing |\n|---|---|---|")
        for k, alone, alive in s["funnel"]:
            A(f"| {GATE[k]} | {alone:,} | {alive:,} |")
        A(f"\nLuck bar: with {s['funnel'][0][1]:,} rules tried, the best one would show a yearly Sharpe of about **{s['luck_bar_all']:.1f}** by luck alone. "
          f"The rules are near-copies of each other (about {s['neff']} truly different ideas), which gives a softer bar of {s['luck_bar_eff']:.1f}. "
          f"The best rule found scored {s['best_sharpe']:.1f}.")
        A(f"\nPassing every gate with the strict bar: **{len(s['passed'])}**. With the softer bar: **{len(s['passed_eff'])}**.\n")
        rows = s["passed"] or s["passed_eff"]
        if rows:
            A("### Candidates\n\n| Rule | Trades | Per day | Halves | Holdout | Win days | Monte Carlo |\n|---|---|---|---|---|---|---|")
            for r in rows[:15]:
                A(f"| {r['desc']} | {r['trades']} | {r['per_day_pct']:+.3f}% | {r['halves'][0]:+.1f}% / {r['halves'][1]:+.1f}% | {r['holdout_pct']:+.1f}% | {r['win']:.0%} | {r['mc']:.0%} |")
        A("\n### Closest rules (pass gates 1-6 and the holdout, ranked by Sharpe)\n\n| Rule | Trades | Total | Holdout | Sharpe / yr | Luck score | Monte Carlo |\n|---|---|---|---|---|---|---|")
        for r in s["near"]:
            A(f"| {r['desc']} | {r['trades']} | {r['total_pct']:+.1f}% | {r['holdout_pct']:+.1f}% | {r['sharpe']:.1f} | {r['dsr_all']:.2f} | {r['mc']:.0%} |")
        if not s["near"]:
            A("| none | | | | | | |")
        A("\n### By family\n\n| Family | Tried | Pass gates 1-6 | Also holdout | Pass all |\n|---|---|---|---|---|")
        for f, (n, a, b, c) in sorted(s["families"].items()):
            A(f"| {f} | {n} | {a} | {b} | {c} |")
        if s.get("live"):
            lv = s["live"]
            A(f"\n### Your live US ORB rule in this test\n\n{lv['desc']}\n\n- Trades {lv['trades']}, total {lv['total_pct']:+.1f}%, holdout {lv['holdout_pct']:+.1f}%, Sharpe {lv['sharpe']:.1f} a year.\n- Gates: "
              + ", ".join(f"{k[1:]} {'pass' if v else 'FAIL'}" for k, v in lv["gates"].items()) + ".")
    A("\n## How to read this\n")
    A("- **Per day / Total**: average return of the day's trades per rupee or dollar put in, after costs. It is not the account's return.")
    A("- **Luck score**: chance the rule is better than the best of that many random rules. It needs 0.95.")
    A("- **Holdout**: the last 40 days, which were not used to pick the rule.")
    A("- A rule that passes here still has to pass on paper before any money goes on it.")
    seen = {}
    for h in hist:
        for n in h.get("passed_eff", []):
            seen[n] = seen.get(n, 0) + 1
    A(f"\n## History\n\n{len(hist)} run(s) so far." + ("".join(f"\n- `{n}` passed (softer bar) in {c} of {len(hist)} runs" for n, c in sorted(seen.items(), key=lambda x: -x[1])[:10]) if seen else " No rule has passed in any run."))
    (OUT / "intraday_report.md").write_text("\n".join(L) + "\n", encoding="utf-8")
    print("report:", OUT / "intraday_report.md")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "all"
    if cmd == "build":
        build(sys.argv[2])
    elif cmd == "update":
        update()
    elif cmd == "run":
        run()
    else:
        update(); run()
