"""Spread book fill quality (11 Nov 2026). Read-only; places nothing.

The 2.75-year replay (research/pipeline/spread_history.py) showed the book
only pays if fills are near the mid: break-even is about 1c per option leg.
This measures our real fills:

  entry: mid credit quoted at 11:00 and the natural (worst) credit, both from
         the agent's log ("sent at the mid"), against the credit we got
         ("spread sold"). Cost per leg = (mid - fill) / 2.
  exit:  the price paid to buy the short call back (exit_debit + the long
         leg's value, which the book takes as ~0 at expiry) against the
         short call's volume-weighted traded price in that minute (Alpaca
         1-minute option bars).

  python -I tools/spread_fills.py [.env]
Writes research/spread_fills.json and prints a short summary."""
import glob, json, re, sys, urllib.parse, urllib.request
from datetime import datetime, timedelta
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "research" / "spread_fills.json"
BREAK_EVEN_PER_LEG = 0.01


def logs():
    files = sorted(glob.glob(str(ROOT / "logs" / "*.log"))) + sorted(glob.glob(str(ROOT / "agent-*.log"))) + [str(ROOT / "agent.log")]
    seen = set()
    for f in files:
        try:
            for line in open(f, encoding="utf-8", errors="ignore"):
                if "Spread book" in line and line not in seen:
                    seen.add(line)
                    yield line
        except OSError:
            pass


def parse():
    ent = {}
    for line in logs():
        try:
            j = json.loads(line)
        except ValueError:
            continue
        f, ts = j.get("fields", {}), j.get("timestamp", "")
        u, day = f.get("underlying"), ts[:10]
        if not u:
            continue
        k = (day, u)
        msg = f.get("message", "")
        if "sent at the mid" in msg:
            ent.setdefault(k, {})
            ent[k].update(mid=float(f["credit"]), natural=float(f["natural"]), quoted_at=ts, short=f.get("short"), long=f.get("long"))
        elif "spread sold" in msg:
            ent.setdefault(k, {})["fill"] = float(f["credit"])
            ent[k]["filled_at"] = ts
    return ent


def api(path):
    return json.load(urllib.request.urlopen("http://127.0.0.1:8088/api/" + path, timeout=20))


def alpaca_headers(env_path):
    env = {}
    for line in open(env_path, encoding="utf-8"):
        if "=" in line and not line.lstrip().startswith("#"):
            k, v = line.split("=", 1); env[k.strip()] = v.strip().strip('"').strip("'")
    return {"APCA-API-KEY-ID": env["ALPACA_API_KEY_ID"], "APCA-API-SECRET-KEY": env["ALPACA_API_SECRET_KEY"]}


def occ(u, d, k):
    return f"{u}{d[2:4]}{d[5:7]}{d[8:10]}C{int(round(float(k) * 1000)):08d}"


def vwap_at(h, sym, at):
    t = datetime.fromisoformat(at.replace("Z", "+00:00"))
    q = dict(symbols=sym, timeframe="1Min", start=(t - timedelta(minutes=6)).strftime("%Y-%m-%dT%H:%M:%SZ"),
             end=(t + timedelta(minutes=1)).strftime("%Y-%m-%dT%H:%M:%SZ"), limit=50)
    try:
        r = json.load(urllib.request.urlopen(urllib.request.Request("https://data.alpaca.markets/v1beta1/options/bars?" + urllib.parse.urlencode(q), headers=h), timeout=30))
        bars = (r.get("bars") or {}).get(sym) or []
        return bars[-1].get("vw") if bars else None
    except Exception:  # noqa: BLE001
        return None


def main():
    h = alpaca_headers(sys.argv[1]) if len(sys.argv) > 1 else None
    ent = parse()
    trades = [t for t in api("us/spreads")["trades"] if t["status"] in ("OPEN", "CLOSED", "CLOSING")]
    rows = []
    for t in trades:
        k = (t["day"], t["underlying"])
        e = ent.get(k, {})
        r = dict(day=t["day"], fund=t["underlying"], short=t["short_strike"], long=t["long_strike"], status=t["status"],
                 mid=e.get("mid"), natural=e.get("natural"), fill=float(t["entry_credit"]) if t.get("entry_credit") else e.get("fill"))
        if r["mid"] is not None and r["fill"] is not None:
            r["entry_cost_per_leg"] = round((r["mid"] - r["fill"]) / 2, 4)
            span = r["mid"] - (r["natural"] or r["mid"])
            r["entry_share_of_gap"] = round((r["mid"] - r["fill"]) / span, 2) if span > 0 else None
        # Alpaca's own expiry buy-back is booked minutes after its fill, so its
        # exit_at is not the fill minute; compare only our own buy-backs.
        if t.get("exit_debit") and t.get("exit_at") and h and t.get("exit_reason") != "ALPACA_EXPIRY_CLOSE":
            vw = vwap_at(h, occ(t["underlying"], t["day"], t["short_strike"]), t["exit_at"])
            r["exit_paid"] = float(t["exit_debit"])
            r["exit_short_vwap"] = vw
            if vw is not None:
                r["exit_cost_per_leg"] = round(float(t["exit_debit"]) - vw, 4)
        r["pnl"] = t.get("pnl")
        rows.append(r)
    OUT.write_text(json.dumps(rows, indent=1))
    ec = [r["entry_cost_per_leg"] for r in rows if r.get("entry_cost_per_leg") is not None]
    xc = [r["exit_cost_per_leg"] for r in rows if r.get("exit_cost_per_leg") is not None]
    print(f"spreads measured: {len(rows)} (entry {len(ec)}, exit {len(xc)})")
    if ec:
        print(f"entry cost per leg vs the quoted mid: avg {sum(ec)/len(ec)*100:+.1f}c (break-even about +{BREAK_EVEN_PER_LEG*100:.0f}c)")
    if xc:
        print(f"exit cost vs the short call's traded price that minute: avg {sum(xc)/len(xc)*100:+.1f}c")
    for r in rows:
        print(f"  {r['day']} {r['fund']} {r['short']}/{r['long']}: mid {r['mid']} natural {r['natural']} fill {r['fill']}"
              f" -> entry {r.get('entry_cost_per_leg', '-')}/leg; exit paid {r.get('exit_paid', '-')} vs traded {r.get('exit_short_vwap', '-')}; P&L {r['pnl']}")


if __name__ == "__main__":
    main()
