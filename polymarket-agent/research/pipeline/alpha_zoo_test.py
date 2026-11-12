"""Test the Vibe-Trading alpha library (Alpha101, GTJA191, Qlib158, academic;
MIT licence, HKUDS/Vibe-Trading) on our stocks with our gates. Research only.

  python -I research/pipeline/alpha_zoo_test.py <vt_zoo_dir> [us|in|both]

<vt_zoo_dir> holds the downloaded `src/factors/...` files (read and checked
11 Nov 2026: pandas/numpy only, no network, file or shell calls). The optional
`bottleneck` speed-up is switched off with a stub config.

Each formula is tested as published (long the highest scores, short the
lowest, 10% each side), rebalanced every 1, 5 and 21 days, after costs, with
the 9 gates from quant_model.py. Then the formulas that did best in the
research years only are combined with walk-forward ridge (like QuantRank)
and tested the same way. Finally every survivor is checked after removing
market risk (beta) and its top 3 contributing stocks.
Data approximations: vwap = (high+low+close)/3, amount = close x volume;
formulas that need sectors are skipped."""
import importlib, json, math, sys, time, traceback, types, warnings
from pathlib import Path
import numpy as np
import pandas as pd

warnings.filterwarnings("ignore")
ZOO = Path(sys.argv[1]).resolve()
WHICH = sys.argv[2] if len(sys.argv) > 2 else "both"
sys.path.insert(0, str(Path(__file__).resolve().parent))
import quant_model as Q  # noqa: E402

# stub for src.config.accessor (only used to decide whether to use bottleneck)
cfg = types.ModuleType("src.config.accessor")
cfg.get_env_config = lambda: types.SimpleNamespace(agent_tuning=types.SimpleNamespace(vibe_trading_disable_bottleneck=True))
sys.modules["src.config"] = types.ModuleType("src.config")
sys.modules["src.config.accessor"] = cfg
sys.path.insert(0, str(ZOO))
OUT = Q.OUT


def zoo_modules():
    mods = []
    for z in ("alpha101", "gtja191", "qlib158", "academic"):
        for f in sorted((ZOO / "src" / "factors" / "zoo" / z).glob("*.py")):
            if f.name != "__init__.py":
                mods.append(f"src.factors.zoo.{z}.{f.stem}")
    return mods


