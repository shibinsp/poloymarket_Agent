"""Command line: run-once, serve, golden-eval, shadow-report, ingest, forecast-eval."""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import time
from datetime import datetime, time as dtime, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

from .config import load_config, load_env_file

IST = ZoneInfo("Asia/Kolkata")
PRE_MARKET = dtime(8, 45)
SESSION_END = dtime(15, 30)


def _cfg(args):
    if args.env_file:
        load_env_file(args.env_file)
    cfg = load_config(args.config)
    if args.state_dir:
        cfg.state_dir = args.state_dir
    return cfg


def _print_run(res) -> None:
    out = {
        "run_id": res.run_id,
        "status": res.status,
        "classified": len(res.classifications),
        "rejected": res.rejected,
        "errors": res.errors,
        "context": json.loads(res.context.model_dump_json()) if res.context else None,
    }
    print(json.dumps(out, indent=2, ensure_ascii=False))


def cmd_run_once(args) -> int:
    from .pipeline import run_once

    res = asyncio.run(run_once(_cfg(args), jsonl_inputs=args.input, publish=not args.no_publish))
    _print_run(res)
    return 0 if res.status == "published" else 2


def next_run_at(now: datetime, refresh_minutes: int) -> datetime:
    """Pre-market at 08:45 IST, then every ``refresh_minutes`` until 15:30, weekdays."""
    local = now.astimezone(IST)
    day = local.date()
    for _ in range(8):
        if day.weekday() < 5:
            start = datetime.combine(day, PRE_MARKET, IST)
            end = datetime.combine(day, SESSION_END, IST)
            t = start
            while t <= end:
                if t > local:
                    return t
                t += timedelta(minutes=refresh_minutes)
        day += timedelta(days=1)
    raise RuntimeError("no run slot found")


def cmd_serve(args) -> int:
    from .pipeline import run_once

    cfg = _cfg(args)
    print(f"analyst serving in shadow mode; state={cfg.state_dir}", flush=True)
    while True:
        nxt = next_run_at(datetime.now(IST), cfg.refresh_minutes)
        print(f"next run {nxt.isoformat()}", flush=True)
        time.sleep(max(0.0, (nxt - datetime.now(IST)).total_seconds()))
        res = asyncio.run(run_once(cfg))
        print(f"{res.run_id} {res.status} classified={len(res.classifications)} errors={len(res.errors)}", flush=True)


def cmd_golden_eval(args) -> int:
    from .golden import evaluate, load_golden
    from .pipeline import make_classifier
    from .store import Store
    from .universe import load_universe

    cfg = _cfg(args)
    if args.keyword:
        cfg.llm.provider = "none"
    today = datetime.now(IST).date()
    uni = load_universe(cfg.state_path / "cache", cfg.sources.universe_csv, cfg.sources.universe_url, cfg.sources.http_timeout_s, today)
    golden = load_golden(args.file, uni, args.split, cfg.llm.body_chars)
    store = Store(cfg.state_path)
    clf = make_classifier(cfg, store)
    report = asyncio.run(evaluate(golden, clf, today, cfg.compose.exclude_severity, repeat=args.repeat))
    print(json.dumps({"classifier": clf.name, "split": args.split, **report.as_dict()}, indent=2))
    return 0


def cmd_shadow_report(args) -> int:
    from .shadow import evaluate, load_fills, pair_round_trips
    from .store import Store

    cfg = _cfg(args)
    contexts = Store(cfg.state_path).contexts()
    trips = pair_round_trips(load_fills(args.engine_db))
    rep = evaluate(trips, contexts)
    print(json.dumps({
        "round_trips": rep.trades,
        "with_active_context": rep.trades_with_context,
        "would_exclude": rep.excluded,
        "would_resize": rep.resized,
        "actual_net_inr": round(rep.actual_net, 2),
        "restricted_net_inr": round(rep.restricted_net, 2),
        "value_of_restrictions_inr": round(rep.value, 2),
        "trading_days": rep.days,
        "gate": "enable only if value > 0 over 40+ trading days",
        "by_day": {k: round(v, 2) for k, v in rep.by_day.items()},
    }, indent=2))
    return 0


def cmd_ingest(args) -> int:
    from .rag import ingest_file
    from .store import Store

    cfg = _cfg(args)
    published = datetime.fromisoformat(args.date) if args.date else datetime.now(IST)
    if published.tzinfo is None:
        published = published.replace(tzinfo=IST)
    doc_id = ingest_file(Store(cfg.state_path), Path(args.file), args.symbol.upper(), args.title, published)
    print(doc_id)
    return 0


def cmd_forecast_eval(args) -> int:
    from .forecast import direction_features, walk_forward_direction
    from .sources.market import download_daily, yahoo_symbol

    frames = download_daily([yahoo_symbol(s) for s in args.symbols], period=args.period)
    summary = {}
    for s in args.symbols:
        df = frames.get(yahoo_symbol(s))
        if df is None:
            summary[s] = "no data"
            continue
        folds = walk_forward_direction(direction_features(df))
        if not folds:
            summary[s] = "not enough history"
            continue
        hit = sum(f.hit_rate * f.n for f in folds) / sum(f.n for f in folds)
        base = sum(f.baseline_hit_rate * f.n for f in folds) / sum(f.n for f in folds)
        summary[s] = {"folds": len(folds), "oos_hit_rate": round(hit, 4), "majority_baseline": round(base, 4), "edge_pts": round((hit - base) * 100, 2)}
    print(json.dumps({"note": "research only; never published to the trading context", "results": summary}, indent=2))
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="moneyai-analyst", description="Restrict-only market analyst (shadow mode)")
    p.add_argument("--config", help="TOML config (see config.example.toml)")
    p.add_argument("--env-file", help="KEY=VALUE file with LLM_API_KEY, e.g. ../polymarket-agent/.env")
    p.add_argument("--state-dir", help="override state directory")
    sub = p.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("run-once", help="run the pipeline once and publish context.json")
    r.add_argument("--input", action="append", help="extra JSONL items (replay); repeatable")
    r.add_argument("--no-publish", action="store_true", help="record the run but do not write context.json")
    r.set_defaults(fn=cmd_run_once)

    s = sub.add_parser("serve", help="run pre-market and every refresh interval on weekdays")
    s.set_defaults(fn=cmd_serve)

    g = sub.add_parser("golden-eval", help="score the classifier on a labelled golden set")
    g.add_argument("file")
    g.add_argument("--split", choices=["tune", "holdout", "all"], default="tune")
    g.add_argument("--keyword", action="store_true", help="evaluate the keyword baseline")
    g.add_argument("--repeat", action="store_true", help="run twice without cache to measure stability")
    g.set_defaults(fn=cmd_golden_eval)

    sr = sub.add_parser("shadow-report", help="value of restrictions against the engine's trades")
    sr.add_argument("--engine-db", default="../polymarket-agent/polymarket-agent.db")
    sr.set_defaults(fn=cmd_shadow_report)

    i = sub.add_parser("ingest", help="add a filing/transcript text file to the RAG corpus")
    i.add_argument("file")
    i.add_argument("--symbol", required=True)
    i.add_argument("--title", required=True)
    i.add_argument("--date", help="ISO date of the document")
    i.set_defaults(fn=cmd_ingest)

    f = sub.add_parser("forecast-eval", help="research: walk-forward next-day direction vs baseline")
    f.add_argument("symbols", nargs="+")
    f.add_argument("--period", default="10y")
    f.set_defaults(fn=cmd_forecast_eval)

    args = p.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
