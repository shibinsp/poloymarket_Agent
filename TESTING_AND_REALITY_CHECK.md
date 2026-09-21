# Polymarket Agent — Testing Guide & Profitability Reality Check

## Part 1: How to Test (Step by Step)

Four levels, cheapest first. Each one answers a different question, and the
later ones are the only ones that have ever found the interesting bugs.

### Level 1 — the test suite (no keys, no network)

```bash
cd polymarket-agent
cargo test            # ~720 tests, in-memory SQLite and wiremock throughout
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

Answers: *does the logic hold*. It will not tell you whether the pieces are
wired to each other — every serious defect found in this project so far lived
in the assembly, not the units.

### Level 2 — run it against a fake venue (no keys, no network)

```bash
./dev/run-local.sh
```

Starts `dev/mock_venue.py` (a fake Alpaca and a fake model on one port) and the
agent against `config/local-mock.toml`, on a 15-second cycle. Open
<http://127.0.0.1:8080>. Within a minute you should see it discover BTC/USD,
take a view, place and fill three orders, record the fills with slippage, and
then stop at the open-position limit saying so.

```bash
./dev/run-local.sh --dry-run    # the preflight, same stack
sqlite3 dev/local.db "select venue_id, symbol, state, filled_qty from orders;"
```

Answers: *is it wired together* — discovery, sizing, ordering, fills,
reconciliation, the halt path, the dashboard. The numbers are fabricated; this
says nothing about whether the strategy works.

### Level 3 — a real venue with fake money

Get **Alpaca paper** keys (free, no funding) and put them in `.env`:

```bash
ALPACA_API_KEY_ID=...
ALPACA_API_SECRET_KEY=...
LLM_API_KEY=...            # a real model key; see config/all-connections.toml
```

```bash
CONFIG_PATH=config/paper.toml cargo run -- --dry-run   # do this first
CONFIG_PATH=config/paper.toml cargo run
```

`--dry-run` checks each venue's credentials, whether the account can trade, how
many of your configured symbols the venue actually lists, a live quote, and
whether it can report the equity the circuit breakers need. Run it before every
change of config.

Answers: *does our understanding of the API match the API*. This is the first
level that can find that, and it is where the surprises are — every adapter in
this repo has had bugs that only a real response would reveal.

**Nothing below level 3 has ever touched a real venue.** Levels 1 and 2 both
test against mocks written from the same understanding as the adapter, so they
agree with it by construction.

### Level 4 — the paper window

Leave level 3 running for **≥14 days**, and judge it by the promotion criteria
in `config/paper.toml` (≥40 orders, ≥30 closed positions) rather than by
impression. Watch for what a short run cannot show: the UTC day rollover that
resets the daily-loss breaker, weekends, session boundaries, and slow leaks.

Coinbase and Binance.US **cannot** be exercised at levels 3 or 4 — neither has
a paper endpoint, which is why the agent refuses to build them outside live
mode. Going live on either means going live on an adapter with no paper track
record, so start with a hard, low `max_live_total_notional_usd`.

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
3. Being legally allowed to trade on Polymarket — its international CLOB prohibits
   US persons under its Terms of Service

**Live mode IS implemented (since commit `7c351e8`) but the Polymarket path is
still NOT safe to run.** `--mode live` places real EIP-712-signed orders. That
path still records orders as filled without confirming the fill and still
hardcodes a 7-day order expiry regardless of `order_ttl_seconds`. The inverted
NO-side bug previously listed here has been fixed. Treat `--mode live` against
Polymarket as disabled.

The **venue path** (`[[venues]]`, e.g. Alpaca) is a different code path and
does have the safety gate — kill switch, circuit breakers, per-cycle
reconciliation, budget ledger, verified backups. See *Safety controls* in the
README. That makes it *mechanically* ready; whether it should be given money
is the question Part 2 is about, and the answer there has not changed.

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
