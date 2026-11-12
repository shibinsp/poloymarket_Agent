"""Read-only hourly report of every book. Never prints keys or tokens."""
import json, sys, urllib.request, datetime as dt

A = "http://127.0.0.1:8088/api/"
IST = dt.timezone(dt.timedelta(hours=5, minutes=30))
now = dt.datetime.now(IST)

def get(p):
    try:
        return json.load(urllib.request.urlopen(A + p, timeout=20))
    except Exception as e:
        return {"_error": str(e)[:120]}

def f(x, d=2):
    try:
        return float(x)
    except Exception:
        return 0.0

def alpaca(env_path):
    env = {}
    for line in open(env_path, encoding="utf-8"):
        if "=" in line and not line.lstrip().startswith("#"):
            k, v = line.split("=", 1); env[k.strip()] = v.strip().strip('"').strip("'")
    h = {"APCA-API-KEY-ID": env["ALPACA_API_KEY_ID"], "APCA-API-SECRET-KEY": env["ALPACA_API_SECRET_KEY"]}
    g = lambda p: json.load(urllib.request.urlopen(urllib.request.Request("https://paper-api.alpaca.markets" + p, headers=h), timeout=20))
    return g("/v2/account"), g("/v2/positions"), g("/v2/orders?status=open&limit=50")

out = []
P = out.append
P(f"REPORT {now:%a %d %b %H:%M} IST")

# Agent
h = get("health")
if "_error" in h:
    P(f"AGENT: NOT REACHABLE ({h['_error']})")
else:
    last = h.get("last_cycle_at", "")
    try:
        age = (dt.datetime.now(dt.timezone.utc) - dt.datetime.fromisoformat(last.replace("Z", "+00:00"))).total_seconds() / 60
    except Exception:
        age = -1
    P(f"AGENT: {h.get('agent_state')} halted={h.get('halted')} last cycle {age:.0f} min ago, up {f(h.get('uptime_seconds'))/3600:.1f} h")
try:
    urllib.request.urlopen("http://127.0.0.1:8091/health", timeout=5)
    P("TIMESFM HELPER: running")
except urllib.error.HTTPError:
    P("TIMESFM HELPER: running")
except Exception:
    P("TIMESFM HELPER: DOWN (India ORB trades without its filter)")
try:
    import subprocess
    ps = subprocess.run(["powershell", "-NoProfile", "-Command",
                         "(Get-CimInstance Win32_Process -Filter \"Name like 'python%'\" | Where-Object { $_.CommandLine -match 'stay_awake' } | Measure-Object).Count"],
                        capture_output=True, text=True, timeout=30).stdout.strip()
    P("STAY-AWAKE (screen on): " + ("running" if ps not in ("", "0") else "DOWN - laptop and screen may sleep"))
except Exception:
    P("STAY-AWAKE: could not check")
up = get("upstox/feed/status")
if "_error" not in up:
    P(f"UPSTOX FEED: connected={up.get('connected')}")

# India
ind = get("india/intraday")
if "_error" not in ind:
    P(f"INDIA (3 books, Rs30,000): today Rs{f(ind.get('pnl_today_inr')):+.2f}, total Rs{f(ind.get('pnl_total_inr')):+.2f}, money now Rs{f(ind.get('trading_capital_inr')):,.2f}")
    for b in ind.get("books", []):
        P(f"  {b['strategy']}: today Rs{f(b['pnl_today_inr']):+.2f} ({b['trades_today']} trades), total Rs{f(b['pnl_total_inr']):+.2f}")
    today = now.strftime("%Y-%m-%d")
    for t in ind.get("trades", []):
        if t.get("day") == today:
            if t.get("exit_price"):
                st = f"closed {t.get('exit_reason')} Rs{f(t.get('pnl')):+.2f}"
            else:
                try:
                    u = f"https://query1.finance.yahoo.com/v8/finance/chart/{t['symbol']}.NS?interval=1m&range=1d"
                    px = json.load(urllib.request.urlopen(urllib.request.Request(u, headers={"User-Agent": "Mozilla/5.0"}), timeout=15))["chart"]["result"][0]["meta"]["regularMarketPrice"]
                    sign = 1 if t["side"] == "LONG" else -1
                    st = f"OPEN, now {px} (Yahoo), P&L Rs{sign * (px - f(t['entry_price'])) * f(t['qty']):+.2f}"
                except Exception:
                    st = "OPEN (price unavailable)"
            P(f"  - {t['strategy']} {t['side']} {t['symbol']} x{t['qty']} @ {t['entry_price']} stop {t.get('stop_price')}: {st}")

# US ORB
orb = get("us/orb/trades")
if "_error" not in orb and orb.get("book"):
    b = orb["book"]
    P(f"US ORB ($1,000): today ${f(b.get('pnl_today_usd')):+.2f}, total ${f(b.get('pnl_total_usd')):+.2f}")
    for t in orb.get("trades", []):
        if t.get("status") == "OPEN":
            P(f"  - OPEN {t['symbol']} {t['side']} x{t['qty']} @ {t['entry_price']} now {t.get('mark_price')} P&L ${f(t.get('unrealized_pnl')):+.2f}")

