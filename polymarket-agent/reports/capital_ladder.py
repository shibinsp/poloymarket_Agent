"""Capital ladder: what each book would have made with a different starting amount.

Replays the week's real paper trades (same stocks, same entry and exit prices)
at each capital level, using the whole amount:
  India  - whole shares only, Upstox intraday charges (same formula as
           src/india/orb.rs mis_charges), losses shrink the cash, profits are
           not added back (same as the live books).
  US ORB - fractional shares, the amount split equally across that night's trades.
  US options - whole contracts (1 contract = 100 shares) that the amount can pay for.
  US spreads - whole spreads; each one ties up (width - credit) x 100 of cash.
Each strategy gets the full amount on its own; "Whole market" splits the amount
equally across that market's live books.

Read-only: opens the agent DB in read-only mode, places nothing.
Usage: python -I capital_ladder.py 2026-11-09 2026-11-13 [out.html]
"""
import json, math, sqlite3, sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
DB = HERE.parent / "polymarket-agent.db"
INDIA = [500, 1000, 2500, 5000, 7500, 10000, 15000, 20000]
US = [10, 15, 20, 25, 50, 100, 250, 500, 750, 1000, 1500, 2000]
USD_INR = 86.5
INDIA_LIVE = {"india_orb": "Opening range breakout", "india_gapgo": "Gap-and-go", "india_burst": "Opening burst"}
INDIA_SHADOW = {"india_orb_tfm_skip": "ORB trades TimesFM blocked", "india_burst_tfm_skip": "Burst trades TimesFM blocked",
                "india_orb_track": "ORB tracking copy"}


def mis_charges(value, buy):
    brokerage = min(value * 0.0005, 20.0)
    exchange = value * 0.0000297
    sebi = value * 0.000001
    gst = (brokerage + exchange + sebi) * 0.18
    tax = value * (0.00003 if buy else 0.00025)
    return round(brokerage + exchange + sebi + gst + tax, 2)


def whole_shares(cash, price, buy):
    if price <= 0 or cash <= 0:
        return 0
    q = math.floor(cash / price)
    while q > 0 and q * price + mis_charges(q * price, buy) > cash:
        q -= 1
    return q


def ny_day(ts):
    t = datetime.fromisoformat(ts.replace("Z", "+00:00").replace(" ", "T"))
    if t.tzinfo is None:
        t = t.replace(tzinfo=timezone.utc)
    # US standard time (UTC-5) from 1 Nov 2026 to 8 Mar 2027.
    return (t - timedelta(hours=5)).date().isoformat()


def load(start, end):
    c = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    c.row_factory = sqlite3.Row
    india = [dict(r) for r in c.execute(
        "SELECT * FROM india_intraday_trades WHERE day BETWEEN ? AND ? AND exit_price IS NOT NULL ORDER BY entry_at", (start, end))]
    orb = [dict(r) for r in c.execute(
        "SELECT * FROM trades WHERE venue_id='alpaca' AND strategy='orb' AND status='CLOSED' AND realized_pnl IS NOT NULL "
        "AND COALESCE(close_reason,'') NOT LIKE 'Failed%' ORDER BY created_at")]
    orb = [r for r in orb if start <= ny_day(r["created_at"]) <= end]
    opts = [dict(r) for r in c.execute(
        "SELECT * FROM us_option_trades WHERE day BETWEEN ? AND ? AND status='CLOSED' AND entry_price IS NOT NULL ORDER BY id", (start, end))]
    spreads = [dict(r) for r in c.execute(
        "SELECT * FROM us_spread_trades WHERE day BETWEEN ? AND ? AND status='CLOSED' AND entry_credit IS NOT NULL ORDER BY id", (start, end))]
    return india, orb, opts, spreads


def india_book(trades, capital):
    """(pnl, trades taken, trades too big)"""
    realized, took, skipped, rows = 0.0, 0, 0, []
    for t in trades:
        buy = bool(t["is_buy"]); e, x = float(t["entry_price"]), float(t["exit_price"])
        cash = min(capital + realized, capital)
        q = whole_shares(cash, e, buy)
        if q == 0:
            skipped += 1; rows.append((t, 0, 0.0)); continue
        gross = (x - e) * q * (1 if buy else -1)
        pnl = gross - mis_charges(q * e, buy) - mis_charges(q * x, not buy)
        realized += pnl; took += 1; rows.append((t, q, pnl))
    return realized, took, skipped, rows


