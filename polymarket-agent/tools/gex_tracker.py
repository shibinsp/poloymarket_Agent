"""Daily gamma-exposure (GEX) reading for QQQ and SPY, read-only research.

Started 4 Nov 2026 at the user's request: GEX is said to tell, before the
open, whether the day is likely to pin and chop (dealers long gamma, net GEX
positive) or trend and squeeze (net GEX negative). The US ORB makes its money
on trend days, so the question is whether negative-GEX days are its good days.
Free data has no history of open interest, so this can only be tested going
forward: run it before each US open, then compare with the day's ORB results.

Method (the common public "naive" convention: dealers long calls, short puts):
  per contract  gamma x open interest x 100 x spot^2 x 1%  (dollars per 1% move)
                calls +, puts -; contracts expiring within 30 days, strikes
                within 15% of spot
  net GEX       the sum, in $ billions
  call wall     strike with the largest call GEX;  put wall: largest put GEX
  gamma flip    the spot level where net GEX changes sign, re-pricing each
                contract's gamma with Black-Scholes at its implied volatility
Data: Alpaca contracts (open interest, as of the previous close) and the free
indicative options snapshots (greeks, IV). Keys from the agent's .env, never
printed. Appends one line per symbol to research/gex_log.jsonl. No orders.
Usage: python -I tools/gex_tracker.py
"""
import json, math, sys, time, urllib.parse, urllib.request
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ENV = {}
for line in (ROOT / ".env").read_text(encoding="utf-8").splitlines():
    if "=" in line and not line.lstrip().startswith("#"):
        k, v = line.split("=", 1)
        ENV[k.strip()] = v.strip().strip('"').strip("'")
HDR = {"APCA-API-KEY-ID": ENV.get("ALPACA_API_KEY_ID", ""), "APCA-API-SECRET-KEY": ENV.get("ALPACA_API_SECRET_KEY", ""),
       "User-Agent": "paper-research"}
TRADING = "https://paper-api.alpaca.markets"
DATA = "https://data.alpaca.markets"
OUT = ROOT / "research" / "gex_log.jsonl"
DAYS, BAND, RATE = 30, 0.15, 0.04


def get(url):
    for attempt in range(4):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=HDR), timeout=30) as r:
                return json.load(r)
        except Exception as e:  # noqa: BLE001
            if attempt == 3:
                raise RuntimeError(f"request failed: {url.split('?')[0]} ({type(e).__name__})") from None
            time.sleep(2 * (attempt + 1))


def paged(url, key):
    out, token = [], None
    while True:
        page = get(url + (f"&page_token={token}" if token else ""))
        items = page.get(key) or {}
        out.extend(items.items() if isinstance(items, dict) else items)
        token = page.get("next_page_token")
        if not token:
            return out


def gamma_bs(s, k, t, iv):
    if s <= 0 or k <= 0 or t <= 0 or iv <= 0:
        return 0.0
    d1 = (math.log(s / k) + (RATE + iv * iv / 2) * t) / (iv * math.sqrt(t))
    return math.exp(-d1 * d1 / 2) / math.sqrt(2 * math.pi) / (s * iv * math.sqrt(t))


def reading(sym, today):
    spot = get(f"{DATA}/v2/stocks/{sym}/trades/latest?feed=iex")["trade"]["p"]
    lo, hi, until = spot * (1 - BAND), spot * (1 + BAND), today + timedelta(days=DAYS)
    q = urllib.parse.urlencode(dict(underlying_symbols=sym, status="active", expiration_date_gte=today.isoformat(),
                                    expiration_date_lte=until.isoformat(), strike_price_gte=f"{lo:.2f}",
                                    strike_price_lte=f"{hi:.2f}", limit=10000))
    contracts = {c["symbol"]: c for c in paged(f"{TRADING}/v2/options/contracts?{q}", "option_contracts")}
    q2 = urllib.parse.urlencode(dict(feed="indicative", limit=1000, expiration_date_lte=until.isoformat(),
                                     strike_price_gte=f"{lo:.2f}", strike_price_lte=f"{hi:.2f}"))
    snaps = dict(paged(f"{DATA}/v1beta1/options/snapshots/{sym}?{q2}", "snapshots"))
    legs = []
    for s, c in contracts.items():
        oi = float(c.get("open_interest") or 0)
        sn = snaps.get(s) or {}
        iv = float(sn.get("impliedVolatility") or 0)
        g = float((sn.get("greeks") or {}).get("gamma") or 0)
        if oi <= 0 or (g <= 0 and iv <= 0):
            continue
        exp = date.fromisoformat(c["expiration_date"])
        t = max((exp - today).days, 0.5) / 365
        legs.append(dict(k=float(c["strike_price"]), call=c["type"] == "call", oi=oi, iv=iv, g=g, t=t))
    if not legs:
        raise RuntimeError(f"{sym}: no contracts with open interest and greeks")

    def net(s, use_snapshot=False):
        tot = 0.0
        for leg in legs:
            gm = leg["g"] if use_snapshot and leg["g"] > 0 else gamma_bs(s, leg["k"], leg["t"], leg["iv"])
            tot += (1 if leg["call"] else -1) * gm * leg["oi"] * 100 * s * s * 0.01
        return tot

    by_strike = {}
    for leg in legs:
        gm = leg["g"] if leg["g"] > 0 else gamma_bs(spot, leg["k"], leg["t"], leg["iv"])
        v = gm * leg["oi"] * 100 * spot * spot * 0.01
        cw, pw = by_strike.get(leg["k"], (0.0, 0.0))
        by_strike[leg["k"]] = (cw + v, pw) if leg["call"] else (cw, pw + v)
    call_wall = max(by_strike, key=lambda k: by_strike[k][0])
    put_wall = max(by_strike, key=lambda k: by_strike[k][1])
    net_spot = net(spot, use_snapshot=True)
    # gamma flip: scan +/-10% of spot for the sign change nearest to spot
    grid = [spot * (0.90 + i * 0.0025) for i in range(81)]
    vals = [net(s) for s in grid]
    flips = [(grid[i] + grid[i + 1]) / 2 for i in range(len(grid) - 1) if (vals[i] > 0) != (vals[i + 1] > 0)]
    flip = min(flips, key=lambda f: abs(f - spot)) if flips else None
    return dict(symbol=sym, spot=round(spot, 2), net_gex_bn=round(net_spot / 1e9, 3),
                regime="positive: pin / chop more likely" if net_spot > 0 else "negative: trend / squeeze more likely",
                call_wall=call_wall, put_wall=put_wall, gamma_flip=round(flip, 2) if flip else None,
                above_flip=(spot > flip) if flip else None, contracts=len(legs),
                oi_date=max((c.get("open_interest_date") or "") for c in contracts.values()))


if __name__ == "__main__":
    now = datetime.now(timezone.utc)
    today = (now - timedelta(hours=5)).date()  # New York date (EST)
    OUT.parent.mkdir(exist_ok=True)
    for sym in ("QQQ", "SPY"):
        try:
            r = reading(sym, today)
        except Exception as e:  # noqa: BLE001
            print(f"{sym}: {e}")
            continue
        r.update(at=now.isoformat(timespec="seconds"), ny_date=today.isoformat())
        with OUT.open("a", encoding="utf-8") as f:
            f.write(json.dumps(r) + "\n")
        print(f"{sym} {r['spot']}: net GEX {r['net_gex_bn']:+.2f} $bn per 1% ({r['regime']}); "
              f"call wall {r['call_wall']}, put wall {r['put_wall']}, gamma flip {r['gamma_flip']} "
              f"({r['contracts']} contracts, OI as of {r['oi_date']})")