# Spreads
sp = get("us/spreads")
if "_error" not in sp:
    b = sp.get("book", {})
    P(f"US SPREADS: today ${f(b.get('pnl_today_usd')):+.2f} ({b.get('spreads_today')} spreads), total ${f(b.get('pnl_total_usd')):+.2f}")
    for t in sp.get("trades", []):
        if t.get("status") in ("OPEN", "PENDING", "CLOSING"):
            P(f"  - {t['status']} {t['underlying']} {t['short_strike']}/{t['long_strike']} credit {t.get('entry_credit')}")

# IBS
ibs = get("us/ibs")
if "_error" not in ibs:
    P(f"US IBS ($1,000, simulated): total ${f(ibs.get('pnl_total_usd')):+.2f}, open {ibs.get('open')}")
    for t in ibs.get("trades", []):
        if t.get("status") == "OPEN":
            P(f"  - OPEN {t['symbol']} x{t['qty']} @ {t['entry_price']}")

# Turnaround Tuesday
tt = get("us/turnaround")
if "_error" not in tt and "pnl_total_usd" in tt:
    P(f"US TURNAROUND TUESDAY ($1,000 DIA, simulated): total ${f(tt.get('pnl_total_usd')):+.2f}, open {tt.get('open')}")
    for t in tt.get("trades", []):
        if t.get("status") == "OPEN":
            P(f"  - OPEN {t['symbol']} x{t['qty']} @ {t['entry_price']} (Friday {t['friday_close']} < 25-day avg {t['sma25']})")

# Pre-FOMC drift
pf = get("us/prefomc")
if "_error" not in pf and "pnl_total_usd" in pf:
    P(f"US PRE-FED DRIFT ($1,000 SPY, simulated): total ${f(pf.get('pnl_total_usd')):+.2f}, open {pf.get('open')}, next Fed decision {pf.get('next_decision')}")

# Crypto
cr = get("crypto")
if "_error" not in cr:
    main = [b for b in cr.get("books", []) if b["book"] == "crypto"]
    if main:
        b = main[0]
        P(f"CRYPTO ($20): value ${f(b['value_usd']):.2f}, open ${f(b['open_value_usd']):.2f}, unrealised ${f(b['unrealized_usd']):+.2f}")
    if cr.get("checks"):
        c = cr["checks"][0]
        P(f"  last check {c.get('at','')[:16]}Z: {c.get('action')}; 60d {c.get('chg_60d_pct')}%, 7d {c.get('chg_7d_pct')}%")

# Polymarket
pm = get("polymarket/trades")
if "_error" not in pm:
    ts = pm.get("trades", [])
    op = [t for t in ts if not t.get("closed_at")]
    won = [t for t in ts if t.get("closed_at") and f(t.get("pnl")) > 0]
    lost = [t for t in ts if t.get("closed_at") and f(t.get("pnl")) <= 0]
    P(f"POLYMARKET ($50): realised ${f(pm.get('realized_pnl_usd')):+.2f}, cash ${f(pm.get('cash_usd')):.2f}, {len(op)} bets open (${f(pm.get('open_cost_usd')):.2f}), settled {len(won)} won / {len(lost)} lost")

# Spread book fill quality (break-even about 1c a leg)
try:
    import subprocess
    sf = subprocess.run([sys.executable, "-I", "tools/spread_fills.py"], capture_output=True, text=True, timeout=60).stdout.strip().splitlines()
    P("SPREAD FILLS: " + " | ".join(sf[:2]))
except Exception:
    P("SPREAD FILLS: could not check")

# Polymarket live-price shadow test (no bets)
try:
    import subprocess
    alive = subprocess.run(["powershell", "-NoProfile", "-Command",
                            "(Get-CimInstance Win32_Process -Filter \"Name like 'python%'\" | Where-Object { $_.CommandLine -match 'poly_shadow' } | Measure-Object).Count"],
                           capture_output=True, text=True, timeout=30).stdout.strip()
    rep = subprocess.run([sys.executable, "-I", "tools/poly_shadow.py", "report"], capture_output=True, text=True, timeout=60).stdout.strip().splitlines()
    P("POLY SHADOW TEST (no bets): " + ("running" if alive not in ("", "0") else "DOWN") + " | " + " | ".join(rep[:3]))
except Exception:
    P("POLY SHADOW TEST: could not check")

# Alpaca account
if len(sys.argv) > 1:
    try:
        acct, pos, orders = alpaca(sys.argv[1])
        eq, last = f(acct["equity"]), f(acct["last_equity"])
        P(f"ALPACA PAPER: equity ${eq:,.2f}, today ${eq - last:+.2f}; {len(pos)} positions, {len(orders)} open orders")
        for p in pos:
            P(f"  - {p['symbol']} x{p['qty']} entry {p['avg_entry_price']} now {p['current_price']} P&L ${f(p['unrealized_pl']):+.2f}")
    except Exception as e:
        P(f"ALPACA: not reachable ({type(e).__name__})")

# Target
tg = get("target")
if "_error" not in tg and tg.get("days"):
    d = tg["days"][-1]
    hits = sum(1 for x in tg["days"] if x.get("hit"))
    P(f"TARGET Rs500/day: today ({d['day']}) Rs{f(d['total_inr']):+,.0f}; hit {hits} of {len(tg['days'])} days; average Rs{f(tg.get('average_inr')):+,.0f}/day")

print("\n".join(out))