def orb_book(trades, capital):
    by_night = defaultdict(list)
    for t in trades:
        by_night[ny_day(t["created_at"])].append(t)
    realized, took, rows = 0.0, 0, []
    for night in sorted(by_night):
        ts = by_night[night]
        cash = min(capital + realized, capital) / len(ts)
        day_pnl = 0.0
        for t in ts:
            notional = float(t["quantity"]) * float(t["avg_fill_price"])
            ret = float(t["realized_pnl"]) / notional if notional else 0.0
            pnl = cash * ret if cash >= 1 else 0.0  # Alpaca minimum order is $1
            day_pnl += pnl; took += cash >= 1; rows.append((t, cash, pnl))
        realized += day_pnl
    return realized, took, 0, rows


def options_book(trades, capital):
    realized, took, skipped, rows = 0.0, 0, 0, []
    for t in trades:
        e, x = float(t["entry_price"]), float(t["exit_price"])
        cash = min(capital + realized, capital)
        n = math.floor(cash / (e * 100))
        if n == 0:
            skipped += 1; rows.append((t, 0, 0.0)); continue
        pnl = (x - e) * 100 * n
        realized += pnl; took += 1; rows.append((t, n, pnl))
    return realized, took, skipped, rows


def spread_book(trades, capital):
    by_day = defaultdict(list)
    for t in trades:
        by_day[t["day"]].append(t)
    realized, took, skipped, rows = 0.0, 0, 0, []
    for day in sorted(by_day):
        ts = by_day[day]
        cash = min(capital + realized, capital)
        need = [(float(t["long_strike"]) - float(t["short_strike"]) - float(t["entry_credit"])) * 100 for t in ts]
        n = [0] * len(ts)
        # hand out spreads one at a time, SPY then QQQ, while the cash covers them
        while True:
            added = False
            for i in range(len(ts)):
                if need[i] <= cash:
                    n[i] += 1; cash -= need[i]; added = True
            if not added:
                break
        for t, k in zip(ts, n):
            pnl = float(t["pnl"]) * k  # stored pnl is per spread, in dollars
            realized += pnl; took += k > 0; skipped += k == 0; rows.append((t, k, pnl))
    return realized, took, skipped, rows


def money(v, cur):
    s = f"{abs(v):,.2f}"
    return ("+" if v > 0 else "−" if v < 0 else "") + cur + s


def cell(v, cur, cap, note=""):
    cls = "pos" if v > 0.005 else "neg" if v < -0.005 else ""
    pct = f"<small>{v / cap * 100:+.1f}%</small>" if cap else ""
    return f'<td class="n {cls}">{money(v, cur)} {pct}{note}</td>'


