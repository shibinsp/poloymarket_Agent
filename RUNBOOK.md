# MoneyAI Autonomous Trading Agent — Operator Runbook

A complete reference guide for running, configuring, monitoring, and operating MoneyAI across all supported trading venues (Prediction Markets, US Equities/ETFs, and Indian NSE Equities).

---

## 1. Quick Start & Dashboard

### Accessing the Web UI
- **URL**: [http://127.0.0.1:8088](http://127.0.0.1:8088)
- **Direct Navigation Hash Routes**:
  - `/#/overview`: Executive portfolio balance, unrealized PnL, circuit breaker status, and recent activity.
  - `/#/india`: Indian NSE Cash Equities sandbox, live tick charts, and interactive backtesting.
  - `/#/us`: US Equities & ETFs (Alpaca Paper), live bar charts, and historical backtests.
  - `/#/learn`: Learn From Mistakes post-mortem analysis, rule violations, and counterfactuals.
  - `/#/trades`: Historical executed trades and outcomes.
  - `/#/orders`: Open, acknowledged, and filled orders across venues.
  - `/#/risk`: Circuit breaker limits, drawdown thresholds, and reconciliation details.
  - `/#/settings`: UI themes (Dark/Light), auto-refresh intervals, and price decimal precision.

---

## 2. Starting & Stopping the Application

### On Windows
| Action | Command / File | Description |
| :--- | :--- | :--- |
| **Quick Start** | Double-click `launch.bat` | Starts Paper Trading mode, turns on Cloudflare remote tunnel, and opens dashboard. |
| **Full Menu** | Double-click `start.bat` | Terminal menu (Paper, Background, Live, Status) with remote hosting always on. |
| **Background Mode** | Double-click `start_background.bat` | Runs silently as a detached process with remote hosting active. |
| **Stop Agent** | Double-click `stop_background.bat` | Gracefully terminates both `polymarket-agent.exe` and `cloudflared.exe`. |
| **Auto-Start on Boot** | Run `install_autostart.bat` | Configures Windows Startup to launch agent and remote hosting automatically on login. |
| **Remote Tunnel Only** | Double-click `start_public_tunnel.bat` | Starts/checks the Cloudflare remote tunnel and displays the public link. |

### On macOS / Linux
```bash
# Move to agent directory
cd polymarket-agent

# Start in foreground (Paper Trading)
cargo run --release -- --mode paper

# Or run in background with logging
RUST_LOG=info nohup cargo run --release -- --mode paper > /tmp/moneyai.log 2>&1 &

# Stop running process
pkill -f polymarket-agent
```

---

## 3. Controlling Agent State (Halt & Resume)

MoneyAI has safety circuit breakers that halt new order entries while keeping exit management and reconciliation running.

### Via Web Dashboard
1. Go to `/#/overview` or `/#/risk`.
2. Look at the top **Trading / Halted** status card:
   - **Halt Trading**: Halts new order generation immediately (useful during high-volatility news events).
   - **Resume Trading**: Clears the halt and acknowledges the current market/reconciliation state.

### Via Terminal / Curl (with Required Security Header)
```bash
# Resume trading / clear circuit breaker halt
curl -X POST http://127.0.0.1:8088/api/resume \
  -H "Content-Type: application/json" \
  -H "x-agent-control: 1"

# Emergency Halt
curl -X POST http://127.0.0.1:8088/api/halt \
  -H "Content-Type: application/json" \
  -H "x-agent-control: 1"
```

---

## 4. Configuration Guide

All configuration is located in `polymarket-agent/config/local.toml` (which overrides `default.toml`).

### A. Capital Limits & Risk Management
```toml
[risk]
# Maximum percentage allocated to a single trade
max_position_pct = 0.06

# Maximum total portfolio exposure
max_total_exposure_pct = 0.30

# Maximum drawdown percentage before halting (e.g. 0.15 = 15%)
max_drawdown_pct = 0.15

# Indian NSE Equities daily capital cap
india_capital_allocation_inr = 5000.0   # Set your intraday limit in INR (e.g. ₹5,000)

# US Equities daily capital cap
us_capital_allocation_usd = 5.0        # Set your limit in USD
```

### B. Cycle Timing & LLM Reasoning Engine
```toml
[agent]
mode = "paper"                    # "paper" | "live" | "backtest"
cycle_interval_seconds = 60       # Frequency of market scans (60s default)

[valuation]
provider = "openai_compatible"
model = "deepseek-ai/deepseek-v4-flash-0731"   # Or "minimaxai/minimax-m3.1-flash"
base_url = "https://integrate.api.nvidia.com/v1"
max_tokens = 4096
min_edge_threshold = 0.005        # Required edge before an entry is considered
```

### C. Active Watchlists
Under `[[venues]]`:
- **Upstox Sandbox (`upstox_sandbox`)**:
  - `symbols = ["RELIANCE", "TCS", "INFY", "HDFCBANK", "ICICIBANK"]`
- **Alpaca Paper (`alpaca`)**:
  - `symbols = ["SPY", "AAPL", "NVDA", "QQQ", "BTC/USD", "ETH/USD"]`

---

## 5. Indian Market (NSE) Operations

- **Session Timing**: 09:15 to 15:30 IST (Asia/Kolkata).
- **Intraday Transaction Fee Structure**:
  - Brokerage: 0.05% (capped at ₹20 per executed order).
  - Securities Transaction Tax (STT): 0.025% on intraday Sell orders (nil on Buy).
  - Exchange Turnover Charges: 0.00297% (NSE).
  - SEBI Turnover: ₹10 / crore.
  - Stamp Duty: 0.015% on Buy orders.
  - GST: 18% on (Brokerage + Exchange + SEBI fees).
- **Placing a Test Sandbox Order**:
```bash
curl -X POST "http://127.0.0.1:8080/api/india/sandbox/order" \
  -H "Content-Type: application/json" \
  -H "x-agent-control: 1" \
  -d '{"symbol":"RELIANCE","side":"BUY","quantity":1,"order_type":"MARKET","price":null}'
```

---

## 6. "Learn From Mistakes" Post-Mortem System

Navigate to **[http://127.0.0.1:8080/#/learn](http://127.0.0.1:8080/#/learn)** to inspect:
1. **Systemic Pattern Detection**:
   - Flags trades where entry occurred against prevailing multi-hour momentum.
   - Highlights sizing anomalies where notional exposure exceeded risk parameters.
2. **Post-Mortem Trade Logs**:
   - Lists every losing trade with an automated **Root Cause Analysis** and **Counterfactual Remedy**.
3. **Guardrails**:
   - Technical filters enforcing `SMA-5 > SMA-14` trend verification and `RSI` threshold bounds before entry.
   - Tight 2% take-profit and dynamic ATR-based trailing stop losses.

---

## 7. Troubleshooting & Common Scenarios

| Issue | Cause | Solution |
| :--- | :--- | :--- |
| **Dashboard shows "HALTED"** | Circuit breaker tripped (drawdown threshold) or venue mismatch. | Go to `/#/overview`, read the halt detail, and click **Resume Trading** (or issue POST `/api/resume`). |
| **Port 8080 Conflict** | Another process (e.g. Docker) is bound to 8080. | Change `dashboard_port = 8088` in `polymarket-agent/config/local.toml` and restart. |
| **Upstox API Token Expired** | Sandbox access token expired. | Generate a fresh token in the Upstox Developer Console and update `UPSTOX_SANDBOX_TOKEN` in `polymarket-agent/.env`. |
| **Orders not executing** | Venue market is outside trading hours. | NSE trades 09:15–15:30 IST; US Equities trade 09:30–16:00 ET. Check `/#/overview` market session status. |
