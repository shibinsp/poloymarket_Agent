# Paper Trading Guide — Test With Fake Money on Real Markets

## How Paper Mode Works

Paper mode uses **real Polymarket market data** but **simulated money**. No wallet, no crypto, no real funds at risk.

```
Real Polymarket Data (public APIs, no auth needed)
        │
        ▼
┌─────────────────────────────┐
│  Gamma API → market list    │  ← Real markets, real prices
│  CLOB API  → order books    │  ← Real bid/ask spreads
└─────────────────────────────┘
        │
        ▼
┌─────────────────────────────┐
│  Claude Sonnet → fair value │  ← Costs ~$0.009/call (real API cost)
│  Kelly Criterion → sizing   │
│  Risk Limits → validation   │
└─────────────────────────────┘
        │
        ▼
┌─────────────────────────────┐
│  Paper Engine (in-memory)   │  ← FAKE $100 balance
│  - Instant fill at limit    │  ← No slippage simulation
│  - Balance tracking         │
│  - Position tracking        │
│  SQLite → trade log         │  ← Persists across restarts
└─────────────────────────────┘
```

Your paper balance starts at **$100** (configurable). Orders fill instantly at the limit price. The only real money spent is on Claude API calls (~$0.009 each).

---

## Step-by-Step Setup

### 1. Fix the Compilation Blocker

Open `Cargo.toml` and change line 4:
```toml
# BEFORE (broken — edition "2024" doesn't exist)
edition = "2024"

# AFTER (correct)
edition = "2021"
```

### 2. Install Rust (if needed)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
rustc --version   # Should show 1.88.0+
```

### 3. Create Your .env File

```bash
cd polymarket-agent
cp .env.example .env
```

Edit `.env` — you only need ONE key for paper mode:

```env
# REQUIRED — this is the only thing that costs real money (~$0.009/call)
ANTHROPIC_API_KEY=sk-ant-your-key-here

# OPTIONAL — leave as placeholders, not needed for paper mode
POLYMARKET_PRIVATE_KEY=0x0000000000000000000000000000000000000000000000000000000000000000
DISCORD_WEBHOOK_URL=
NOAA_API_TOKEN=
ESPN_API_KEY=
RUST_LOG=info
DATABASE_URL=sqlite:polymarket-agent.db
```

### 4. Build and Run Tests

```bash
cargo check          # Compile check (fix any errors first)
cargo test           # Run all unit tests
cargo clippy         # Lint check
```

### 5. Start Paper Trading

```bash
cargo run
```

You'll see output like:

```
INFO  polymarket_agent  Polymarket Agent starting  mode=Paper cycle_interval_s=600
INFO  polymarket_agent  Agent initialized  mode=Paper cycle_number=0 valuation_enabled=true
INFO  polymarket_agent  Starting cycle  cycle=0 state=Alive
INFO  polymarket_agent  Scanned markets  candidates=47
INFO  polymarket_agent  Claude valuation  market="Will BTC hit $100k?" fair_value=0.62 confidence=0.78
INFO  polymarket_agent  Edge found  edge=0.12 side=YES kelly=0.08
INFO  polymarket_agent  Paper order filled  order_id=a1b2c3 balance=94.20
...
```

Press **Ctrl+C** to stop gracefully (positions are saved in SQLite).

---

## What Happens Each 10-Minute Cycle

| Step | What It Does | Cost |
|------|-------------|------|
| 1. Survival Check | Checks paper balance vs thresholds | Free |
| 2. Exit Signals | Fetches live prices for open positions, triggers stop-loss if > 20% loss | Free |
| 3. Resolution | Checks if any markets resolved, settles P&L | Free |
| 4. Budget Check | Verifies daily API spend < $5 cap | Free |
| 5. Market Scan | Queries Gamma API for active markets | Free |
| 6. Filter | Keeps markets with volume > $5k, spread < 5%, resolving within 14 days | Free |
| 7. Valuation | Asks Claude for fair probability estimate | **~$0.009 per market** |
| 8. Edge Check | Compares Claude's estimate vs market price | Free |
| 9. Kelly Sizing | Calculates optimal bet size | Free |
| 10. Paper Trade | Simulates order fill, deducts from paper balance | Free |
| 11. Log Cycle | Writes everything to SQLite | Free |

**Typical cost**: 5-15 markets evaluated per cycle × $0.009 = **$0.05-$0.14 per cycle**. With 6 cycles/hour, that's about **$0.30-$0.84/hour** or **$3-8/day**.

The daily budget cap ($5 default) stops Claude calls when you hit the limit.

---

## Configuration You Can Tweak

Edit `config/default.toml`:

```toml
[agent]
mode = "paper"                    # Keep this as "paper"
cycle_interval_seconds = 600      # How often to run (600 = 10 min)
initial_paper_balance = 100.0     # Starting fake money
daily_api_budget = 5.0            # Max real $ spent on Claude per day