def build(start, end):
    india, orb, opts, spreads = load(start, end)
    ib = defaultdict(list)
    for t in india:
        ib[t["strategy"]].append(t)
    live = [s for s in INDIA_LIVE]
    us_books = [("Opening range breakout (stocks)", orb, orb_book), ("Options on breakouts", opts, options_book),
                ("Credit spreads (SPY/QQQ)", spreads, spread_book)]
    data = {"start": start, "end": end, "india": {}, "us": {}}

    rows_html = []
    for cap in INDIA:
        cells, tot_split = [], 0.0
        for s in live:
            p, took, sk, _ = india_book(ib.get(s, []), cap)
            p_split, *_ = india_book(ib.get(s, []), cap / len(live))
            tot_split += p_split
            note = f"<br><small>{took} taken" + (f", {sk} too costly" if sk else "") + "</small>"
            cells.append(cell(p, "₹", cap, note))
            data["india"].setdefault(str(cap), {})[s] = round(p, 2)
        data["india"][str(cap)]["whole_market_split"] = round(tot_split, 2)
        rows_html.append(f'<tr><td class="n"><b>₹{cap:,}</b></td>{"".join(cells)}{cell(tot_split, "₹", cap)}</tr>')
    india_tbl = ("<tr><th>Starting amount</th>" + "".join(f"<th>{INDIA_LIVE[s]}</th>" for s in live) +
                 "<th>Whole market (amount split across the 3)</th></tr>" + "".join(rows_html))

    rows_html = []
    for cap in US:
        cells, tot_split = [], 0.0
        for name, tr, fn in us_books:
            p, took, sk, _ = fn(tr, cap)
            p_split, *_ = fn(tr, cap / len(us_books))
            tot_split += p_split
            note = f"<br><small>{took} taken" + (f", {sk} too costly" if sk else "") + "</small>"
            cells.append(cell(p, "$", cap, note))
            data["us"].setdefault(str(cap), {})[name] = round(p, 2)
        data["us"][str(cap)]["whole_market_split"] = round(tot_split, 2)
        rows_html.append(f'<tr><td class="n"><b>${cap:,}</b></td>{"".join(cells)}{cell(tot_split, "$", cap)}'
                         f'<td class="n">≈ ₹{tot_split * USD_INR:,.0f}</td></tr>')
    us_tbl = ("<tr><th>Starting amount</th>" + "".join(f"<th>{n}</th>" for n, *_ in us_books) +
              "<th>Whole market (split across the 3)</th><th>In ₹</th></tr>" + "".join(rows_html))

    # the real trades behind the numbers
    tl = []
    for t in india:
        if t["strategy"] in INDIA_LIVE:
            tl.append(f'<tr><td>{t["day"]}</td><td>India · {INDIA_LIVE[t["strategy"]]}</td><td>{t["symbol"]} {"buy" if t["is_buy"] else "sell first"}</td>'
                      f'<td class="n">₹{float(t["entry_price"]):,.2f} → ₹{float(t["exit_price"]):,.2f}</td><td>{t["exit_reason"]}</td></tr>')
    for t in orb:
        e = float(t["avg_fill_price"]); r = float(t["realized_pnl"]) / (float(t["quantity"]) * e)
        tl.append(f'<tr><td>{ny_day(t["created_at"])}</td><td>US · breakout</td><td>{t["symbol"]} {"buy" if t["side"] == "BUY" else "sell first"}</td>'
                  f'<td class="n">${e:,.2f} ({r * 100:+.2f}%)</td><td>{t["close_reason"]}</td></tr>')
    for t in opts:
        tl.append(f'<tr><td>{t["day"]}</td><td>US · option</td><td>{t["contract"]}</td>'
                  f'<td class="n">${float(t["entry_price"]):.2f} → ${float(t["exit_price"]):.2f}</td><td>{t["exit_reason"]}</td></tr>')
    for t in spreads:
        tl.append(f'<tr><td>{t["day"]}</td><td>US · spread</td><td>{t["underlying"]} {t["short_strike"]}/{t["long_strike"]}</td>'
                  f'<td class="n">credit ${float(t["entry_credit"]):.2f}, paid back ${float(t["exit_debit"]):.2f}</td><td>{t["exit_reason"]}</td></tr>')
    shadow = {s: len(ib.get(s, [])) for s in INDIA_SHADOW}
    return india_tbl, us_tbl, "".join(tl), data, len(india), len(orb) + len(opts) + len(spreads), shadow


