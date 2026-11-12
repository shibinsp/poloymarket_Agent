"""The user's "sell high, buy low" rule, followed on paper beside the real US
ORB trades (started 4 Nov 2026 at the user's request: "we will compare it on
Friday"). No orders are ever sent.

The rule, fixed before any live result (no tuning):
  sell high  once the trade has been at least ARM (0.5%) in profit, sell as soon
             as a close is TRAIL (1%) below the best close since entry
  buy low    after a sale, buy again when a close is REBUY_DROP (1%) below the
             sale price and the last 5 minutes closed up; at most MAX_REBUYS (2)
             a day, then the same sell rule
  safety     the trade's original ORB stop throughout; all flat at 15:45 ET
  costs      SLIP (0.05%) on every buy and sell
Each decision is taken on a 1-minute close and filled at the next minute's open.

  --day YYYY-MM-DD  shadow that day's real ORB / burst trades (read-only from
                    the agent's database and log, SIP 1-minute bars from Alpaca,
                    which are free 15 minutes after the fact) and append the
                    result to research/sell_high_shadow.jsonl
  --report          the running comparison: shadow against the real trades
Keys from the agent's .env, never printed. Run with python -I.
"""
import json, sqlite3, sys, time, urllib.parse, urllib.request
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "research" / "sell_high_shadow.jsonl"
NY = ZoneInfo("America/New_York")
ARM, TRAIL, REBUY_DROP, MAX_REBUYS, SLIP = 0.005, 0.01, 0.01, 2, 0.0005
FLAT = 15 * 60 + 45


def shadow(bars, entry_i, buy, entry_px, stop, flat_i):
    """Run the rule over [(minute, o, h, l, c)] from bar entry_i (filled at entry_px).
    Returns (return as a fraction of one position, list of actions)."""
    sign = 1 if buy else -1
    total, actions = 0.0, []
    pos, px_in, best, rebuys, sold_at = True, entry_px, entry_px, 0, None
    for k in range(entry_i, len(bars)):
        m, o, h, l, c = bars[k]
        if m >= flat_i:
            if pos:
                out = o * (1 - sign * SLIP)
                total += sign * (out / px_in - 1); actions.append(("flat", m, round(out, 2)))
                pos = False
            break
        nxt = bars[k + 1][1] if k + 1 < len(bars) else c
        if pos:
            if (buy and l <= stop) or (not buy and h >= stop):
                out = (min(o, stop) if buy else max(o, stop)) * (1 - sign * SLIP)
                total += sign * (out / px_in - 1); actions.append(("stop", m, round(out, 2)))
                pos = False; sold_at = None
                continue
            best = max(best, c) if buy else min(best, c)
            gain = sign * (best / px_in - 1)
            drop = sign * (best - c) / best
            if gain >= ARM and drop >= TRAIL:
                out = nxt * (1 - sign * SLIP)
                total += sign * (out / px_in - 1); actions.append(("sell high", m, round(out, 2)))
                pos, sold_at = False, out
        elif sold_at is not None and rebuys < MAX_REBUYS and k >= 5:
            fell = sign * (sold_at - c) / sold_at
            bounced = sign * (c - bars[k - 5][4]) > 0
            if fell >= REBUY_DROP and bounced and (c > stop if buy else c < stop):
                px_in = nxt * (1 + sign * SLIP)
                pos, best, rebuys = True, px_in, rebuys + 1
                actions.append(("buy low", m, round(px_in, 2)))
    if pos:
        out = bars[-1][4] * (1 - sign * SLIP)
        total += sign * (out / px_in - 1); actions.append(("end", bars[-1][0], round(out, 2)))
    return total, actions


def env():
    e = {}
    for line in (ROOT / ".env").read_text(encoding="utf-8").splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            k, v = line.split("=", 1)
            e[k.strip()] = v.strip().strip('"').strip("'")
    return e