[scanning]
max_markets = 1000                # Max markets to scan
min_volume_24h = 5000.0           # Skip illiquid markets
max_resolution_days = 14          # Only short-term markets
categories = ["weather", "sports", "crypto", "politics"]

[valuation]
claude_model = "claude-sonnet-4-20250514"   # Which Claude model
min_edge_threshold = 0.08         # Minimum edge to trade (8%)

[risk]
kelly_fraction = 0.5              # Half-Kelly (conservative)
max_position_pct = 0.06           # Max 6% of bankroll per trade
max_total_exposure_pct = 0.30     # Max 30% total exposure
max_positions_per_category = 3    # Diversification limit
min_position_usd = 1.0            # Don't bother with < $1 trades
```

**Want to test faster?** Set `cycle_interval_seconds = 60` (1-minute cycles). Warning: burns through API budget faster.

**Want to start with more fake money?** Set `initial_paper_balance = 1000.0`.

---

## Monitoring Your Results

### Web Dashboard

Open `http://127.0.0.1:8080` in your browser while the agent runs. Shows real-time balance, trades, and cycle history.

### SQLite Queries

```bash
# Open the database
sqlite3 polymarket-agent.db

# See all trades
SELECT id, market_question, direction, entry_price, size, status, pnl
FROM trades ORDER BY created_at DESC LIMIT 20;

# Win/loss summary
SELECT
  status,
  COUNT(*) as count,
  ROUND(SUM(CAST(pnl AS REAL)), 2) as total_pnl
FROM trades
WHERE status LIKE 'RESOLVED%'
GROUP BY status;

# Claude's prediction accuracy
SELECT
  COUNT(*) as total_predictions,
  SUM(CASE WHEN forecast_correct = 1 THEN 1 ELSE 0 END) as correct,
  ROUND(100.0 * SUM(CASE WHEN forecast_correct = 1 THEN 1 ELSE 0 END) / COUNT(*), 1) as accuracy_pct
FROM confidence_calibration
WHERE resolved = 1;

# How much real money spent on Claude
SELECT
  date(created_at) as day,
  ROUND(SUM(CAST(cost AS REAL)), 4) as daily_cost
FROM api_costs
GROUP BY date(created_at)
ORDER BY day DESC;

# Current open positions
SELECT market_question, direction, entry_price, size, created_at
FROM trades WHERE status = 'OPEN';
```

### Log Files

```bash
# Run with verbose logging to see everything
RUST_LOG=debug cargo run 2>&1 | tee agent.log

# Search for specific events
grep "Paper order filled" agent.log
grep "Trade settled" agent.log
grep "Edge found" agent.log
grep "budget exhausted" agent.log
```

---

## What to Watch For

| Signal | What It Means | Action |
|--------|--------------|--------|
| accuracy_pct > 60% after 30+ trades | Claude has real edge | Consider more aggressive settings |
| accuracy_pct < 50% after 30+ trades | Claude is worse than random | Stop. Review categories/model |
| total_pnl positive after 50+ resolved | Strategy is profitable on paper | Consider small live test |
| total_pnl negative and declining | Agent is losing fake money | Stop. Review logs. Adjust thresholds |
| "budget exhausted" every day | Evaluating too many markets | Increase budget or narrow categories |
| No trades for many cycles | Edge threshold too high | Lower min_edge_threshold from 0.08 to 0.06 |

---

## Timeline

| Week | What to Do |
|------|-----------|
| Week 1 | Run paper mode. Watch logs. Check dashboard daily. Look for obvious errors. |
| Week 2 | Check calibration table. Is Claude > 55% accurate? Review which categories work best. |
| Week 3 | Tune config based on results. Narrow to best categories. Adjust edge threshold. |
| Week 4 | If consistently profitable on paper: consider $50 live test (requires EIP-712 implementation). |

---

## Important Limitations of Paper Mode

1. **Instant fills** — Real markets have slippage. Paper mode fills at limit price instantly. Real trades might not fill or fill at worse prices.

2. **No market impact** — Paper trades don't move the market. A real $50 order in a thin market would move the price.

3. **Resolution depends on Gamma API** — If Gamma is slow to report market resolution, P&L settlement is delayed.

4. **Claude costs are real** — Even in paper mode, each valuation call costs ~$0.009 in real Anthropic credits. The $5/day budget cap protects you.

5. **No live mode yet** — The `place_limit_order` function returns `bail!()` in live mode. Polymarket SDK wallet signing (EIP-712) is not implemented.
