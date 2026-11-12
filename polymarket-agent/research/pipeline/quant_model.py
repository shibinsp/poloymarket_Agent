"""QuantRank: our own daily stock-ranking model, plus Kalman pairs trading and
the plain known strategies as baselines (research only, built 11 Nov 2026).

  python -I research/pipeline/quant_model.py run [us|in|both]

What it builds, for each market (91 US stocks; NIFTY 50 stocks):

1. Baselines (known strategies, the bar to beat)
   - cross-sectional reversal: buy the worst L-day losers, sell the best
   - cross-sectional momentum: buy the 12-month winners skipping the last month
2. QuantRank, our own model. Each day, for every stock, 25 measures built
   only from data known at that close (see FEATURES). They are turned into
   cross-sectional ranks, and a ridge regression learns how to weigh them to
   predict the next H days' relative return. Walk-forward: the weights used
   in year Y are learned only from years before Y (with an H-day gap), so
   every traded year is out of sample.
3. Kalman pairs: every quarter, pick the K most correlated pairs over the past
   year; a Kalman filter tracks each pair's hedge ratio day by day; trade the
   z-score of the spread back toward zero.

Portfolios are dollar-neutral (equal $ long and short) unless marked "long":
long top q, short bottom q. Costs: US 5 bp a side, India 12 bp a side
(delivery STT etc.), charged on turnover. Return figures are per $1 on each
side.

Gates (research years = until 9 Nov 2024; holdout = 10 Nov 2024 onward):
  1 at least 100 trades      4 profitable at double costs   7 deflated Sharpe >= 0.95
  2 both halves profitable   5 60% of neighbours profitable 8 holdout profitable
  3 60% of years profitable  6 family walk-forward positive 9 Monte Carlo 95%
Plus: QuantRank must beat the best baseline in the holdout to count as "ours
adds something".

Caveat printed in the report: the stock lists are today's names, so stocks
that were dropped or went bust over 25 years are missing (survivorship bias).
That flatters long-only results most; dollar-neutral results less.
Reads prices only. Never places orders or changes the agent."""
import json, math, sys, time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from statistics import NormalDist
import numpy as np

np.seterr(all="ignore")
ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "research" / "data"
OUT = ROOT / "reports" / "research"
HOLD_FROM = "2024-11-10"
CFG = {
    "us": dict(dir="daily_us", bench="SPY", cost=0.0005, label="US stocks (91)", skip=set()),
    "in": dict(dir="daily_in", bench="NSEI", cost=0.0012, label="India NIFTY 50 stocks",
               skip={"NSEI", "NSEBANK", "NIFTYBEES", "BANKBEES", "INDIAVIX"}),
}
N = NormalDist()


# ------------------------------------------------------------------ data
def load(mk):
    c = CFG[mk]
    files = sorted((DATA / c["dir"]).glob("*.json"))
    raw = {f.stem: json.loads(f.read_text()) for f in files}
    bench = raw.get(c["bench"])
    days = sorted({r[0] for r in bench}) if bench else sorted({r[0] for v in raw.values() for r in v})
    di = {d: i for i, d in enumerate(days)}
    syms = [s for s in raw if s not in c["skip"] and s != c["bench"] and len(raw[s]) > 300]
    S, D = len(syms), len(days)
    O, H, L, C, V = (np.full((D, S), np.nan) for _ in range(5))
    for j, s in enumerate(syms):
        for r in raw[s]:
            i = di.get(r[0])
            if i is not None:
                O[i, j], H[i, j], L[i, j], C[i, j], V[i, j] = r[1], r[2], r[3], r[4], r[5]
    B = np.full(D, np.nan)
    if bench:
        for r in bench:
            B[di[r[0]]] = r[4]
    B = ffill(B[:, None])[:, 0]
    return syms, np.array(days), O, H, L, C, V, B


def ffill(a):
    a = a.copy()
    for i in range(1, len(a)):
        m = np.isnan(a[i])
        a[i, m] = a[i - 1, m]
    return a


