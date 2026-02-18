# Polymarket Agent — Testing Guide & Profitability Reality Check

## Part 1: How to Test (Step by Step)

### Prerequisites

```bash
# 1. Install Rust (if not already)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# 2. Navigate to project
cd polymarket-agent

# 3. Create your .env file from the example
cp .env.example .env
```

### Step A: Compile & Run Tests (No API Keys Needed)

```bash
# Compile — catches type errors, missing imports, etc.
cargo check

# Run all unit + integration tests (uses in-memory SQLite, no network calls)
cargo test

# Lint for idiomatic Rust issues
cargo clippy -- -W clippy::all
```

**Expected**: All tests pass. If `cargo check` fails, there may be minor import issues
to fix — the code was written without a local compiler. Fix any errors `rustc` reports.

### Step B: Paper Trading (Needs Only Anthropic API Key)

Paper mode simulates trades against **real market data** but uses fake money.
No Polymarket wallet or private key needed.

```bash
# In your .env file, set ONLY this:
ANTHROPIC_API_KEY=sk-ant-your-real-key-here

# Everything else can stay as placeholders or be empty

# Run in paper mode (this is the default)
cargo run
```

**What happens each cycle (every 10 minutes):**
1. Scans Polymarket for active markets via Gamma API (public, no auth needed)
2. Filters markets by volume, spread, category, resolution date
3. Fetches order books from CLOB API (public, no auth needed)
4. Asks Claude Sonnet to estimate fair probability (~$0.009 per call)
5. Computes edge = |fair_value - market_price|
6. Runs Kelly criterion to size the position
7. Simulates the trade in memory (paper balance starts at $100)
8. Logs everything to SQLite + console
9. Checks for resolved markets and settles P&L
10. Dashboard available at http://127.0.0.1:8080

**Cost to run paper mode**: ~$0.009 per market evaluated. With the default
$5/day budget cap, that's ~550 evaluations max per day.

### Step C: Monitor Paper Performance

```bash
# View the SQLite database
sqlite3 polymarket-agent.db

# Check your trades
SELECT * FROM trades ORDER BY created_at DESC LIMIT 20;

# Check P&L
SELECT status, COUNT(*), SUM(CAST(pnl AS REAL)) as total_pnl
FROM trades WHERE status LIKE 'RESOLVED%' GROUP BY status;

# Check calibration (Claude accuracy)
SELECT COUNT(*) as total,
       SUM(CASE WHEN forecast_correct = 1 THEN 1 ELSE 0 END) as correct,
       ROUND(100.0 * SUM(CASE WHEN forecast_correct = 1 THEN 1 ELSE 0 END) / COUNT(*), 1) as accuracy_pct
FROM confidence_calibration WHERE resolved = 1;

# Check daily API costs
SELECT date(created_at) as day, SUM(CAST(cost AS REAL)) as daily_cost
FROM api_costs GROUP BY date(created_at);
```

### Step D: Live Trading (ADVANCED — Real Money at Risk)

**DO NOT do this until paper trading has run for at least 2-4 weeks with positive results.**

Live mode requires:
1. A Polymarket account with USDC on Polygon
2. An Ethereum private key that controls the account
3. The `polymarket-client-sdk` EIP-712 signing (currently stubbed with `bail!()`)

**Live mode is NOT yet implemented.** The `place_limit_order` function returns an
error in live mode. This is intentional — implementing live order signing requires
careful security work.

---

## Part 2: Will It Make Money? (Honest Assessment)

### The Short Answer

**Nobody knows.** This is a speculative trading system. Here's why:

### What the Agent Does Well

- **Risk management is solid**: Half-Kelly sizing, position limits, category diversification,
  stop-loss exits, daily API budget cap — all implemented
- **Self-funding math is correct**: It tracks whether its edge covers the cost of
  Claude API calls to evaluate markets
- **Calibration tracking**: Measures Claude's actual prediction accuracy over time
  and adjusts confidence accordingly — this is critical for avoiding overconfidence
- **Data quality scoring**: Doesn't blindly trust Claude — scores data availability
  and adjusts edge thresholds based on how much real data backs the prediction

### The Fundamental Uncertainty

The agent's profitability depends entirely on one question: **Can Claude Sonnet
predict real-world events more accurately than the Polymarket crowd?**

Here's what we know:
- Prediction markets are reasonably efficient — they aggregate information from
  thousands of informed participants
- LLMs have a knowledge cutoff and cannot access real-time breaking news
- LLMs can hallucinate confidence (say 85% when they should say 55%)
- The calibration system mitigates this over time, but needs ~50+ resolved trades
  to become statistically meaningful

### Realistic Scenarios

| Scenario | Monthly Return | Likelihood |
|----------|---------------|------------|
| Agent finds consistent small edges in low-attention markets | +2% to +8% | Possible but unproven |
| Agent breaks even after API costs | ~0% | Most likely initially |
| Agent loses money on overconfident bets | -5% to -20% | Real risk, especially early |
| Agent loses everything | Possible with live trading | Low with paper trading controls |

### Key Risks

1. **Hallucination risk**: Claude states "85% confident" but the real probability is 50%.
   The calibration system catches this *over time*, but early bets are uncalibrated.

2. **Stale information**: Markets react to breaking news instantly. Claude's knowledge
   has a cutoff and no real-time news feed. By the time Claude evaluates a market,
   the price may already reflect information Claude doesn't have.

3. **API costs eat into returns**: At $0.009/call, evaluating 100 markets/day = $0.90/day
   = ~$27/month. On a $100 paper balance, that's 27% monthly overhead just for Claude.

4. **Market efficiency**: The most liquid Polymarket markets are heavily arbitraged.
   The edge the agent finds may not be real — it may just be the market's uncertainty
   premium that resolves randomly.

### My Recommendation

1. **Run paper trading for 4+ weeks** before considering live
2. **Track the calibration table** — if Claude's accuracy is below 55%, it cannot
   be profitable (the house always wins on vig/spread)
3. **Start small if going live** — $50-100 max, treat it as tuition
4. **Never invest money you can't afford to lose** — this is experimental software
5. **Monitor daily** — check the dashboard, review trade logs, watch for anomalies

### What Would Make It More Profitable

If you want to seriously improve the odds, these additions would help (in order of impact):

1. **Real-time news integration** — Feed current news headlines into Claude's prompt
   so it's not trading on stale information
2. **Multi-model consensus** — Compare Claude's estimate with GPT-4 or Gemini;
   only trade when models agree (reduces hallucination risk)
3. **Specialization** — Focus on 1-2 categories where LLMs have genuine information
   advantage (e.g., crypto sentiment analysis, weather prediction)
4. **Historical backtesting** — Run the strategy against past resolved markets to
   measure what the actual hit rate would have been
5. **Longer time horizons** — Markets resolving in 7-14 days give more time for
   information to be priced in; avoid same-day resolution markets

---

## Part 3: Quick Commands Reference

```bash
# Build and test
cargo check && cargo test && cargo clippy

# Run in paper mode
cargo run

# Run with debug logging
RUST_LOG=debug cargo run

# View dashboard
open http://127.0.0.1:8080

# Check trades in DB
sqlite3 polymarket-agent.db "SELECT * FROM trades ORDER BY created_at DESC LIMIT 10;"

# Check if agent is profitable
sqlite3 polymarket-agent.db "SELECT SUM(CAST(pnl AS REAL)) FROM trades WHERE status LIKE 'RESOLVED%';"

# Ctrl+C to gracefully stop the agent
```
