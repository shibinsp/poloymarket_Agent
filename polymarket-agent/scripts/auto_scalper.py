#!/usr/bin/env python3
"""
Automated US Multi-Stock Micro-Capital Scalper
Trades all affordable US paper stocks (SIRI, NOK, LCID, PLUG, NIO, SOFI)
compounding initial $5.00 capital continuously until US market close (16:00 ET).
"""

import sys
import time
import json
import urllib.request
import urllib.error
from datetime import datetime

# Force UTF-8 on Windows
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")

BASE_URL = "http://localhost:8080"
TARGET_EQUITY_USD = 25.00  # High ceiling to allow continuous compounding until market close

# Diverse universe of all affordable US equities & tech/clean-energy/EV growth stocks
SCALP_UNIVERSE = [
    {"symbol": "SIRI", "name": "Sirius XM", "buy_price": "4.80", "sell_price": "5.20"},   # +$0.40
    {"symbol": "NOK",  "name": "Nokia ADR", "buy_price": "4.25", "sell_price": "4.65"},   # +$0.40
    {"symbol": "LCID", "name": "Lucid Group", "buy_price": "2.50", "sell_price": "2.85"}, # +$0.35
    {"symbol": "PLUG", "name": "Plug Power", "buy_price": "2.20", "sell_price": "2.50"},  # +$0.30
    {"symbol": "NIO",  "name": "NIO Inc", "buy_price": "4.50", "sell_price": "4.90"},     # +$0.40
    {"symbol": "SOFI", "name": "SoFi Tech", "buy_price": "4.90", "sell_price": "5.35"},   # +$0.45
]

def get_json(url):
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "AutoScalper/2.0"})
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except Exception as e:
        print(f"[{datetime.now().strftime('%H:%M:%S')}] GET {url} error: {e}")
        return None

def post_order(symbol, side, qty, price):
    url = f"{BASE_URL}/api/us/paper/order"
    payload = json.dumps({
        "symbol": symbol,
        "side": side,
        "order_type": "LIMIT",
        "quantity": qty,
        "price": price
    }).encode("utf-8")
    try:
        req = urllib.request.Request(
            url,
            data=payload,
            headers={"Content-Type": "application/json", "User-Agent": "AutoScalper/2.0"}
        )
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8")
        print(f"[{datetime.now().strftime('%H:%M:%S')}] Order HTTPError {e.code}: {body}")
        return None
    except Exception as e:
        print(f"[{datetime.now().strftime('%H:%M:%S')}] Order Exception: {e}")
        return None

def run_scalper():
    print("=" * 70)
    print("🚀 MULTI-STOCK US SCALPER RUNNING ACROSS ENTIRE UNIVERSE")
    print("📋 Active Universe: SIRI, NOK, LCID, PLUG, NIO, SOFI")
    print("⏰ Trading continuously until US Market Close (16:00 ET)")
    print("=" * 70)

    trade_count = 0
    cycle_idx = 0

    while True:
        # 1. Check Market Session & Intraday Status
        status = get_json(f"{BASE_URL}/api/intraday/status")
        if status and "us" in status:
            us_session = status["us"].get("session_state", "OPEN")
            local_et = status["us"].get("local_time", "")
            if us_session == "CLOSED":
                print(f"[{datetime.now().strftime('%H:%M:%S')}] 🛑 US Market is CLOSED ({local_et}). Trading paused until next session.")
                time.sleep(60)
                continue

        # 2. Check US Paper Status & Equity
        paper = get_json(f"{BASE_URL}/api/us/paper/status")
        if not paper:
            time.sleep(5)
            continue

        total_equity = float(paper.get("total_equity_usd", 5.0))
        avail_cash = float(paper.get("available_cash_usd", 5.0))
        realized_profit = float(paper.get("realized_profit_usd", 0.0))
        holdings = paper.get("holdings", {})

        held_str = ", ".join([f"{q}x {s}" for s, q in holdings.items() if float(q) > 0]) or "None"
        print(f"[{datetime.now().strftime('%H:%M:%S')}] 📊 Equity: ${total_equity:.2f} | Cash: ${avail_cash:.2f} | Realized Profit: +${realized_profit:.2f} | Held: [{held_str}]")

        # 3. Sell any open positions that hit profit target
        for inst in SCALP_UNIVERSE:
            sym = inst["symbol"]
            held_qty = int(float(holdings.get(sym, 0)))
            if held_qty > 0:
                sell_p = inst["sell_price"]
                print(f"[{datetime.now().strftime('%H:%M:%S')}] 🟠 SELLING {held_qty}x {sym} @ ${sell_p} to lock in profit...")
                res = post_order(sym, "SELL", held_qty, sell_p)
                if res and res.get("status") == "success":
                    trade_count += 1
                    new_profit = float(res.get("realized_profit_usd", realized_profit))
                    new_equity = float(res.get("total_equity_usd", total_equity))
                    print(f"[{datetime.now().strftime('%H:%M:%S')}] 💰 {sym} SOLD! Profit: +${new_profit:.2f} | Equity: ${new_equity:.2f}")
                time.sleep(1)

        # Re-fetch cash after sells
        paper = get_json(f"{BASE_URL}/api/us/paper/status")
        if paper:
            avail_cash = float(paper.get("available_cash_usd", avail_cash))
            holdings = paper.get("holdings", {})

        # 4. Rotate and Buy next affordable stock in universe
        for _ in range(len(SCALP_UNIVERSE)):
            inst = SCALP_UNIVERSE[cycle_idx % len(SCALP_UNIVERSE)]
            cycle_idx += 1
            sym = inst["symbol"]
            buy_p = float(inst["buy_price"])
            held_qty = int(float(holdings.get(sym, 0)))

            if held_qty == 0 and avail_cash >= buy_p:
                print(f"[{datetime.now().strftime('%H:%M:%S')}] 🟢 BUYING 1x {sym} ({inst['name']}) @ ${inst['buy_price']}...")
                res = post_order(sym, "BUY", 1, inst["buy_price"])
                if res and res.get("status") == "success":
                    trade_count += 1
                    print(f"[{datetime.now().strftime('%H:%M:%S')}] ✅ {sym} BUY Order FILLED! Total trades: {trade_count}")
                    avail_cash -= buy_p
                break

        # Pacing between trade cycles
        time.sleep(15)

if __name__ == "__main__":
    run_scalper()