# ------------------------------------------------------------------ math helpers (rolling, along axis 0 = time)
def roll_sum(x, n):
    x0 = np.nan_to_num(x)
    cs = np.cumsum(x0, 0)
    out = np.full_like(x, np.nan)
    out[n - 1:] = cs[n - 1:] - np.r_[np.zeros((1,) + x.shape[1:]), cs[:-n]]
    cnt = np.cumsum(np.isfinite(x), 0)
    c = np.full(x.shape, 0.0); c[n - 1:] = cnt[n - 1:] - np.r_[np.zeros((1,) + x.shape[1:]), cnt[:-n]]
    out[c < n * 0.8] = np.nan
    return out


def roll_mean(x, n):
    return roll_sum(x, n) / n


def roll_std(x, n):
    m = roll_mean(x, n)
    return np.sqrt(np.maximum(roll_mean(x * x, n) - m * m, 0))


def lag(x, k):
    out = np.full_like(x, np.nan)
    if k > 0:
        out[k:] = x[:-k]
    else:
        out[:] = x
    return out


def xrank(x):
    """cross-sectional rank to [-0.5, 0.5] per day, NaN kept"""
    out = np.full_like(x, np.nan)
    for i in range(len(x)):
        row = x[i]; m = np.isfinite(row)
        n = m.sum()
        if n >= 5:
            r = np.empty(n); r[np.argsort(row[m], kind="stable")] = np.arange(n)
            out[i, m] = r / (n - 1) - 0.5
    return out


def hurst(r, n=100):
    """rolling Hurst exponent from the variance-ratio of 1-day and 10-day sums"""
    v1 = roll_std(r, n) ** 2
    r10 = roll_sum(r, 10)
    v10 = roll_std(r10, n) ** 2
    return 0.5 * np.log(v10 / np.maximum(v1, 1e-12) ) / np.log(10)


def ou_halflife(logp, n=63):
    """rolling Ornstein-Uhlenbeck half-life of the price around its own mean:
    dX = a + b X ; half-life = -ln2 / ln(1 + b)"""
    x = lag(logp, 1); dx = logp - x
    mx, mdx = roll_mean(x, n), roll_mean(dx, n)
    cov = roll_mean(x * dx, n) - mx * mdx
    var = roll_mean(x * x, n) - mx * mx
    b = cov / np.where(var > 0, var, np.nan)
    hl = -np.log(2) / np.log(np.clip(1 + b, 1e-6, 0.999999))
    return np.clip(hl, 0, 252)


def kalman_trend(logp, q=1e-5, r=1e-3):
    """local linear trend Kalman filter on log price; returns the filtered slope (per day)"""
    D, S = logp.shape
    slope = np.full((D, S), np.nan)
    lvl = np.full(S, np.nan); sl = np.zeros(S)
    P = np.tile(np.eye(2), (S, 1, 1)) * 1.0
    Q = np.diag([q, q / 10])
    for t in range(D):
        y = logp[t]
        ok = np.isfinite(y)
        new = ok & ~np.isfinite(lvl)
        lvl[new] = y[new]; sl[new] = 0; P[new] = np.eye(2)
        act = ok & ~new
        if act.any():
            # predict
            lv, s_, Pa = lvl[act] + sl[act], sl[act], P[act]
            F = np.array([[1.0, 1.0], [0.0, 1.0]])
            Pp = F @ Pa @ F.T + Q
            # update with observation y = level
            Sv = Pp[:, 0, 0] + r
            K0, K1 = Pp[:, 0, 0] / Sv, Pp[:, 1, 0] / Sv
            e = y[act] - lv
            lvl[act] = lv + K0 * e; sl[act] = s_ + K1 * e
            Pn = Pp.copy()
            Pn[:, 0, 0] = (1 - K0) * Pp[:, 0, 0]; Pn[:, 0, 1] = (1 - K0) * Pp[:, 0, 1]
            Pn[:, 1, 0] = Pp[:, 1, 0] - K1 * Pp[:, 0, 0]; Pn[:, 1, 1] = Pp[:, 1, 1] - K1 * Pp[:, 0, 1]
            P[act] = Pn
        slope[t, act | new] = sl[act | new]
    return slope