def run_market(mk):
    t0 = time.time()
    c = Q.CFG[mk]
    syms, days, O, H, L, C, V, B = Q.load(mk)
    r = np.vstack([np.full((1, C.shape[1]), np.nan), np.diff(np.log(C), axis=0)])
    idx = pd.to_datetime(days)
    df = lambda a: pd.DataFrame(a, index=idx, columns=syms)
    panel = {"open": df(O), "high": df(H), "low": df(L), "close": df(C), "volume": df(V),
             "vwap": df((H + L + C) / 3), "amount": df(C * V),
             "benchmark_close": pd.Series(B, index=idx)}
    # Leak check (added 11 Nov 2026): our 25-year prices are split/dividend
    # adjusted backwards, so a stock that later grew and split shows a tiny
    # early price. Any formula whose picks depend on the price LEVEL secretly
    # sees the future (buying the lowest adjusted price "earned" +17%/yr,
    # t=2.9, then reversed in the holdout). Each formula is recomputed after
    # a fake split per stock (price / k, volume x k); if its cross-sectional
    # ranks move, it is scale-dependent and excluded.
    rs = np.random.default_rng(1)
    k = pd.Series(np.exp(rs.uniform(np.log(0.1), np.log(10), len(syms))), index=syms)
    split = {key: (val.div(k, axis=1) if key in ("open", "high", "low", "close", "vwap") else val.mul(k, axis=1) if key == "volume" else val)
             for key, val in panel.items()}
    V_ = []
    done, skipped, failed, leaky = 0, [], [], []
    for name in zoo_modules():
        try:
            m = importlib.import_module(name)
            meta = getattr(m, "__alpha_meta__", {})
            if meta.get("requires_sector"):
                skipped.append(name); continue
            a = m.compute(dict(panel))
            if isinstance(a, pd.Series):
                skipped.append(name); continue
            A = a.reindex(index=idx, columns=syms).to_numpy(float)
            if np.isfinite(A).sum() < A.size * 0.2:
                skipped.append(name); continue
            A2 = m.compute(dict(split)).reindex(index=idx, columns=syms).to_numpy(float)
            rows = np.arange(300, len(idx), 97)
            ra, rb = Q.xrank(A[rows]), Q.xrank(A2[rows])
            ok = np.isfinite(ra) & np.isfinite(rb)
            if ok.sum() == 0 or np.abs(ra[ok] - rb[ok]).mean() > 0.01:
                leaky.append(meta.get("id", name)); continue
            aid = meta.get("id", name.split(".")[-1])
            for Hh in (1, 5, 21):
                g, tu, tr = Q.pnl(Q.to_weights(A, 0.1, Hh), r, c["cost"])
                V_.append(dict(name=f"{mk} {aid} H{Hh}", desc=f"{aid} ({', '.join(meta.get('theme', []))}): long top 10% / short bottom 10%, every {Hh} day(s)",
                               group=f"{mk} {aid}", kind="zoo", g=g, tu=tu, tr=tr, aid=aid, A=None))
            done += 1
            if done % 50 == 0:
                print(f"  {mk}: {done} formulas computed ({time.time() - t0:.0f}s)", flush=True)
        except Exception as e:  # noqa: BLE001
            failed.append((name, type(e).__name__))
    print(f"{mk}: {done} formulas tested, {len(leaky)} excluded as price-level leaks, {len(skipped)} skipped (need sectors/other data), {len(failed)} failed to run ({time.time() - t0:.0f}s)", flush=True)
    out, funnel, info = Q.evaluate(mk, V_, days, c["cost"])
    return dict(mk=mk, label=c["label"], days=days, r=r, syms=syms, out=out, funnel=funnel, info=info,
                done=done, skipped=len(skipped), failed=failed, leaky=leaky, panel=panel, B=B)


def combo(R):
    """Walk-forward combination: in each year Y, take the 20 formulas with the best
    research-to-date Sharpe (H=5 version, years before Y only), rank them, and let
    ridge learn the weights on years before Y."""
    mk, days, r = R["mk"], R["days"], R["r"]
    years = np.array([int(d[:4]) for d in days])
    by_id = {o["aid"]: o for o in R["out"] if o["name"].endswith(" H5")}
    ids = list(by_id)
    # recompute the scores we need (memory: one at a time)
    scores = {}
    for name in zoo_modules():
        aid = name.split(".")[-1]
        meta_id = None
        try:
            m = importlib.import_module(name)
            meta_id = getattr(m, "__alpha_meta__", {}).get("id", aid)
            if meta_id not in by_id:
                continue
            a = m.compute(dict(R["panel"])).reindex(index=pd.to_datetime(days), columns=R["syms"]).to_numpy(float)
            scores[meta_id] = Q.xrank(a).astype(np.float32)
        except Exception:  # noqa: BLE001
            pass
    p1 = {k: by_id[k]["p1"] for k in scores}
    final = np.full(r.shape, np.nan)
    chosen = {}
    lp = np.nancumsum(np.nan_to_num(r), 0)
    fwd = np.full_like(lp, np.nan); fwd[:-5] = lp[5:] - lp[:-5]; fwd[~np.isfinite(r)] = np.nan
    Y = Q.xrank(fwd)
    for yr in range(years.min() + 4, years.max() + 1):
        past = years < yr
        past[:260] = False
        if past.sum() < 500:
            continue
        sr = {k: (v[past].mean() / v[past].std()) if v[past].std() > 0 else -9 for k, v in p1.items()}
        top = sorted(sr, key=lambda k: -sr[k])[:20]
        chosen[yr] = top
        X = np.stack([scores[k] for k in top], 2)
        tr = np.where(past)[0]; tr = tr[tr < tr[-1] - 5]
        Xt, yt = X[tr].reshape(-1, len(top)), Y[tr].reshape(-1)
        m = np.isfinite(yt) & np.isfinite(Xt).all(1)
        w = np.linalg.solve(Xt[m].T @ Xt[m] + 1.0 * m.sum() * np.eye(len(top)), Xt[m].T @ yt[m])
        te = years == yr
        s = np.einsum("dsk,k->ds", np.nan_to_num(X[te]), w)
        s[~np.isfinite(X[te]).all(2)] = np.nan
        final[te] = s
    V_ = []
    for Hh in (1, 5, 21):
        for q in (0.1, 0.2):
            g, tu, tr = Q.pnl(Q.to_weights(final, q, Hh), r, Q.CFG[mk]["cost"])
            V_.append(dict(name=f"{mk} zoo-combo H{Hh} q{q}", desc=f"Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top {int(q*100)}% / short bottom {int(q*100)}%, every {Hh} day(s)",
                           group=f"{mk} zoo-combo", kind="ours", g=g, tu=tu, tr=tr))
            g, tu, tr = Q.pnl(Q.to_weights(final, q, Hh, long_only=True), r, Q.CFG[mk]["cost"])
            V_.append(dict(name=f"{mk} zoo-combo-long H{Hh} q{q}", desc=f"Ours, long only: best 20 formulas combined, top {int(q*100)}% minus holding all stocks, every {Hh} day(s)",
                           group=f"{mk} zoo-combo-long", kind="ours", g=g, tu=tu, tr=tr))
    out, funnel, info = Q.evaluate(mk, V_, days, Q.CFG[mk]["cost"])
    return out, funnel, info, chosen.get(max(chosen)) if chosen else []