PAGE = """<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Capital Ladder Report</title>
<style>
:root{--bg:#f6f7f9;--paper:#fff;--ink:#1b2330;--muted:#5b6676;--line:#d9dee6;--accent:#1f5f8b;--soft:#e5eef6;--good:#1d7a46;--bad:#a3302a}
@media (prefers-color-scheme:dark){:root:not([data-theme="light"]){--bg:#11161d;--paper:#18202a;--ink:#e6ebf2;--muted:#9aa6b6;--line:#2c3745;--accent:#79b4e0;--soft:#1d2d3d;--good:#6fd39a;--bad:#f08a83;color-scheme:dark}}
:root[data-theme="dark"]{--bg:#11161d;--paper:#18202a;--ink:#e6ebf2;--muted:#9aa6b6;--line:#2c3745;--accent:#79b4e0;--soft:#1d2d3d;--good:#6fd39a;--bad:#f08a83;color-scheme:dark}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:16px/1.6 "Segoe UI",system-ui,sans-serif}
.wrap{max-width:980px;margin:0 auto;padding:36px 16px 72px;display:flex;flex-direction:column;gap:36px}
h1,h2{font-family:Georgia,serif;margin:0;line-height:1.2}h2{border-bottom:2px solid var(--ink);padding-bottom:6px}
p{margin:0;max-width:72ch}section{display:flex;flex-direction:column;gap:14px}
.note{background:var(--soft);border-left:4px solid var(--accent);padding:12px 16px;border-radius:4px}
.tbl{overflow-x:auto;background:var(--paper);border:1px solid var(--line);border-radius:6px}
table{border-collapse:collapse;width:100%;font-size:.92rem}th,td{padding:9px 11px;border-bottom:1px solid var(--line);text-align:left;vertical-align:top}
th{font-size:.74rem;text-transform:uppercase;letter-spacing:.05em;color:var(--muted);background:var(--bg)}
td.n{font-family:Consolas,ui-monospace,monospace;white-space:nowrap}small{color:var(--muted);font-family:"Segoe UI",sans-serif}
.pos{color:var(--good)}.neg{color:var(--bad)}.muted{color:var(--muted);font-size:.92rem}
@media print{body{background:#fff}.tbl,tr,.note{break-inside:avoid}}
</style></head><body><div class="wrap">
<header><p class="muted">Paper trading · week of {start} to {end}</p><h1>Capital Ladder Report</h1>
<p>What each strategy would have made this week if it had started with a different amount of money and used all of it. Same trades, same prices. Only the size changes.</p></header>
<div class="note"><b>How to read it:</b> each cell is the week's profit or loss for that starting amount, with the % return underneath. "Too costly" means the amount couldn't buy even one share or contract, so that trade was skipped. Practice money only.</div>
<section><h2>India (Upstox, whole shares, real charges)</h2><div class="tbl"><table>{india}</table></div>
<p class="muted">Charges are Upstox intraday fees (brokerage up to ₹20 an order, STT, exchange, GST, stamp). On small amounts they eat a much bigger share of the profit. {shadow}</p></section>
<section><h2>US (Alpaca)</h2><div class="tbl"><table>{us}</table></div>
<p class="muted">Breakout stocks can be bought in fractions, so even $10 works (the amount is split across that night's trades). One option contract covers 100 shares, so small amounts usually can't buy one. One credit spread ties up about $150–$175, so spreads need at least that much. ₹ at $1 = ₹{usd_inr}.</p></section>
<section><h2>The real trades behind these numbers</h2><div class="tbl"><table><tr><th>Day</th><th>Book</th><th>Trade</th><th>Price</th><th>How it ended</th></tr>{trades}</table></div></section>
<p class="muted">Made from the agent's own trade records ({n_india} India, {n_us} US trades) on {made}. Assumptions: the same fill prices at every size (fair at these small sizes); India losses shrink the cash and profits are not added back, like the live books.</p>
</div></body></html>"""


if __name__ == "__main__":
    start, end = sys.argv[1], sys.argv[2]
    out = Path(sys.argv[3]) if len(sys.argv) > 3 else HERE / f"capital-ladder-{start}-to-{end}.html"
    india_tbl, us_tbl, trades, data, ni, nu, shadow = build(start, end)
    sh = ", ".join(f"{INDIA_SHADOW[k]}: {v}" for k, v in shadow.items() if v)
    sh = f"Not counted (comparison ledgers only): {sh}." if sh else ""
    html = (PAGE.replace("{start}", start).replace("{end}", end).replace("{india}", india_tbl).replace("{us}", us_tbl)
            .replace("{trades}", trades or '<tr><td colspan="5">No closed trades in this week.</td></tr>')
            .replace("{shadow}", sh).replace("{usd_inr}", str(USD_INR)).replace("{n_india}", str(ni)).replace("{n_us}", str(nu))
            .replace("{made}", datetime.now().strftime("%d %b %Y %H:%M")))
    out.write_text(html, encoding="utf-8")
    out.with_suffix(".json").write_text(json.dumps(data, indent=1), encoding="utf-8")
    print("wrote", out)
    for cap, v in data["india"].items():
        print("IN", cap, v)
    for cap, v in data["us"].items():
        print("US", cap, v)