# ------------------------------------------------------------------ features (all known at the close of day t)
def features(O, H, L, C, V, B):
    lp = np.log(C)
    r = np.vstack([np.full((1, C.shape[1]), np.nan), np.diff(lp, axis=0)])
    rb = np.r_[np.nan, np.diff(np.log(B))][:, None]
    F = {}
    for k in (1, 2, 5, 10, 21):
        F[f"ret{k}"] = lp - lag(lp, k)
    F["mom12_1"] = lag(lp, 21) - lag(lp, 252)
    F["mom6_1"] = lag(lp, 21) - lag(lp, 126)
    F["vol21"] = roll_std(r, 21)
    F["vol63"] = roll_std(r, 63)
    F["volratio"] = F["vol21"] / F["vol63"]
    for n in (20, 50):
        m, s = roll_mean(C, n), roll_std(C, n)
        F[f"z{n}"] = (C - m) / s
    F["ibs"] = (C - L) / np.where(H > L, H - L, np.nan)
    up, dn = np.maximum(r, 0), np.maximum(-r, 0)
    for n in (2, 14):
        F[f"rsi{n}"] = roll_mean(up, n) / (roll_mean(up, n) + roll_mean(dn, n))
    hi252 = np.full_like(C, np.nan)
    for t in range(251, len(C)):
        hi252[t] = np.nanmax(C[t - 251:t + 1], 0)
    F["dist_high"] = C / hi252 - 1
    # beta to the market and beta-adjusted (residual) returns
    cov = roll_mean(r * rb, 126) - roll_mean(r, 126) * roll_mean(np.broadcast_to(rb, r.shape), 126)
    varb = roll_std(np.broadcast_to(rb, r.shape).copy(), 126) ** 2
    beta = cov / varb
    F["beta"] = beta
    res = r - lag(beta, 1) * rb
    F["resid5"] = roll_sum(res, 5)
    F["resid21"] = roll_sum(res, 21)
    F["resid_z"] = F["resid5"] / (roll_std(res, 63) * math.sqrt(5))
    F["hurst"] = hurst(r)
    F["ou_hl"] = ou_halflife(lp)
    F["kalman_slope"] = kalman_trend(lp) / F["vol63"]
    F["vol_shock"] = np.log(V / roll_mean(V, 21))
    F["gap"] = np.log(O / lag(C, 1))
    F["intraday"] = np.log(C / O)
    return F, r


# ------------------------------------------------------------------ portfolios
def to_weights(score, q, H, long_only=False):
    """score [D,S] -> weights held from close t, rebalanced every H days.
    dollar-neutral: +1/n_long top q, -1/n_short bottom q; long: top q minus equal weight."""
    D, S = score.shape
    W = np.zeros((D, S))
    cur = np.zeros(S)
    for t in range(D):
        if t % H == 0:
            row = score[t]; m = np.isfinite(row)
            n = m.sum()
            cur = np.zeros(S)
            if n >= 10:
                k = max(1, int(round(n * q)))
                o = np.argsort(row[m])
                idx = np.where(m)[0]
                top, bot = idx[o[-k:]], idx[o[:k]]
                cur[top] = 1.0 / k
                if long_only:
                    # long top q, short an equal-weight basket of all stocks: the
                    # result is the extra over simply holding every stock
                    cur[idx] -= 1.0 / n
                else:
                    cur[bot] = -1.0 / k
        W[t] = cur
    return W


def pnl(W, r, cost):
    """W held from close t earns r[t+1]; cost per $ traded, each side"""
    rr = np.nan_to_num(np.r_[r[1:], np.zeros((1, r.shape[1]))])
    gross = (W * rr).sum(1)
    turn = np.abs(np.diff(np.vstack([np.zeros((1, W.shape[1])), W]), axis=0)).sum(1)
    trades = (np.abs(np.diff(np.vstack([np.zeros((1, W.shape[1])), W]), axis=0)) > 1e-12).sum(1)
    # shift so the result is dated on the day it is earned
    g = np.r_[0.0, gross[:-1]]; tu = np.r_[0.0, turn[:-1]]; tr = np.r_[0, trades[:-1]]
    return g, tu, tr


