import requests
import sqlite3
import datetime

import os

def get_env_val(key, default=""):
    val = os.environ.get(key)
    if val:
        return val
    env_file = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".env")
    if os.path.exists(env_file):
        with open(env_file, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith("#") and "=" in line:
                    k, v = line.split("=", 1)
                    if k.strip() == key:
                        return v.strip()
    return default

API_KEY = get_env_val("ALPACA_API_KEY_ID")
API_SECRET = get_env_val("ALPACA_API_SECRET_KEY")
BASE_URL = "https://paper-api.alpaca.markets"

headers = {
    "APCA-API-KEY-ID": API_KEY,
    "APCA-API-SECRET-KEY": API_SECRET,
    "Content-Type": "application/json"
}

print("================================================================")
print("             POLYMARKET AGENT - PROFIT REPORT                  ")
print("================================================================")
now_str = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S")
print(f"Generated at: {now_str}\n")

# 1. Alpaca Account Balance & Equity
try:
    acc = requests.get(f"{BASE_URL}/v2/account", headers=headers).json()
    equity = float(acc.get("equity", 0))
    cash = float(acc.get("cash", 0))
    buying_power = float(acc.get("buying_power", 0))
    last_equity = float(acc.get("last_equity", equity))
    day_pnl = equity - last_equity
    day_pnl_pct = (day_pnl / last_equity * 100) if last_equity > 0 else 0.0

    print("--- ACCOUNT OVERVIEW ---")
    print(f"Total Portfolio Equity : ${equity:,.2f}")
    print(f"Available Cash         : ${cash:,.2f}")
    print(f"Buying Power           : ${buying_power:,.2f}")
    pnl_sign = "+" if day_pnl >= 0 else ""
    print(f"Today's Net P&L        : {pnl_sign}${day_pnl:.2f} ({pnl_sign}{day_pnl_pct:.2f}%)\n")
except Exception as e:
    print(f"Error fetching account info: {e}\n")

# 2. Open Positions & Unrealized Profit
try:
    positions = requests.get(f"{BASE_URL}/v2/positions", headers=headers).json()
    print("--- ACTIVE POSITIONS & UNREALIZED PROFIT ---")
    if positions:
        total_unrealized_pl = 0.0
        print(f"{'Symbol':<10} {'Qty':<14} {'Entry ($)':<12} {'Current ($)':<12} {'Unrealized P&L':<18}")
        print("-" * 66)
        for p in positions:
            sym = p.get("symbol")
            qty = float(p.get("qty", 0))
            entry = float(p.get("avg_entry_price", 0))
            curr = float(p.get("current_price", 0))
            upl = float(p.get("unrealized_pl", 0))
            upl_pct = float(p.get("unrealized_plpc", 0)) * 100
            total_unrealized_pl += upl
            sign = "+" if upl >= 0 else ""
            print(f"{sym:<10} {qty:<14.6f} ${entry:<11.2f} ${curr:<11.2f} {sign}${upl:.2f} ({sign}{upl_pct:.2f}%)")
        print("-" * 66)
        tot_sign = "+" if total_unrealized_pl >= 0 else ""
        print(f"Total Unrealized P&L: {tot_sign}${total_unrealized_pl:.2f}\n")
    else:
        print("No open positions currently filled.\n")
except Exception as e:
    print(f"Error fetching positions: {e}\n")

# 3. Active Take-Profit Orders
try:
    orders = requests.get(f"{BASE_URL}/v2/orders?status=open", headers=headers).json()
    print("--- ACTIVE TAKE-PROFIT & PENDING EXIT ORDERS ---")
    if orders:
        print(f"{'Symbol':<10} {'Side':<6} {'Qty':<10} {'Target Price':<14} {'Class':<10} {'Status':<10}")
        print("-" * 62)
        for o in orders:
            sym = o.get("symbol")
            side = o.get("side").upper()
            qty = o.get("qty")
            lim = f"${float(o.get('limit_price', 0)):.2f}" if o.get("limit_price") else "MARKET"
            oc = o.get("order_class") or "limit"
            st = o.get("status")
            print(f"{sym:<10} {side:<6} {qty:<10} {lim:<14} {oc:<10} {st:<10}")
        print("-" * 62)
    else:
        print("No open resting orders.")
    print()
except Exception as e:
    print(f"Error fetching orders: {e}\n")

# 4. Local SQLite Closed Trades & Realized Profit
try:
    import os
    db_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "polymarket-agent.db")
    con = sqlite3.connect(db_path)
    cur = con.cursor()
    cur.execute("SELECT id, symbol, quantity, entry_price, mark_price, realized_pnl, close_reason, status FROM trades WHERE status = 'CLOSED'")
    closed = cur.fetchall()
    print("--- REALIZED PROFIT FROM CLOSED TRADES ---")
    if closed:
        total_realized = 0.0
        print(f"{'Trade #':<9} {'Symbol':<10} {'Qty':<12} {'Entry ($)':<12} {'Exit ($)':<12} {'Realized PnL':<15}")
        print("-" * 70)
        for row in closed:
            tid, sym, qty, entry, exit_p, rpnl, reason, st = row
            rpnl_val = float(rpnl) if rpnl else 0.0
            total_realized += rpnl_val
            sign = "+" if rpnl_val >= 0 else ""
            exit_str = f"${float(exit_p):.2f}" if exit_p else "-"
            print(f"#{tid:<8} {sym:<10} {qty:<12} ${float(entry):<11.2f} {exit_str:<12} {sign}${rpnl_val:.2f}")
        print("-" * 70)
        print(f"Total Realized Profit: ${total_realized:.2f}\n")
    else:
        print("No trades closed yet (orders are resting active for profit target).\n")
    con.close()
except Exception as e:
    print(f"Error reading database: {e}\n")

print("================================================================")
print("Web Dashboard live anytime: http://127.0.0.1:8080")
print("================================================================")