def sip_minutes(sym, day, e):
    """Regular-session SIP 1-minute bars for one day: [(minute from midnight ET, o, h, l, c)]."""
    start = datetime(day.year, day.month, day.day, 9, 30, tzinfo=NY).astimezone(timezone.utc)
    end = start + timedelta(hours=6, minutes=30)
    q = urllib.parse.urlencode(dict(timeframe="1Min", start=start.isoformat().replace("+00:00", "Z"),
                                    end=end.isoformat().replace("+00:00", "Z"), feed="sip", limit=10000, adjustment="split"))
    hdr = {"APCA-API-KEY-ID": e.get("ALPACA_API_KEY_ID", ""), "APCA-API-SECRET-KEY": e.get("ALPACA_API_SECRET_KEY", "")}
    for attempt in range(4):
        try:
            with urllib.request.urlopen(urllib.request.Request(f"https://data.alpaca.markets/v2/stocks/{sym}/bars?{q}", headers=hdr), timeout=30) as r:
                bars = json.load(r).get("bars") or []
            break
        except Exception:  # noqa: BLE001
            if attempt == 3:
                raise RuntimeError(f"{sym}: Alpaca bars unavailable") from None
            time.sleep(2 * (attempt + 1))
    out = []
    for b in bars:
        t = datetime.fromisoformat(b["t"].replace("Z", "+00:00")).astimezone(NY)
        out.append((t.hour * 60 + t.minute, b["o"], b["h"], b["l"], b["c"]))
    return out


def original_stops():
    """trade_id -> the stop logged when the ORB / burst entry was sent (the DB keeps the moved stop)."""
    stops = {}
    log = ROOT / "agent.log"
    with log.open(encoding="utf-8", errors="replace") as f:
        for line in f:
            if "entry submitted to the Alpaca paper account" not in line:
                continue
            try:
                fl = json.loads(line)["fields"]
                stops[int(fl["trade_id"])] = float(fl["stop"])
            except Exception:  # noqa: BLE001
                continue
    return stops


def run_day(day):
    e = env()
    con = sqlite3.connect(f"file:{ROOT / 'polymarket-agent.db'}?mode=ro", uri=True)
    rows = con.execute(
        "SELECT id, symbol, side, avg_fill_price, entry_price, quantity, status, realized_pnl, created_at, strategy "
        "FROM trades WHERE venue_id='alpaca' AND strategy IN ('orb','us_burst') AND date(created_at)=?", (day.isoformat(),)).fetchall()
    stops = original_stops()
    done = set()
    if OUT.exists():
        for line in OUT.read_text(encoding="utf-8").splitlines():
            done.add(json.loads(line)["trade_id"])
    OUT.parent.mkdir(exist_ok=True)
    for tid, sym, side, fill, entry, qty, status, real, created, strat in rows:
        if tid in done:
            print(f"{sym}: already recorded"); continue
        if status != "CLOSED" or tid not in stops:
            print(f"{sym}: not closed yet or no logged stop; skipped"); continue
        buy = side.upper() == "BUY"
        px = float(fill or entry); q = float(qty)
        t_in = datetime.fromisoformat(created.replace(" ", "T")).replace(tzinfo=timezone.utc).astimezone(NY)
        m_in = t_in.hour * 60 + t_in.minute
        bars = sip_minutes(sym, day, e)
        k = next((i for i, b in enumerate(bars) if b[0] > m_in), None)
        if k is None:
            print(f"{sym}: no bars after entry"); continue
        ret, actions = shadow(bars, k, buy, px, stops[tid], FLAT)
        rec = dict(day=day.isoformat(), trade_id=tid, symbol=sym, side=side, strategy=strat, entry=px, qty=q,
                   stop=stops[tid], real_pnl=round(float(real or 0), 2), shadow_pnl=round(ret * px * q, 2),
                   actions=actions)
        with OUT.open("a", encoding="utf-8") as f:
            f.write(json.dumps(rec) + "\n")
        print(f"{sym} {side}: real ${rec['real_pnl']:+.2f}  shadow ${rec['shadow_pnl']:+.2f}  " +
              "; ".join(f"{a} {m // 60:02d}:{m % 60:02d} @{p}" for a, m, p in actions))


def report():
    if not OUT.exists():
        print("no shadow trades yet"); return
    recs = [json.loads(x) for x in OUT.read_text(encoding="utf-8").splitlines()]
    real = sum(r["real_pnl"] for r in recs); sh = sum(r["shadow_pnl"] for r in recs)
    for r in recs:
        print(f"{r['day']} {r['symbol']:<6} {r['side']:<4} real ${r['real_pnl']:+7.2f}  shadow ${r['shadow_pnl']:+7.2f}  "
              f"({len([a for a in r['actions'] if a[0] in ('sell high', 'buy low')])} extra trades)")
    print(f"TOTAL over {len(recs)} trades: real ${real:+.2f}  shadow (sell high, buy low) ${sh:+.2f}")


if __name__ == "__main__":
    if "--report" in sys.argv:
        report()
    elif "--day" in sys.argv:
        run_day(date.fromisoformat(sys.argv[sys.argv.index("--day") + 1]))
    else:
        print(__doc__)