# ------------------------------------------------------------------ QuantRank (walk-forward ridge on ranks)
def quantrank(Fr, r, days, H, alpha):
    """Fr: dict of ranked features [D,S]. Target: next-H-day cross-sectional rank.
    Year Y is predicted with weights learned on years < Y, with an H-day gap."""
    names = sorted(Fr)
    X = np.stack([Fr[k] for k in names], 2)                 # [D,S,K]
    lp = np.nancumsum(np.nan_to_num(r), 0)
    fwd = np.full_like(lp, np.nan)
    fwd[:-H] = lp[H:] - lp[:-H]
    fwd[~np.isfinite(r)] = np.nan
    Y = xrank(fwd)
    years = np.array([int(d[:4]) for d in days])
    score = np.full(r.shape, np.nan)
    coefs = {}
    for yr in range(years.min() + 4, years.max() + 1):
        tr_end = np.where(years < yr)[0]
        if len(tr_end) == 0:
            continue
        last = tr_end[-1] - H                                 # gap: targets must be known before year starts
        tr = np.arange(260, max(260, last))
        if len(tr) < 500:
            continue
        Xt = X[tr].reshape(-1, X.shape[2]); yt = Y[tr].reshape(-1)
        m = np.isfinite(yt) & np.isfinite(Xt).all(1)
        if m.sum() < 5000:
            continue
        Xt, yt = Xt[m], yt[m]
        A = Xt.T @ Xt + alpha * len(yt) * np.eye(Xt.shape[1])
        w = np.linalg.solve(A, Xt.T @ yt)
        coefs[yr] = w
        te = np.where(years == yr)[0]
        Xe = X[te]
        s = np.einsum("dsk,k->ds", np.nan_to_num(Xe), w)
        s[~np.isfinite(Xe).all(2)] = np.nan
        score[te] = s
    return score, names, coefs


# ------------------------------------------------------------------ Kalman pairs
def kalman_pairs(C, r, days, K, ent, ext, lookback=252, reform=63, max_hold=20, delta=1e-4):
    D, S = C.shape
    lp = np.log(C)
    W = np.zeros((D, S))
    pairs = []
    state = {}
    for t in range(lookback, D):
        if (t - lookback) % reform == 0:
            win = r[t - lookback + 1:t + 1]
            ok = np.isfinite(win).sum(0) >= lookback * 0.95
            idx = np.where(ok)[0]
            if len(idx) < 4:
                pairs = []
            else:
                cc = np.corrcoef(np.nan_to_num(win[:, idx]).T)
                np.fill_diagonal(cc, -1)
                iu = np.triu_indices(len(idx), 1)
                order = np.argsort(-cc[iu])
                used, pairs = set(), []
                for o in order:
                    a, b = idx[iu[0][o]], idx[iu[1][o]]
                    if a in used or b in used:
                        continue
                    pairs.append((a, b)); used |= {a, b}
                    if len(pairs) == K:
                        break
            new = {}
            for p in pairs:
                a, b = p
                ya, yb = lp[t - lookback + 1:t + 1, a], lp[t - lookback + 1:t + 1, b]
                beta = np.polyfit(yb, ya, 1)
                new[p] = dict(th=np.array([beta[1], beta[0]]), P=np.eye(2), e=[], pos=0, age=0)
                # warm the filter's spread history on the formation window
                for i in range(lookback):
                    x = np.array([1.0, yb[i]]); e = ya[i] - x @ new[p]["th"]
                    new[p]["e"].append(e)
                new[p]["e"] = new[p]["e"][-60:]
            state = new
        w = np.zeros(S)
        for (a, b), st in state.items():
            ya, yb = lp[t, a], lp[t, b]
            if not (np.isfinite(ya) and np.isfinite(yb)):
                st["pos"] = 0
                continue
            # Kalman update of [intercept, hedge ratio]
            x = np.array([1.0, yb])
            Pp = st["P"] + delta / (1 - delta) * np.eye(2)
            e = ya - x @ st["th"]
            Sv = x @ Pp @ x + 1e-4
            Kg = Pp @ x / Sv
            st["th"] = st["th"] + Kg * e
            st["P"] = Pp - np.outer(Kg, x) @ Pp
            st["e"].append(e); st["e"] = st["e"][-60:]
            sd = np.std(st["e"][-60:])
            z = e / sd if sd > 0 else 0.0
            if st["pos"] == 0:
                if z > ent:
                    st["pos"], st["age"] = -1, 0
                elif z < -ent:
                    st["pos"], st["age"] = 1, 0
            else:
                st["age"] += 1
                if (st["pos"] == 1 and z > -ext) or (st["pos"] == -1 and z < ext) or st["age"] >= max_hold:
                    st["pos"] = 0
            if st["pos"]:
                hb = float(np.clip(st["th"][1], 0.2, 5))
                g = 1.0 / (1 + hb)
                w[a] += st["pos"] * g / max(1, K)
                w[b] -= st["pos"] * g * hb / max(1, K)
        W[t] = w
    return W


