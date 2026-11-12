# MoneyAI NSE Intraday Agent: Development and Tuning Plan

Source design: `Intraday_Trading_Agent_Architecture.docx` (v1.3, 10 Nov 2026).
Codebase: `polymarket-agent/` (Rust). All paths below are relative to it.
Status as of 12 Nov 2026: **design stage. Profit confidence is 30/100 (the doc's estimate).**

> This is an engineering plan, not investment advice. Most individual intraday traders in India lose money.

---

## 1. What the architecture doc asks for

An event-driven intraday agent for liquid **Nifty 100** stocks on NSE. It has three concerns that are kept separate:

| Concern | Component | Hard rule |
|---|---|---|
| Find trades | Rule strategies: opening range breakout (ORB), VWAP reversion, relative-volume momentum. A gradient-boosted filter is added later. | A strategy goes live only after it passes the backtest and paper gates |
| Limit damage | Deterministic **risk gate** | Plain code, in the same task as order submission; nothing can override it |
| Understand the day | **LLM analyst** | Restrict-only: it can drop stocks or cut size, and has no order access |

Supporting pieces: a feed handler that validates data quality, a feature engine (1m/5m bars, VWAP, relative volume, opening range), an order state machine with broker reconciliation, an internal paper broker with the full Indian cost model, a backtest lab that uses the same strategy code as live, an append-only audit store, monitoring with Telegram alerts, and a kill switch.

### The trading day (all times IST, all from config)
1. Login with 2FA and a fresh token.
2. Pre-market LLM run.
3. Wait for the opening range.
4. Trade on closed bars only.
5. **No new entries after 14:45.**
6. **Flatten by 15:02.**
7. End-of-day review.

### Target interpretation (doc §9)
- **"85% accuracy"** means LLM news-classification accuracy (target ≥ 90%). It does **not** mean trade win rate. An 85% win-rate design needs ~0.4R wins and loses money at 80%.
- **"₹2,000/day"** is only achievable as a monthly average, and it depends on capital. It needs about ₹20-40 lakh behind a proven edge. On ₹5 lakh it would require 9% a month, which is not realistic.
- **No daily profit target in code.** Only a daily loss stop.

---

## 2. Decision: evolve the Rust engine, do not rewrite in Python

The doc proposes Python 3.12, asyncio, Redis, QuestDB and Postgres. MoneyAI already has a working Rust/tokio engine with:
- an Upstox V3 WebSocket feed;
- the `Venue` trait;
- circuit breakers and a kill switch;
- a reconciler;
- a backtester;
- SQLite;
- a dashboard.

The doc's design rules do not depend on the language, so we keep Rust and close the gaps.

| Doc component | Our choice |
|---|---|
| asyncio event loop | tokio (existing) |
| Redis Streams | in-process bounded `tokio::mpsc` channels, with a raw-message recorder to disk for replay |
| QuestDB / TimescaleDB | SQLite and Parquet files for now; revisit at more than 1 GB/month |
| PostgreSQL audit | SQLite append-only tables (migration `010_india_audit.sql`) |
| LangGraph analyst | Rust module with a fixed 5-step pipeline over `src/valuation/llm.rs` |
| LightGBM filter | Python sidecar, called offline or batched. Added only after a strategy passes its gates |

---

## 3. Gap map (doc requirement → current code)

| Requirement | Current location | Status |
|---|---|---|
| Upstox WebSocket feed | `src/india/market_feed.rs` | ✅ exists, with REST fallback |
| Stale halt at 3 s | `market_feed.rs::is_stale` (15 s, receipt time) | ⚠️ too loose; uses receipt time, not exchange time |
| Out-of-order and duplicate rejection | `process_incoming_ticks` | ⚠️ exact duplicates only; no out-of-order handling |
| Feed-health state machine | — | ❌ |
| Bars from cumulative volume | `src/india/strategy_worker.rs::BuildingCandle` | ⚠️ 5m only; a 250 ms poll misses intra-bar highs and lows |
| 1m bars, opening range, relative volume | — | ❌ |
| VWAP, EMA, ATR | `src/strategy/pullback.rs`, `src/risk/volatility.rs` | ✅ |
| Nifty 100 universe | `src/india/instruments.rs` (13 hardcoded) | ❌ |
| ORB / VWAP reversion / momentum | — (only the pullback strategy) | ❌ |
| Risk gate (0.5% / 2% / 3 positions / 6 trades / 14:45 / 15:02 / 5 orders per second) | NSE worker only checks the kill switch, stale data and session | ❌ **critical** |
| Daily boundary on the IST trading day | `src/risk/circuit_breaker.rs` uses UTC | ❌ |
| Order state machine and client tags | `src/agent/reconcile.rs`, `src/venue/types.rs::OrderState` (generic loop only) | ⚠️ not wired to NSE |
| Reconcile every 5-10 s; 3 rejects → kill | — for NSE | ❌ |
| Internal paper broker | `src/india/simulator.rs` | ⚠️ instant fill at last price, no slippage, fixed +2% / −1.5% |
| Intraday cost model | `src/india/costs.rs` | ❌ **uses delivery STT and stamp rates** |
| Backtester (same code, costs, walk-forward) | `src/india/backtest.rs`, `src/strategy/backtest.rs` | ⚠️ two engines with inconsistent costs; no walk-forward; no data downloader |
| LLM analyst (restrict-only) | `src/valuation/directional.rs` | ❌ the current LLM **drives entries**; the opposite of the doc |
| Audit store | SQLite generic tables | ⚠️ NSE decisions are logged but not persisted |
| Telegram alerts | `src/monitoring/alerts.rs` (Discord only) | ❌ |
| Kill switch | `src/agent/kill_switch.rs` | ✅ |
| Daily token login | OAuth callback, in-memory token | ⚠️ no freshness check |

### Footguns found (fixed in Phase 0)
- Two NSE trading paths (generic `venue_cycle` and `strategy_worker`) write to the same `india_sandbox_orders` ledger.
- Orders are sent as `MARKET` with product `"D"` (delivery). An unparsable quantity defaults to 1 through `unwrap_or(1)`.
- **Fake data** is used when the feed fails: sine-wave candles (`src/venue/upstox/mod.rs:126-155`), quotes defaulting to ₹1000, and seed prices in `chart.rs`.
- Cash and holdings are rebuilt from only the last 200 orders, so they drift as history grows.
- `/api/india/sandbox/order`, `/api/upstox/simulator/order` and `/api/settings/caps` are outside the same-origin control guard.
- The Upstox redirect URI uses port 8080, but the dashboard listens on 8088.

---

## 4. Phased plan

Each phase gets one branch, one PR, a `/code-review`, and a squash-merge.

**Every phase's definition of done includes:**
- `cargo fmt --check`, `cargo clippy -D warnings` and `cargo test` all green;
- a mutation check: remove each new guard and confirm its test fails;
- running the control live in paper mode, not just reading the diff.

### Phase 0: Safety and correctness (week 1)
- [ ] Rewrite `IndianCostConfig` with intraday rates:

  | Charge | Rate |
  |---|---|
  | STT | 0.025% on the sell side only |
  | Stamp duty | 0.003% on the buy side |
  | NSE transaction charge | current rate |
  | SEBI fee | current rate |
  | GST | 18% on brokerage + charges |
  | Brokerage | ₹20 per order |

  Delete the hardcoded rates in `src/strategy/backtest.rs` and call `costs.rs` instead. Add a golden test: a ₹1L round trip costs ≈ ₹83, and ₹5L costs ≈ ₹223 (doc §8).
- [ ] Disable `venue_cycle` for the India venue, so `strategy_worker` is the only NSE trader. Add a test.
- [ ] Remove all fake-data fallbacks. When the feed fails, return an error and stop entries.
- [ ] Use LIMIT or price-protected orders only, with product `"I"`. Reject an unparsable quantity.
- [ ] Put the India order and settings endpoints behind the same-origin control guard.
- [ ] Compute cash and holdings from the full ledger (or a running balance table).
- [ ] Fix the Upstox redirect port.

### Phase 1: Feed handler and feature engine (week 2)
- [ ] Order ticks by exchange timestamp. Drop any tick older than the last one for its symbol. De-duplicate on (symbol, LTT, cumulative volume).
- [ ] Mark a symbol stale after 3 s (configurable).
- [ ] Add a feed-health state machine: `Ok → Stale → Down`. Block entries while Stale; flatten after 60 s Down.
- [ ] Feed every tick through a bounded channel, keeping only the newest per symbol under burst. Never drop order events.
- [ ] Add `src/india/features.rs` with:
  - 1m and 5m bars, closed by the next minute's first tick or 2 s after the minute;
  - VWAP from cumulative ATP and volume;
  - relative volume against the same time of day over N days;
  - opening range (15 or 30 min);
  - ATR.
- [ ] Load the Nifty 100 universe from the Upstox instrument master, cached daily.
- [ ] Add a raw-message recorder (append to file) for replay.

**Done when:**
- system bars match Upstox historical candles on ≥ 99.5% of prices, with volume within 1%;
- injecting bad data leaves the bars unchanged.

### Phase 2: Risk gate, order state machine, paper broker (week 3)
- [ ] Add `src/india/risk_gate.rs`: pure synchronous checks, called in the same task as order submission.

  | Limit | Starting value | Hard ceiling in code |
  |---|---|---|
  | Risk per trade | 0.5% of capital | 1% |
  | Daily loss stop (IST trading day) | 2% | 3% |
  | Open positions | 3 | 5 |
  | Trades per day | 6 | 10 |
  | Gross exposure | below the 5× intraday margin | 5× |
  | Order rate | 5 per second | 9 per second |
  | No new entries after | 14:45 | 15:00 |
  | Flatten by | 15:02 | 15:10 |
  | Price-band check | on | always on |

  Config may tighten these limits, never loosen past the ceiling. The ceiling lives in the builder, not in a config default.
- [ ] Add an NSE order state machine: `New → Sent → Ack → Partial → Filled | Cancelled | Rejected | Unknown`.
  - Every order carries a unique client tag.
  - An `Unknown` order is looked up by its tag and **never resent**.
- [ ] Size stops and targets to the filled quantity. Cancel the unfilled remainder after a timeout.
- [ ] Reconcile with the broker every 5-10 s. A mismatch stops new entries. Three rejects within 5 min trips the kill switch.
- [ ] Rebuild the paper broker behind the `Venue` trait. It fills against bid/ask, with:
  - slippage of 4-10 bps (higher on stops and breakouts);
  - partial fills;
  - injected rejects;
  - full charges.

  Switching between paper and live is a **config change only**.
- [ ] Add an end-of-day flatten task.

**Done when:**
- an order walks through every state in tests;
- the paper run is provably flat at 15:02.

### Phase 3: Backtester and first strategy (week 4)
- [ ] Add a `Strategy` trait that consumes feature snapshots, shared by live, paper and backtest.
- [ ] Add a downloader for 3+ years of 1-minute candles (Upstox historical API) into Parquet or SQLite.
- [ ] Unify on one backtest engine with the Phase 0 cost model and next-bar fills.
- [ ] Add rolling walk-forward: train on 12 months, test on the next 3, judge only the test periods.
- [ ] Report Sharpe (per trade and daily), profit factor, max drawdown, a trial counter, the deflated Sharpe, and the probability of backtest overfitting.
- [ ] Add the ORB strategy on high relative-volume stocks (`src/strategy/orb.rs`), with three parameters: opening range length, relative-volume threshold and stop distance.

**Done when:** the backtest reproduces a hand-checked sample of 10 trades exactly.

### Phase 4: More strategies and start of paper trading (week 5)
- [ ] Add VWAP reversion (`src/strategy/vwap_reversion.rs`) and relative-volume momentum (`src/strategy/momentum.rs`).
- [ ] Re-gate the existing pullback strategy. Write a walk-forward report for each strategy and **drop any that fails**.
- [ ] Add append-only audit tables for signals, risk decisions, orders, fills and LLM I/O (`migrations/010_india_audit.sql`).
- [ ] Start continuous paper trading on the internal broker.

### Phase 5: LLM analyst in shadow mode, plus monitoring (week 6)
- [ ] Add `src/india/analyst/`, a linear 5-step pipeline that runs pre-market and every 15 minutes:

  | Step | Done by |
  |---|---|
  | 1. Gather news and events | code |
  | 2. Filter, de-duplicate, check the universe | code |
  | 3. Classify each item | model, one call per item, run in parallel |
  | 4. Compose the watchlist and bias | model |
  | 5. Validate | code |

- [ ] Output schema:

  | Field | Shape | Effect |
  |---|---|---|
  | `watchlist` | stock list | strategies focus on these stocks |
  | `bias` | `neutral` / `cautious` / `risk_off` | cautious and risk-off tighten entry rules |
  | `exclude` | stock list | no trades in these stocks |
  | `size_multiplier` | 0 to 1 | **only scales size down** |
  | `expires_at` | publication + 20 min | after expiry, defaults apply |
  | `reasons` | text with source ids | audit only |

- [ ] Event types come from a fixed enum: results, guidance, merger, regulatory, management change, block deal, rumour, other, unknown. Severity is 0-3.
- [ ] The validator rejects any claim whose source id does not exist. Invalid output is discarded, never repaired.
- [ ] Model settings: temperature 0; max tokens 300 (classify) and 600 (compose); timeouts of 10 s for tools and 20 s for the model; one retry, then fall back to defaults; discard late answers.
- [ ] Cache classifications by headline hash. Log the model, prompt and schema versions with every run.
- [ ] Shadow mode: restrictions are logged only, and trading ignores them.
- [ ] Build a golden set of 200-300 hand-labelled Nifty 100 news items dated after the model's training cutoff. Hold out 25%.
- [ ] Add Telegram alerts beside Discord.
- [ ] Add an India dashboard panel showing P&L, latency p50/p95/p99, rejects, feed state, and the rolling 50-trade win rate against break-even.

### Phase 6: Sandbox wiring, drills, compliance (week 7)
- [ ] Wire the real Upstox order API, or the Dhan sandbox, to test placement, modification, cancellation and rejects (not P&L).
- [ ] Run the drills:

  | Drill | Pass mark |
  |---|---|
  | Socket cut for 10 s and 90 s | stale flagged within 3 s; no entries while stale; clean resync; flatten at 60 s |
  | Burst replay of the first 15 min at 10× | queue stays bounded; lag ≤ 1 s |
  | Kill switch | everything flattens |
  | Reconnect | clean resync |
  | Bad-data injection | bars unchanged |

- [ ] Latency: arrival → order sent < 50 ms at p99; confirmation < 500 ms at p95.
- [ ] Compliance:
  - refuse to trade without a fresh daily token;
  - 5 orders per second cap in code;
  - no plain market orders;
  - static IP and a standby IP documented in the runbook;
  - keep every cut-off time in config.

### Phase 7: Review and freeze v1 (week 8)
- [ ] Calibrate slippage from paper fills.
- [ ] Write the go/no-go report against the gates in §6.
- [ ] Tag `v1-intraday`.

> The eight weeks build the system; they do not prove it. Paper trading continues to 40 days and 150 trades (about week 13), then 30 days live at minimum quantity.

---

## 5. Tuning playbook

**Never tune the risk limits for returns.** They are tightened on evidence, never loosened.

### Strategy rules
- At most 3 tunable parameters per strategy.
- Pick a **plateau**: a value whose neighbours also work. A setting that only works at one exact value is noise.
- Walk-forward only: tune on 12 months, test on the next 3, then roll forward.
- Count every trial and raise the bar (deflated Sharpe) as the count grows.
- Always include full costs and slippage measured from paper fills.
- Use the most recent 6 months as a final holdout, **used once**.
- Review monthly and re-validate quarterly. **Never re-tune after a losing week.**

### LLM analyst

| Setting | Start with | How to tune |
|---|---|---|
| Temperature | 0 | leave it at 0 and cache results |
| Text per item | headline + first 1,500 characters | test longer and shorter on the golden set |
| Items per run | top 30 after filtering | reduce if the run misses its time budget |
| Few-shot examples | 5-8 covering each event type | replace with items it got wrong; include Indian cases (SEBI orders, pledges, block deals, bonus/split) |
| Refresh interval | 15 min, plus event triggers | shorten only if logs show restrictions arriving late |
| Exclusion threshold | severity ≥ 2 | tune on shadow P&L |
| Size multiplier | 1.0 / 0.75 / 0.5 | steps of 0.25; never above 1.0 |
| Model | small model first | move up only if golden-set accuracy clearly improves within the latency limit |

**Quality targets:**

| Measure | Target |
|---|---|
| Schema-valid output | ≥ 99% |
| Golden-set accuracy | ≥ 90% |
| Claims with no matching source | 0 |
| Same answer on a repeat run | ≥ 95% |
| Run time, p95 | ≤ 30 s intraday, ≤ 120 s pre-market |
| Fallback rate | ≤ 2% |
| **Value of restrictions** | **positive over 40+ trading days.** This is the only thing that justifies switching the analyst on |

**Loop:**
1. Run in shadow mode.
2. Change one thing at a time.
3. Replay on the golden set and on recorded days.
4. Keep the change only if the targets hold and the value of restrictions does not fall.
5. Score once on the held-back test set.

### ML filter (only after a rule strategy passes its gates)
- **Label:** did the signal beat its costs? Not price direction.
- **Features:** relative volume, gap, range against ATR, time of day, index trend, spread, depth imbalance, sector move. Each must be point-in-time.
- **Data:** thousands of labelled signals.
- **Validation:** time-ordered splits with a purge gap; never shuffle.
- **Model:** depth 3-5, large minimum samples per leaf, low learning rate, early stopping.
- **Threshold:** calibrate probabilities, then choose the cut-off that maximises net result per trade.
- **Stability:** drop features whose importance flips between periods.
- **Retraining:** monthly. A **drift guard** switches the filter off automatically when the live hit rate leaves its band.

### High win-rate guard rails (if the operator chooses the 85% design)
- Halt the strategy if the rolling 50-trade win rate falls within 2 points of break-even.
- Hard stop at the broker; worst trade ≤ 1.2R after slippage.
- No averaging down; never double size after a loss.
- Require at least 300 paper trades before trusting the win rate.

---

## 6. Go-live gates and stop rules

| Gate | Threshold |
|---|---|
| Backtest | 3+ years of 1-minute data with full costs; 300+ out-of-sample trades; Sharpe ≥ 1.0 after costs; deflated Sharpe probability ≥ 0.95; probability of backtest overfitting ≤ 0.2 |
| Paper | ≥ 40 trading days and ≥ 150 trades; profit factor ≥ 1.3 after costs; max drawdown ≤ 6% |
| Slippage | realised ≤ 1.25× modelled |
| Operations | no unreconciled positions; no unprotected-order rejects; flat by 15:02 every day; all drills passed |
| Live at minimum quantity | ≥ 30 trading days in line with paper; then at most 2× size per month, only while drawdown < half its limit |
| **Stop rules** | halt and return to research if the 3-month profit factor < 1.0, drawdown > 10%, or slippage doubles |

Capital sizing once an edge is proven: capital needed = ₹45,000 ÷ proven monthly net return. Risk per trade stays at 0.5%.

---

## 7. Non-goals

- The LLM never places, sizes up or picks the direction of a trade.
- No daily profit target in code.
- No loosening of risk limits to improve a backtest.
- No manual entries, and no overriding the system after a losing streak.
- No selling or sharing of signals (that needs SEBI registration).
- No scalping or speed competition; the strategies work on 1-5 minute horizons.

---

## 8. Verification checklist (per phase)

1. `cargo fmt --check && cargo clippy -- -D warnings && cargo test`
2. Mutation check: delete each new guard and confirm its test fails.
3. Run the agent in paper mode and check on the `/#/india` dashboard that:
   - there are no entries after 14:45;
   - everything is flat by 15:02;
   - a feed cut gives Stale, then Down, then a flatten;
   - the daily loss stop halts entries.
4. Replay parity: a recorded day replayed through the backtester gives exactly the same signals as the live paper run.
