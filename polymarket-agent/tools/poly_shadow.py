"""Polymarket live-price shadow test (11 Nov 2026). Places NO bets.

The rule under test (research/pipeline/poly_retest.py, WATCH): about 168 h
before a market's scheduled end, buy the side whose price is 50-60c, hold to
resolution. The backtest used hourly prices and assumed a 1c cost; the real
ask can be far worse. This logs what the rule would really have paid.

  python -I tools/poly_shadow.py scan      once: find markets 162-174 h from their end, log the real book
  python -I tools/poly_shadow.py settle    once: settle logged picks whose market has resolved
  python -I tools/poly_shadow.py loop      scan + settle every hour, forever
  python -I tools/poly_shadow.py report    summary

Log: research/poly_shadow.jsonl (one line per pick, then one per settlement).
For each pick it records the side's best ask (what a buy would pay now), best
bid, mid, size on the ask, and the hourly-style price the backtest would have
used, so the cost gap is measured directly. Read-only public endpoints."""
import json, sys, time, urllib.parse, urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LOG = ROOT / "research" / "poly_shadow.jsonl"
UA = {"User-Agent": "research-shadow/1.0"}
LO, HI = 0.50, 0.60
H_FROM, H_TO = 162, 174


def get(url):
    for a in range(4):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=30) as r:
                return json.load(r)
        except Exception:  # noqa: BLE001
            time.sleep(2 * (a + 1))
    return None


def read_log():
    if not LOG.exists():
        return []
    return [json.loads(l) for l in LOG.read_text(encoding="utf-8").splitlines() if l.strip()]


def append(row):
    LOG.parent.mkdir(parents=True, exist_ok=True)
    with LOG.open("a", encoding="utf-8") as f:
        f.write(json.dumps(row) + "\n")


def book(token):
    b = get("https://clob.polymarket.com/book?token_id=" + token)
    if not b:
        return None
    bids = sorted(((float(x["price"]), float(x["size"])) for x in b.get("bids", [])), reverse=True)
    asks = sorted((float(x["price"]), float(x["size"])) for x in b.get("asks", []))
    return dict(bid=bids[0][0] if bids else None, ask=asks[0][0] if asks else None,
                ask_size=asks[0][1] if asks else 0.0, ask_usd_within_2c=sum(p * s for p, s in asks if asks and p <= asks[0][0] + 0.02))


def scan():
    now = datetime.now(timezone.utc)
    lo, hi = now + timedelta(hours=H_FROM), now + timedelta(hours=H_TO)
    seen_ev = {r["event"] for r in read_log() if r.get("type") == "pick"}
    seen_cid = {r["condition_id"] for r in read_log() if r.get("type") == "pick"}
    found = 0
    for off in range(0, 3000, 500):
        q = dict(closed="false", active="true", limit=500, offset=off,
                 end_date_min=lo.strftime("%Y-%m-%dT%H:%M:%SZ"), end_date_max=hi.strftime("%Y-%m-%dT%H:%M:%SZ"))
        page = get("https://gamma-api.polymarket.com/markets?" + urllib.parse.urlencode(q))
        if not page:
            break
        for m in page:
            try:
                toks = json.loads(m["clobTokenIds"]); outs = json.loads(m["outcomes"]); px = [float(x) for x in json.loads(m["outcomePrices"])]
            except Exception:  # noqa: BLE001
                continue
            if len(toks) != 2 or not m.get("acceptingOrders"):
                continue
            ev = ((m.get("events") or [{}])[0] or {}).get("id") or m.get("id")
            if ev in seen_ev or m["conditionId"] in seen_cid:
                continue
            for i in (0, 1):
                if LO <= px[i] < HI:
                    bk = book(toks[i])
                    row = dict(type="pick", at=now.isoformat(), condition_id=m["conditionId"], event=ev, question=m.get("question"),
                               side=outs[i], side_index=i, token=toks[i], end=m.get("endDate"), game_start=m.get("gameStartTime"),
                               shown_price=px[i], hours_to_end=round((datetime.fromisoformat(m["endDate"].replace("Z", "+00:00")) - now).total_seconds() / 3600, 1),
                               liquidity=m.get("liquidityNum"), **(bk or {}))
                    append(row); seen_ev.add(ev); seen_cid.add(m["conditionId"]); found += 1
                    break
        if len(page) < 500:
            break
    return found


def settle():
    rows = read_log()
    done = {r["condition_id"] for r in rows if r.get("type") == "settle"}
    n = 0
    for r in rows:
        if r.get("type") != "pick" or r["condition_id"] in done:
            continue
        m = None
        for closed in ("true", "false"):
            page = get("https://gamma-api.polymarket.com/markets?" + urllib.parse.urlencode({"condition_ids": r["condition_id"], "closed": closed}))
            m = next((x for x in (page or []) if x.get("conditionId") == r["condition_id"]), None)
            if m:
                break
        if not m or not m.get("closed"):
            continue
        try:
            px = [float(x) for x in json.loads(m["outcomePrices"])]
        except Exception:  # noqa: BLE001
            continue
        if max(px) < 0.98:
            append(dict(type="settle", condition_id=r["condition_id"], at=datetime.now(timezone.utc).isoformat(), result="void", prices=px)); n += 1
            continue
        won = px[r["side_index"]] > 0.5
        def ret(price):
            return None if not price else ((1 / price - 1) if won else -1.0)
        append(dict(type="settle", condition_id=r["condition_id"], at=datetime.now(timezone.utc).isoformat(), won=won,
                    ret_at_ask=ret(r.get("ask")), ret_at_shown_plus_1c=ret(min(r["shown_price"] + 0.01, 0.99))))
        n += 1
    return n


def report():
    rows = read_log()
    picks = {r["condition_id"]: r for r in rows if r.get("type") == "pick"}
    sets = [r for r in rows if r.get("type") == "settle" and r.get("result") != "void"]
    gaps = [p["ask"] - p["shown_price"] for p in picks.values() if p.get("ask") is not None]
    print(f"picks logged: {len(picks)}; settled: {len(sets)}")
    if gaps:
        gaps.sort()
        print(f"real ask minus shown price: median {gaps[len(gaps)//2]*100:+.1f}c, mean {sum(gaps)/len(gaps)*100:+.1f}c (backtest assumed +1c)")
    if sets:
        won = sum(s["won"] for s in sets)
        ra = [s["ret_at_ask"] for s in sets if s.get("ret_at_ask") is not None]
        rs = [s["ret_at_shown_plus_1c"] for s in sets if s.get("ret_at_shown_plus_1c") is not None]
        print(f"won {won}/{len(sets)} ({won/len(sets):.0%}); avg return per bet at the real ask {sum(ra)/max(len(ra),1)*100:+.1f}% "
              f"vs at shown price + 1c {sum(rs)/max(len(rs),1)*100:+.1f}% (backtest: +11.5%, 59% won)")
        print(f"$2 a bet: {sum(ra)*2:+.2f} at the real ask")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "report"
    if cmd == "scan":
        print("new picks:", scan())
    elif cmd == "settle":
        print("settled:", settle())
    elif cmd == "loop":
        while True:
            try:
                a = scan(); b = settle()
                print(datetime.now(timezone.utc).isoformat(timespec="seconds"), "picks", a, "settled", b, flush=True)
            except Exception as e:  # noqa: BLE001
                print("loop error", type(e).__name__, flush=True)
            time.sleep(3600)
    else:
        report()