# ------------------------------------------------------------------ gates
def dsr(sr, sr0, T, sk, ku):
    den = np.sqrt(np.maximum(1 - sk * sr + (ku - 1) / 4 * sr ** 2, 1e-9))
    return np.array([N.cdf(x) for x in (sr - sr0) * math.sqrt(max(T - 1, 1)) / den])


def evaluate(mk, V, days, cost):
    """V: list of dict(name, desc, group, kind, g, tu, tr). Returns rows + funnel."""
    rng = np.random.default_rng(11)
    res = days < HOLD_FROM
    first = 260
    res[:first] = False
    hold = days >= HOLD_FROM
    years = np.array([d[:4] for d in days])
    ry = sorted(set(years[res]))
    out = []
    for v in V:
        p1 = v["g"] - cost * v["tu"]
        p2 = v["g"] - 2 * cost * v["tu"]
        R = p1[res]
        mid = len(R) // 2
        by = np.array([p1[res & (years == y)].sum() for y in ry])
        sd = R.std()
        sr = R.mean() / sd if sd > 0 else 0
        zc = (R - R.mean()) / sd if sd > 0 else R * 0
        out.append(dict(v, p1=p1, tot=R.sum(), h1=R[:mid].sum(), h2=R[mid:].sum(), ypos=(by > 0).mean(), by=by,
                        c2=p2[res].sum(), sr=sr, sk=(zc ** 3).mean(), ku=(zc ** 4).mean(), ntr=int(v["tr"][res].sum()),
                        hold=p1[hold].sum(), hold_sr=(p1[hold].mean() / p1[hold].std() * math.sqrt(252)) if p1[hold].std() > 0 else 0,
                        ann=R.mean() * 252, dd=float((np.cumsum(R) - np.maximum.accumulate(np.cumsum(R))).min())))
    srs = np.array([o["sr"] for o in out])
    T = int(res.sum())
    # luck bar: expected best Sharpe of len(out) unrelated rules with this spread of Sharpe
    e = 0.5772156649
    n = max(2, len(out))
    v_sr = srs.var() if len(srs) > 1 else 0
    # effective trials from the correlation of the research returns
    M = np.array([o["p1"][res] for o in out])
    M = M[M.std(1) > 0]
    Z = (M - M.mean(1, keepdims=True)) / M.std(1, keepdims=True)
    ev = np.linalg.svd(Z, compute_uv=False) ** 2 if len(Z) > 1 else np.array([1.0])
    neff = max(2, int(round(ev.sum() ** 2 / (ev ** 2).sum())))
    bar = lambda k: math.sqrt(v_sr) * ((1 - e) * N.inv_cdf(1 - 1 / k) + e * N.inv_cdf(1 - 1 / (k * math.e)))
    b_all, b_eff = bar(n), bar(neff)
    d_all = dsr(srs, b_all, T, np.array([o["sk"] for o in out]), np.array([o["ku"] for o in out]))
    d_eff = dsr(srs, b_eff, T, np.array([o["sk"] for o in out]), np.array([o["ku"] for o in out]))
    groups = {}
    for i, o in enumerate(out):
        groups.setdefault(o["group"], []).append(i)
    for gname, ix in groups.items():
        g5 = np.mean([out[i]["tot"] > 0 for i in ix]) >= 0.6
        wf = 0.0
        for k in range(1, len(ry)):
            best = max(ix, key=lambda i: out[i]["by"][:k].sum())
            wf += out[best]["by"][k]
        for i in ix:
            out[i]["g5"], out[i]["g6"] = bool(g5), wf > 0
    for i, o in enumerate(out):
        o["g1"] = o["ntr"] >= 100
        o["g2"] = o["h1"] > 0 and o["h2"] > 0
        o["g3"] = o["ypos"] >= 0.6
        o["g4"] = o["c2"] > 0
        o["g7"] = bool(d_all[i] >= 0.95)
        o["g7soft"] = bool(d_eff[i] >= 0.95)
        o["g8"] = o["hold"] > 0
        o["dsr"], o["dsr_eff"] = float(d_all[i]), float(d_eff[i])
        base = o["g1"] and o["g2"] and o["g3"] and o["g4"] and o["g5"] and o["g6"] and o["g8"]
        if base:
            x = o["p1"][first:]
            o["mc"] = float((rng.choice(x, (5000, len(x))).sum(1) > 0).mean())
        else:
            o["mc"] = 0.0
        o["g9"] = o["mc"] >= 0.95
        o["pass"] = base and o["g7"] and o["g9"]
        o["pass_soft"] = base and o["g7soft"] and o["g9"]
    funnel, alive = [], np.ones(len(out), bool)
    for k in ("g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8", "g9"):
        a = np.array([o[k] for o in out])
        alive &= a
        funnel.append((k, int(a.sum()), int(alive.sum())))
    return out, funnel, dict(n=n, neff=neff, bar=b_all * math.sqrt(252), bar_eff=b_eff * math.sqrt(252), T=T)