def risk_check(R, o):
    """beta-adjusted extra and the result without the top 3 contributing stocks (holdout)."""
    days = R["days"]
    ew = np.r_[0.0, np.nan_to_num(np.nanmean(R["r"][1:], 1))]
    out = {}
    for nm, m in (("research", (days < Q.HOLD_FROM) & (np.arange(len(days)) >= 260)), ("holdout", days >= Q.HOLD_FROM)):
        y, x = o["p1"][m], ew[m]
        b = np.cov(y, x)[0, 1] / x.var() if x.var() > 0 else 0
        a = y - b * x
        out[nm] = dict(extra=y.mean() * 252 * 100, beta=b, alpha=a.mean() * 252 * 100, t=a.mean() / a.std() * math.sqrt(len(a)) if a.std() > 0 else 0)
    return out


def report(results):
    import datetime as dt
    now = dt.datetime.now(dt.timezone(dt.timedelta(hours=5, minutes=30))).strftime("%Y-%m-%d %H:%M IST")
    L = [f"# Alpha library test (Vibe-Trading zoo: Alpha101, GTJA191, Qlib158, academic)\n\nRun: {now}. Research only.\n"]
    js = {"at": now, "markets": {}}
    for R in results:
        out = R["out"]
        L.append(f"## {R['label']}\n\n{R['done']} formulas run x 3 holding periods = {len(out)} variants. {R['skipped']} skipped (need sector or other data), {len(R['failed'])} failed to run on our data.\n")
        L.append("| Gate (on top of the ones before) | Pass alone | Still standing |\n|---|---|---|")
        for (k, a, b) in R["funnel"]:
            L.append(f"| {Q.GN[k]} | {a} | {b} |")
        i = R["info"]
        L.append(f"\nLuck bar: yearly Sharpe {i['bar']:.2f} (strict), {i['bar_eff']:.2f} (softer, about {i['neff']} distinct ideas).\n")
        P = [o for o in out if o["pass"]]; PS = [o for o in out if o["pass_soft"] and not o["pass"]]
        L.append(f"**Single formulas passing every gate: {len(P)}; with the softer bar only: {len(PS)}.**\n")
        top = sorted(out, key=lambda o: -o["sr"])[:15]
        L.append("### Top 15 single formulas by research Sharpe\n\n| Formula | Trades | Per year | Halves | Years up | Sharpe | Holdout | Holdout Sharpe | Gates failed |\n|---|---|---|---|---|---|---|---|---|")
        for o in top:
            fails = ",".join(k[1:] for k in ("g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8", "g9") if not o[k]) or "none"
            L.append(f"| {o['desc']} | {o['ntr']} | {o['ann']*100:+.1f}% | {o['h1']*100:+.0f}% / {o['h2']*100:+.0f}% | {o['ypos']:.0%} | {o['sr']*math.sqrt(252):.2f} | {o['hold']*100:+.1f}% | {o['hold_sr']:.2f} | {fails} |")
        # library-wide picture
        res_pos = np.mean([o["tot"] > 0 for o in out]); hold_pos = np.mean([o["hold"] > 0 for o in out])
        best_res = sorted(out, key=lambda o: -o["sr"])[: max(1, len(out) // 10)]
        L.append(f"\nAcross the library: {res_pos:.0%} of variants were profitable in the research years and {hold_pos:.0%} in the holdout. "
                 f"The top 10% by research Sharpe made {np.mean([o['hold'] for o in best_res])*100:+.1f}% on average in the holdout "
                 f"(all variants: {np.mean([o['hold'] for o in out])*100:+.1f}%). If the best in research stay best in the holdout, the library has signal.\n")
        co, cf, ci, chosen = R["combo"]
        L.append("### Our combination of the best 20 formulas (chosen and weighted on past years only)\n\n| Rule | Trades | Per year | Halves | Years up | Sharpe | Holdout | Gates failed | Beta-adjusted research / holdout |\n|---|---|---|---|---|---|---|---|---|")
        for o in co:
            rc = risk_check(R, o)
            fails = ",".join(k[1:] for k in ("g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8", "g9") if not o[k]) or "none"
            L.append(f"| {o['desc']} | {o['ntr']} | {o['ann']*100:+.1f}% | {o['h1']*100:+.0f}% / {o['h2']*100:+.0f}% | {o['ypos']:.0%} | {o['sr']*math.sqrt(252):.2f} | {o['hold']*100:+.1f}% | {fails} | {rc['research']['alpha']:+.1f}% / {rc['holdout']['alpha']:+.1f}% |")
        L.append(f"\nFormulas chosen for the latest year: {', '.join(chosen)}\n")
        survivors = [o for o in out + co if o["pass"] or o["pass_soft"]]
        if survivors:
            L.append("### Risk check of every survivor (extra after removing market risk)\n\n| Rule | Research beta-adjusted | Holdout beta-adjusted | Holdout beta |\n|---|---|---|---|")
            for o in survivors:
                rc = risk_check(R, o)
                L.append(f"| {o['desc']} | {rc['research']['alpha']:+.1f}%/yr (t {rc['research']['t']:.1f}) | {rc['holdout']['alpha']:+.1f}%/yr (t {rc['holdout']['t']:.1f}) | {rc['holdout']['beta']:+.2f} |")
        js["markets"][R["mk"]] = dict(done=R["done"], funnel=R["funnel"], info=R["info"], passed=[o["name"] for o in P], passed_soft=[o["name"] for o in PS],
                                      combo=[(o["name"], o["pass"], o["pass_soft"], o["hold"]) for o in co], failed=R["failed"][:30])
    (OUT / "alpha_zoo_report.md").write_text("\n".join(L) + "\n", encoding="utf-8")
    (OUT / "alpha_zoo_latest.json").write_text(json.dumps(js, indent=1, default=float))
    print("report:", OUT / "alpha_zoo_report.md")


if __name__ == "__main__":
    mks = ["us", "in"] if WHICH == "both" else [WHICH]
    results = []
    for mk in mks:
        R = run_market(mk)
        R["combo"] = combo(R)
        results.append(R)
    report(results)