# ------------------------------------------------------------------ main
def run_market(mk):
    t0 = time.time()
    c = CFG[mk]
    syms, days, O, H, L, C, Vv, B = load(mk)
    F, r = features(O, H, L, C, Vv, B)
    print(f"{mk}: {len(syms)} stocks, {len(days)} days {days[0]}..{days[-1]}, features {len(F)} ({time.time() - t0:.0f}s)", flush=True)
    Fr = {k: xrank(v) for k, v in F.items()}
    V = []

    def add(name, desc, group, kind, W):
        g, tu, tr = pnl(W, r, c["cost"])
        V.append(dict(name=f"{mk} {name}", desc=desc, group=f"{mk} {group}", kind=kind, g=g, tu=tu, tr=tr))

    # 1. baselines
    for L_ in (1, 2, 3, 5, 10, 21):
        s = -(np.log(C) - lag(np.log(C), L_))
        for q in (0.1, 0.2):
            for Hh in (1, 5, 21):
                if Hh < L_ / 2:
                    continue
                add(f"reversal L{L_} q{q} H{Hh}", f"Known: buy the {int(q*100)}% worst {L_}-day losers, sell the {int(q*100)}% best, rebalance every {Hh} day(s)",
                    "baseline reversal", "baseline", to_weights(s, q, Hh))
    for lb, sk in ((252, 21), (126, 21), (63, 5)):
        s = lag(np.log(C), sk) - lag(np.log(C), lb)
        for q in (0.1, 0.2):
            for Hh in (5, 21):
                add(f"momentum {lb}-{sk} q{q} H{Hh}", f"Known: buy the {int(q*100)}% best {lb}-day performers (skipping the last {sk} days), sell the worst, rebalance every {Hh} days",
                    "baseline momentum", "baseline", to_weights(s, q, Hh))
    # 2. QuantRank
    qr_scores = {}
    for Hh in (1, 5, 21):
        for alpha in (0.01, 1.0):
            score, names, coefs = quantrank(Fr, r, days, Hh, alpha)
            qr_scores[(Hh, alpha)] = (score, names, coefs)
            for q in (0.1, 0.2):
                add(f"quantrank H{Hh} a{alpha} q{q}", f"Ours: QuantRank (25 measures, ridge learned on past years only), long top {int(q*100)}% / short bottom {int(q*100)}%, rebalance every {Hh} day(s)",
                    f"quantrank H{Hh}", "ours", to_weights(score, q, Hh))
                add(f"quantrank-long H{Hh} a{alpha} q{q}", f"Ours, long only: QuantRank top {int(q*100)}%, rebalance every {Hh} day(s) (measured as the extra over holding all stocks)",
                    f"quantrank-long H{Hh}", "ours", to_weights(score, q, Hh, long_only=True))
    # 3. Kalman pairs
    for K in (5, 10):
        for ent in (1.5, 2.0, 2.5):
            for ext in (0.0, 0.5):
                add(f"pairs K{K} in{ent} out{ext}", f"Ours: Kalman pairs, {K} most correlated pairs re-picked quarterly, enter at z={ent}, exit at z={ext}, max 20 days",
                    "kalman pairs", "ours", kalman_pairs(C, r, days, K, ent, ext))
    print(f"{mk}: {len(V)} variants built ({time.time() - t0:.0f}s)", flush=True)
    ew = np.nanmean(np.r_[r[1:], np.full((1, r.shape[1]), np.nan)], 1)
    ew = np.r_[0.0, np.nan_to_num(ew[:-1])]
    resm, holdm = (days < HOLD_FROM), (days >= HOLD_FROM)
    resm[:260] = False
    ew_info = dict(res_ann=float(ew[resm].mean() * 252 * 100), hold=float(ew[holdm].sum() * 100))
    out, funnel, info = evaluate(mk, V, days, c["cost"])
    # what QuantRank learned (last year's weights, best H)
    imp = None
    sc, names, coefs = qr_scores[(5, 1.0)]
    if coefs:
        w = coefs[max(coefs)]
        imp = sorted(zip(names, w.tolist()), key=lambda x: -abs(x[1]))[:10]
    return dict(mk=mk, label=c["label"], stocks=len(syms), days=[str(days[0]), str(days[-1])], out=out, funnel=funnel, info=info, imp=imp, ew=ew_info)


def row(o):
    return dict(name=o["name"], desc=o["desc"], kind=o["kind"], trades=o["ntr"], ann=o["ann"] * 100, tot=o["tot"] * 100, halves=[o["h1"] * 100, o["h2"] * 100],
                ypos=o["ypos"], c2=o["c2"] * 100, sharpe=o["sr"] * math.sqrt(252), dsr=o["dsr"], dsr_eff=o["dsr_eff"], hold=o["hold"] * 100,
                hold_sharpe=o["hold_sr"], dd=o["dd"] * 100, mc=o["mc"], gates={k: bool(o[k]) for k in ("g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8", "g9")})


GN = {"g1": "1. At least 100 trades", "g2": "2. Both halves profitable", "g3": "3. 60% of years profitable", "g4": "4. Profitable at double costs",
      "g5": "5. 60% of its neighbours profitable", "g6": "6. Walk-forward profitable", "g7": "7. Beats the luck bar", "g8": "8. Holdout (since 10 Nov 2024) profitable", "g9": "9. Monte Carlo 95%"}


def report(results):
    OUT.mkdir(parents=True, exist_ok=True)
    now = datetime.now(timezone(timedelta(hours=5, minutes=30))).strftime("%Y-%m-%d %H:%M IST")
    L = [f"# QuantRank, Kalman pairs and baselines\n\nRun: {now}. Research only: nothing here trades until you say so.\n"]
    js = {"at": now, "markets": {}}
    for R in results:
        out = R["out"]
        L.append(f"## {R['label']}\n\n{R['stocks']} stocks, {R['days'][0]} to {R['days'][1]}. Research until 9 Nov 2024, holdout after. {len(out)} variants.\n")
        L.append("| Gate (on top of the ones before) | Pass alone | Still standing |\n|---|---|---|")
        for k, a, b in R["funnel"]:
            L.append(f"| {GN[k]} | {a} | {b} |")
        i = R["info"]
        L.append(f"\nLuck bar: best of {i['n']} unrelated rules would show a yearly Sharpe of about {i['bar']:.2f} by luck; "
                 f"these are about {i['neff']} truly different ideas, a softer bar of {i['bar_eff']:.2f}.\n")
        P = [o for o in out if o["pass"]]
        PS = [o for o in out if o["pass_soft"] and not o["pass"]]
        L.append(f"**Pass every gate: {len(P)}. Pass with the softer luck bar only: {len(PS)}.**\n")
        L.append("### Best of each kind (by research Sharpe)\n\n| Kind | Rule | Trades | Per year | Halves | Years up | Worst fall | Sharpe | Holdout | Holdout Sharpe | Luck score | Gates failed |\n|---|---|---|---|---|---|---|---|---|---|---|---|")
        for grp in sorted({o["group"] for o in out}):
            g = [o for o in out if o["group"] == grp]
            b = max(g, key=lambda o: o["sr"])
            fails = ",".join(k[1:] for k in ("g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8", "g9") if not b[k]) or "none"
            L.append(f"| {b['kind']} | {b['desc']} | {b['ntr']} | {b['ann']*100:+.1f}% | {b['h1']*100:+.0f}% / {b['h2']*100:+.0f}% | {b['ypos']:.0%} | {b['dd']*100:.0f}% | {b['sr']*math.sqrt(252):.2f} | {b['hold']*100:+.1f}% | {b['hold_sr']:.2f} | {b['dsr']:.2f} | {fails} |")
        ours = [o for o in out if o["kind"] == "ours"]
        base = [o for o in out if o["kind"] == "baseline"]
        if ours and base:
            bo, bb = max(ours, key=lambda o: o["sr"]), max(base, key=lambda o: o["sr"])
            L.append(f"\n**Ours vs the known strategies:** our best ({bo['desc'][:80]}...) research Sharpe {bo['sr']*math.sqrt(252):.2f}, holdout {bo['hold']*100:+.1f}%; "
                     f"best known ({bb['desc'][:80]}...) research Sharpe {bb['sr']*math.sqrt(252):.2f}, holdout {bb['hold']*100:+.1f}%.\n")
        if R["imp"]:
            L.append("### What QuantRank learned (weights for the latest year, 5-day version)\n\nPositive = higher rank predicts a better next week; negative = it predicts a worse one.\n\n| Measure | Weight |\n|---|---|")
            for n_, w in R["imp"]:
                L.append(f"| {n_} | {w:+.4f} |")
        js["markets"][R["mk"]] = dict(stocks=R["stocks"], days=R["days"], funnel=R["funnel"], info=R["info"], imp=R["imp"],
                                      passed=[row(o) for o in P], passed_soft=[row(o) for o in PS],
                                      best=[row(max([o for o in out if o["group"] == g], key=lambda o: o["sr"])) for g in sorted({o["group"] for o in out})])
    L.append("\n## Caveat\n\nThe stock lists are today's names. Stocks that were removed or failed over 25 years are missing, which makes the past look better than it was, most of all for long-only rules.\n")
    (OUT / "quant_model_report.md").write_text("\n".join(L) + "\n", encoding="utf-8")
    (OUT / "quant_model_latest.json").write_text(json.dumps(js, indent=1))
    print("report:", OUT / "quant_model_report.md")


if __name__ == "__main__":
    which = sys.argv[2] if len(sys.argv) > 2 else "both"
    mks = ["us", "in"] if which == "both" else [which]
    report([run_market(mk) for mk in mks])
