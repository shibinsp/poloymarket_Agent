# Polymarket Autonomous Trading Agent

A self-sustaining autonomous trading agent built in Rust that trades on [Polymarket](https://polymarket.com/) prediction markets. The agent scans markets, estimates fair value using Claude AI, identifies mispriced contracts, sizes positions using Kelly Criterion, executes trades, and pays its own API bills from profits. If balance hits $0, the agent shuts down ("dies").

## System Architecture

```
┌─────────────────────────────────────────────────────┐
│                   AGENT CORE (Rust)                 │
├─────────────┬─────────────┬─────────────────────────┤
│  Scheduler  │  Portfolio  │   Self-Funding Module   │
│  (10 min)   │  Manager    │   (API bill tracking)   │
├─────────────┴─────────────┴─────────────────────────┤
│                 MARKET SCANNER                       │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐    │
│  │ Weather  │ │ Sports   │ │ Crypto/Politics  │    │
│  │ (NOAA)   │ │ (ESPN)   │ │ (on-chain+news)  │    │
│  └──────────┘ └──────────┘ └──────────────────┘    │
├──────────────────────────────────────────────────────┤
│              VALUATION ENGINE                        │
│  Claude API → Fair Value Estimate → Edge Calc        │
├──────────────────────────────────────────────────────┤
│              POSITION SIZING (Kelly Criterion)       │
│  Half-Kelly · Max 6% bankroll · Portfolio limits     │
├──────────────────────────────────────────────────────┤
│              EXECUTION ENGINE                        │
│  Polymarket CLOB API → Limit orders → Fill tracking  │
├──────────────────────────────────────────────────────┤
│              MONITORING & SURVIVAL                   │
│  Health endpoint · Discord alerts · Metrics · SQLite │
└──────────────────────────────────────────────────────┘
```

## How It Works

Every 10 minutes — or, once venues are configured, whenever a venue has something
tradeable, waking at the next market open rather than idling through a closed one —
the agent runs a cycle:

1. **Scan** — Discovers active markets via the Polymarket CLOB/Gamma API, filtered by volume (>$5k), spread (<5%), and resolution date (<14 days)
2. **Data** — Gathers external context (weather via NOAA, sports via ESPN, crypto feeds, news) relevant to each market
3. **Value** — Sends market data + external signals to Claude AI to estimate a fair probability
4. **Edge** — Compares the AI's fair value against the market price; only trades if edge >8% (6% at high confidence)
5. **Size** — Applies half-Kelly criterion with confidence scaling, portfolio constraints, and liquidity-adjusted sizing from order book depth
6. **Execute** — Places limit orders on Polymarket (paper-simulated by default, never market orders)
7. **Survive** — Tracks burn rate, monitors bankroll, and transitions through lifecycle states

### Agent Lifecycle States

| State | Balance | Behavior |
|-------|---------|----------|
| **Alive** | > $10 | Normal operation — full scan, trade, monitor |
| **LowFuel** | $2 – $10 | Quarter-Kelly sizing, reduced scan scope |
| **CriticalSurvival** | < next cycle cost | No new trades, monitor existing positions only |
| **Dead** | $0 | Log final state, send death alert, shutdown |

## Project Structure

```
polymarket-agent/
├── Cargo.toml                  # Dependencies & build config
├── .env.example                # Required environment variables
├── config/
│   └── default.toml            # All tunable parameters
├── migrations/
│   └── 001_init.sql            # SQLite schema (trades, cycles, api_costs)
├── deploy/
│   ├── setup.sh                # Ubuntu VPS setup script
│   └── polymarket-agent.service # systemd service file
├── src/
│   ├── main.rs                 # Entry point, mode dispatch (paper/live/backtest)
│   ├── config.rs               # TOML + env config loading
│   ├── agent/
│   │   ├── lifecycle.rs        # Agent state machine, 10-minute heartbeat loop
│   │   └── self_funding.rs     # Burn rate, survival checks, cost-vs-edge analysis
│   ├── market/
│   │   ├── models.rs           # Domain types (Market, OrderBook, Side, AgentState)
│   │   ├── polymarket.rs       # CLOB API wrapper with paper trading, rate limiting, retry
│   │   └── scanner.rs          # Market discovery and filtering pipeline
│   ├── data/
│   │   ├── weather.rs          # NOAA weather data source
│   │   ├── sports.rs           # ESPN sports data source
│   │   ├── crypto.rs           # Crypto price feeds & on-chain metrics
│   │   └── news.rs             # News headline aggregation
│   ├── valuation/
│   │   ├── claude.rs           # Claude API client with token/cost tracking
│   │   ├── fair_value.rs       # Valuation prompt construction & response parsing
│   │   └── edge.rs             # Edge calculation and confidence-based threshold gating
│   ├── risk/
│   │   ├── kelly.rs            # Kelly criterion with half-Kelly, state-aware scaling
│   │   ├── portfolio.rs        # Portfolio constraints (exposure, concentration, duplicates)
│   │   └── limits.rs           # Liquidity-adjusted sizing from order book depth
│   ├── execution/
│   │   ├── order.rs            # Order preparation and placement
│   │   ├── fills.rs            # Fill tracking and P&L recording
│   │   └── wallet.rs           # Balance and exposure monitoring
│   ├── monitoring/
│   │   ├── logger.rs           # Structured JSON logging via tracing
│   │   ├── metrics.rs          # Performance metrics (Sharpe, win rate, ROI, drawdown)
│   │   ├── alerts.rs           # Discord webhook notifications
│   │   └── health.rs           # Agent health state (served at /api/health by the dashboard)
│   ├── backtesting/
│   │   ├── engine.rs           # Backtest replay through full pipeline
│   │   ├── historical.rs       # CSV loading and synthetic data generation
│   │   └── results.rs          # P&L tracking, drawdown, Sharpe calculation
│   └── db/
│       └── store.rs            # SQLite via sqlx (trades, cycles, api_costs)
└── tests/
    ├── integration.rs
    └── kelly_tests.rs
```

## Prerequisites

- **Rust** >= 1.88.0
- **SQLite** (bundled via sqlx — no external install needed)

```bash
# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## Quick Start

```bash
# Clone the repository
git clone https://github.com/shibinsp/poloymarket_Agent.git
cd poloymarket_Agent/polymarket-agent

# Build the project
cargo build --release

# Copy and configure your API keys
cp .env.example .env
# Edit .env with your actual keys

# Run a backtest first (no API keys needed)
# Edit config/default.toml → mode = "backtest"
cargo run --release

# Run in paper trading mode (requires ANTHROPIC_API_KEY)
# Edit config/default.toml → mode = "paper"
cargo run --release
```

## Configuration

### Environment Variables (`.env`)

| Variable | Required | Description |
|----------|----------|-------------|
| `LLM_API_KEY` | Yes (paper/live) | API key for the configured valuation provider. Falls back to `ANTHROPIC_API_KEY`. |
| `ANTHROPIC_API_KEY` | — | Legacy fallback for `LLM_API_KEY`. |
| `POLYMARKET_PRIVATE_KEY` | Yes (live) | Ethereum private key for signing orders |
| `DISCORD_WEBHOOK_URL` | No | Discord webhook for trade/status alerts |
| `NOAA_API_TOKEN` | No | NOAA weather API for weather market data |
| `ESPN_API_KEY` | No | ESPN API for sports market data |
| `RUST_LOG` | No | Log level filter (default: `info`) |
| `DASHBOARD_TOKEN` | No* | Bearer token for the dashboard's `/api/*` routes. *Required when `dashboard_bind` is not loopback — live mode refuses to start without it. |
| `CONFIG_PATH` | No | Path to the TOML config (default: `config/default.toml`). Deployments point this outside the git checkout. |

### Config File (`config/default.toml`)

<details>
<summary>Full configuration reference</summary>

**Agent:**
| Parameter | Default | Description |
|-----------|---------|-------------|
| `mode` | `"paper"` | `paper`, `live`, or `backtest` |
| `cycle_interval_seconds` | `600` | Time between cycles while any venue has something tradeable (10 min) |
| `max_sleep_seconds` | `3600` | Longest sleep while every venue is closed; the agent still wakes to mark positions and reconcile orders |
| `initial_paper_balance` | `100.0` | Starting balance in paper mode |
| `low_fuel_threshold` | `10.0` | Balance threshold for LowFuel state |
| `death_balance_threshold` | `0.0` | Balance threshold for Dead state |
| `api_reserve` | `2.0` | Reserved balance for API costs |

**Scanning:**
| Parameter | Default | Description |
|-----------|---------|-------------|
| `max_markets` | `1000` | Max markets to scan per cycle |
| `min_volume_24h` | `5000.0` | Minimum 24h volume filter |
| `max_resolution_days` | `14` | Only markets resolving within N days |
| `max_spread_pct` | `0.05` | Max 5% spread (illiquidity filter) |
| `categories` | `["weather", "sports", "crypto", "politics"]` | Market categories to scan |

**Valuation:**
| Parameter | Default | Description |
|-----------|---------|-------------|
| `provider` | `"anthropic"` | `anthropic` or `openai_compatible` (NVIDIA NIM, vLLM, OpenRouter, …) |
| `model` | `"claude-sonnet-4-20250514"` | Model id as the provider names it (accepts the old `claude_model` key) |
| `base_url` | provider default | API root. Required for `openai_compatible`, e.g. `https://integrate.api.nvidia.com/v1` |
| `input_price_per_million` / `output_price_per_million` | provider default | Cost-tracking rates. Default to Claude pricing for `anthropic` and **$0** for `openai_compatible` — set them if your endpoint bills you, or the daily budget cap treats calls as free |
| `min_edge_threshold` | `0.08` | Minimum edge to trade (8%) |
| `high_confidence_edge` | `0.06` | Reduced threshold at high confidence |
| `low_confidence_edge` | `0.10` | Raised threshold at low confidence |
| `cache_ttl_seconds` | `300` | Valuation cache duration |

**Risk:**
| Parameter | Default | Description |
|-----------|---------|-------------|
| `kelly_fraction` | `0.5` | Half-Kelly (fraction of full Kelly) |
| `max_position_pct` | `0.06` | Max 6% of bankroll per position |
| `max_total_exposure_pct` | `0.30` | Max 30% total portfolio exposure |
| `max_positions_per_category` | `3` | Concentration limit per category |
| `min_position_usd` | `1.0` | Minimum trade size |

**Execution:**
| Parameter | Default | Description |
|-----------|---------|-------------|
| `order_type` | `"limit"` | Always limit orders (never market) |
| `order_ttl_seconds` | `300` | Order expiry (5 min) |
| `max_slippage_pct` | `0.02` | Max 2% slippage from midpoint |
| `max_retries` | `3` | Retry attempts on transient failures |

**Rate Limiting:**
| Parameter | Default | Description |
|-----------|---------|-------------|
| `requests_per_second` | `10` | Token bucket rate |
| `burst_size` | `20` | Token bucket burst capacity |
| `backoff_base_ms` | `1000` | Exponential backoff base |
| `backoff_max_ms` | `30000` | Exponential backoff ceiling |

</details>

## Operating Modes

### Backtest

Replays historical or synthetic market data through the full pipeline without any API calls. Place a CSV file at `data/backtest.csv` or the agent generates 500 synthetic snapshots automatically.

```toml
[agent]
mode = "backtest"
```

CSV format:
```csv
timestamp,market_id,question,category,yes_price,no_price,volume_24h,spread,end_date,resolved_outcome
2025-01-01T00:00:00Z,m1,Will BTC hit 100k?,crypto,0.65,0.35,50000,0.03,2025-01-08T00:00:00Z,1.0
```

The backtester outputs a full results summary including win rate, Sharpe ratio, max drawdown, profit factor, edge accuracy, and net P&L after API costs.

### Paper Trading (default)

Simulates all trades locally. Orders fill at limit price. No real money is used and no Polymarket API keys are needed. Claude API is called for valuations.

```toml
[agent]
mode = "paper"
```

### Live Trading

Places real orders on Polymarket via the CLOB API. Requires a funded Polygon wallet.

```toml
[agent]
mode = "live"
```

**Run paper mode for at least 48-72 hours before going live.**

## Database Schema

All trade history, cycle metrics, and API costs are persisted in SQLite:

- **`trades`** — Every trade: market, direction, entry price, size, edge, Kelly fractions, P&L, status
- **`cycles`** — Per-cycle: markets scanned, opportunities found, trades placed, bankroll, agent state
- **`api_costs`** — Per-call: provider, tokens used, cost, cycle number

## Monitoring

### Dashboard

While running in paper or live mode, the agent serves a web dashboard at `http://127.0.0.1:8080` (`dashboard_bind` / `dashboard_port` in `config/default.toml`). It has six pages — Overview, Trades, Cycles, Costs, Health and Settings — reached by hash routes such as `#/trades`.

If `DASHBOARD_TOKEN` is set, every `/api/*` route except `/api/health` requires `Authorization: Bearer <token>`; the page asks for it once and remembers it in the browser, and it can be changed, cleared or tested from the Settings page. Binding to a non-loopback address without a token is refused in live mode and falls back to `127.0.0.1` otherwise.

Because `/api/health` stays public, the header keeps reporting whether the agent is alive even when nothing else will load. Every figure is shown with how old it is, and if the agent stops the page says so instead of leaving stale numbers looking current.

**`static/index.html` is a build artifact — do not edit it.** The dashboard source lives in [`polymarket-agent/ui/`](polymarket-agent/ui/README.md) (React + TypeScript, built by Vite into that one self-contained file). After changing it:

```bash
cd polymarket-agent/ui
npm install
npm run build:embed   # vite build, then cargo build
```

`cargo build` is not optional: the page is embedded with `include_str!` at
compile time, so a rebuilt bundle does not reach the binary — or a running
agent — until Rust recompiles and the agent restarts.

### Pages

| Page | What it answers |
|---|---|
| Overview | Is it alive, and is it making or losing money |
| Trades | What positions were taken, and how they resolved |
| **Orders** | **Did execution work — fill rate, slippage, time to fill, stuck orders** |
| **Risk** | **How close each breaker is, the drawdown curve, reconciliation history** |
| Cycles | What each pass did, and how long it took |
| Costs | What the valuations cost |
| Health | Liveness, anomaly counts, and the halt control |
| Settings | Poll interval, theme, token |

Orders and Risk are the two the paper window is read from. The promotion
criteria are a fill rate, a median and p95 slippage, zero unresolved
reconciliation mismatches and a bounded drawdown — none of which the other
pages show, because they are about what happened *after* execution worked.

Execution statistics are computed in the browser from the most recent 500
orders and fills, and the page says so. Fill rate is measured over orders that
reached a terminal state, so submitting an order does not make the venue look
worse until it finishes.

### Starting the paper window

The agent only exercises the venue path when `[[venues]]` is configured.
Without it, it falls back to the legacy Polymarket-only loop — which is not
what the safety gate covers, and is not available to US persons at all.

```bash
cp config/paper.toml config/local.toml      # gitignored
# Edit [database] in it. The template ships the production path
# (/var/lib/polymarket-agent), which a normal user cannot create — either
# make that directory writable or point it somewhere you can write.
echo 'CONFIG_PATH=config/local.toml' >> .env
# Alpaca → Paper account → API keys. Paper keys do not work against the live
# endpoint, or the reverse; the failure is a 403 at startup.
echo 'ALPACA_API_KEY_ID=...'     >> .env
echo 'ALPACA_API_SECRET_KEY=...' >> .env
cargo run --release -- --dry-run            # check the keys before committing 14 days
cargo run --release -- --mode paper
```

`--dry-run` iterates every enabled venue and reports, in order:

```
5. Venues:
   alpaca:
      auth + balance: ✅ 100000.00 USD available, 100000.00 equity
      instruments: ✅ 4 of the configured universe
      quote: ✅ BTC/USD 60000/60010 (mid 60005)
      session: open (has tradeable assets now)
```

It exits non-zero if any of that fails, and prints the whole error chain —
"Failed to fetch the Alpaca account: HTTP 403" rather than just the first
line, because a 403 from mismatched paper/live keys needs a different fix
from a timeout. Paper keys work only against `paper-api.alpaca.markets` and
live keys only against `api.alpaca.markets`.

Two failures it is specifically there to catch, because neither shows up as
an error once the agent is running — it logs healthy cycles and trades
nothing, for as long as you leave it:

- **no instruments discovered** — usually `scanning.max_markets` set below
  the symbol universe, which caps the venue scan and not just the legacy one
- **no equity figure** — the circuit breakers cannot run without one

Polymarket is checked only when it is an enabled venue. It is not one in the
paper template, so an unreachable Gamma no longer fails a dry run about an
Alpaca deployment.

`config/paper.toml` raises `max_daily_loss_usd` above the shipped default,
deliberately and with the reason in the file: $5 is the right number for a
$50–100 live rollout and the wrong one against a $100,000 paper account,
where it halts most mornings and the window never reaches the ≥40 orders the
criteria ask for.

#### Where each promotion criterion is read

| Criterion | Where |
|---|---|
| ≥40 orders, fill rate ≥60% | Orders → Execution quality |
| median slippage ≤10 bps, p95 ≤30 bps | Orders → Execution quality |
| zero `UNKNOWN` orders older than a cycle | Orders → Unknown tile |
| ≥30 closed positions, ≥1 stop and ≥1 take-profit filled | Trades |
| zero unresolved reconciliation mismatches | Risk → Reconciliation |
| max drawdown ≤10%, no day ≤−5% | Risk → Equity and drawdown |
| Brier ≤0.24 on ≥30 closes | `/api/risk` → `brier_score` |
| spend ≤80% of budget daily | Costs |
| cycle uptime ≥99% | Cycles |
| kill-switch and `kill -9` drills | manual; see *Stopping the agent* |

Brier is reported as `null` until 30 forecasts have resolved — over four
closes it is noise with a decimal point, and a number there would be read as
a pass. 0.25 is what always guessing 50% scores, so 0.24 is barely a view at
all; that is the point of the threshold.

**30 closed positions proves the plumbing, not edge.**

### Venues

| Venue | Assets | Paper mode | Status |
|---|---|---|---|
| Alpaca | US equities + crypto | yes, separate keys and host | the paper-window target |
| Coinbase Advanced Trade | spot crypto, 24/7 | **no** | implemented, live mode only |
| Polymarket | prediction markets | in-process | legacy path; US persons may not trade it |

**Coinbase has no paper endpoint.** Its sandbox serves authentication and
serialization only — there is no matching engine — so an enabled Coinbase
venue reaches the *live* exchange. That is different from Alpaca, where paper
and live are different hosts and different keys, and there is no paper
simulator on the `Venue` path either: the cycle calls `place_order` on every
venue in the registry whatever the mode.

So the agent **refuses to build a Coinbase venue unless `agent.mode = "live"`**
and names it in the startup log and in `--dry-run`. Leaving this to a disabled
line in the shipped config made a comment the only thing between a paper window
and real money — and the file operators are told to edit is `config/local.toml`,
which that comment is not in.

`fee_pct` in the template is the **taker** rate at the lowest volume tier
(~1.2%), not the maker rate: orders go out as limit GTC without `post_only`, so
they can take, and `round_trip_cost` doubles the number. Quoting the maker rate
would let trades that are negative after fees clear the edge threshold.

Credentials are `COINBASE_API_KEY_NAME` (`organizations/{org}/apiKeys/{key}`)
and `COINBASE_API_PRIVATE_KEY`, the EC PEM issued with it. Escaped newlines
are accepted, since that is how the key arrives when pasted out of the
downloaded JSON.

Requests are signed with a per-request ES256 JWT whose `uri` claim names the
method, host and path, so a token cannot be replayed against another
endpoint. It expires in two minutes.

### Tracing (OpenTelemetry / Langfuse)

The agent exports spans over OTLP when an endpoint is configured, and stays
silent otherwise. Set one env var and it turns itself on:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318        # any OTLP collector
# or, for a self-hosted Langfuse:
LANGFUSE_HOST=http://localhost:3000
LANGFUSE_PUBLIC_KEY=pk-lf-...
LANGFUSE_SECRET_KEY=sk-lf-...
```

Spans cover the trading cycle, the venue cycle and exit pass, reconciliation,
order placement, and every model call. The valuation spans carry GenAI
semantic-convention attributes (`gen_ai.request.model`,
`gen_ai.usage.input_tokens`, `gen_ai.usage.cost`), so Langfuse renders them as
generations with cost and token counts rather than as anonymous spans.

`telemetry.export_content` controls whether prompts and completions travel with
the span. It defaults to on, which assumes the destination is infrastructure you
control. Turn it off and the spans keep model, token counts, cost and latency
but carry no text.

Two things it deliberately does *not* do:

- **Prompts never reach the ordinary logs.** `export_content` governs what
  rides on an exported span, and nothing else. With no exporter configured
  there is no span to ride on, and the JSON log formatter is configured not to
  copy span fields into log lines at all.
- **An explicit `enabled = false` beats the environment.** Setting
  `OTEL_EXPORTER_OTLP_ENDPOINT` turns export on only when the config file has
  not expressed an opinion. On a platform that injects OTLP variables for every
  process, write `enabled = false` under `[telemetry]` and the agent will not
  export no matter what the environment says.

`RUST_LOG` controls the log level only. Span export has its own level, from
`OTEL_LOG_LEVEL`, defaulting to `polymarket_agent=info` — so quieting the logs
does not silently turn tracing off, and dependency spans stay out of a paid
ingest.

A telemetry endpoint that cannot be parsed or reached is a monitoring problem,
never a startup failure: the agent logs why export is off and trades on.

This is what makes a slow cycle legible: a 200-second cycle caused by a venue
retrying behind a blocked DNS entry is three unrelated warnings in the logs, and
one span with three children in a trace.

### Health Check

```bash
curl http://localhost:8080/api/health
```

```json
{
  "status": "ok",
  "agent_state": "ALIVE",
  "cycle_number": 42,
  "started_at": "2026-09-17T08:00:00Z",
  "last_cycle_at": "2026-09-17T15:00:00Z",
  "next_cycle_due": "2026-09-17T15:10:00Z",
  "uptime_seconds": 25200,
  "alerts_delivering": true,
  "anomalies": { "venue_unreachable": 3 },
  "halted": false,
  "halt": null
}
```

`status` is `ok`, `halted`, or `dead`, and — like `halted` — is read from the
kill switch at request time, so an uptime probe keyed on it sees a halt
immediately rather than at the end of the next cycle. `anomalies` counts *occurrences* per
kind since start — alerts are deduped per `(kind, scope)` for 30 minutes, so
one webhook message can stand for hundreds of these, and the difference
between "once" and "four hundred times" is the interesting part.

`halted` and `halt` are read from the kill switch at request time, not from
the last completed cycle, so a halt raised mid-cycle shows up immediately.

## Safety controls

These are what stand between a bad afternoon and a lost account. All of them
apply to the **venue path**; see caveat 8 for what the legacy Polymarket path
still lacks.

### Stopping the agent

Halting stops *new positions*. Exits, order polling, reconciliation and
settlement all keep running — refusing to close a position because the day
went badly is how a bounded loss becomes an unbounded one. It is not a
shutdown, and it does not close what is already open.

Four ways in, because the one that works is whichever is reachable:

The data directory is `database.data_dir` in the config file, defaulting to
whatever directory `database.path` lives in.

```bash
touch <data_dir>/HALT                    # survives a restart; no token needed
echo "why" > <data_dir>/HALT             # the contents become the reason
kill -USR1 $(pgrep polymarket-agent)     # a PID and nothing else
curl -X POST -H 'x-agent-control: 1' localhost:8080/api/halt   # or the button
```

The `HALT` file is polled every five seconds and wakes the idle loop, so the
agent cancels its resting orders promptly rather than at its next scheduled
wake — which across a closed weekend is an hour away. Removing the file
resumes; it does not clear a halt raised by anything else.

```bash
curl -X POST -H 'x-agent-control: 1' localhost:8080/api/resume
curl -X POST -H 'x-agent-control: 1' localhost:8080/api/reconcile/ack
```

The `x-agent-control` header is required on every state-changing route, on top
of the bearer token. Its value is irrelevant; its presence is what stops the
request being a CORS *simple request*. Without it, any page the operator
happened to browse could `fetch('http://localhost:8080/api/resume', {method:
'POST'})` — the browser would send it and the side effect would land, lifting
the very drawdown halt that is supposed to need a person. A cross-site
`Origin` is refused outright as well.

A halt is written to the `halts` table and reinstated on startup. Restarting
the process is the first thing anyone does when something looks wrong; if
that cleared a drawdown halt, the breaker would be decorative.

### Circuit breakers

Configured under `[risk]`. Every other control here is per-trade — Kelly sizes
one position, the stop bounds one loss — and none of them bounds a *sequence*.
Twenty trades each losing their stop is twenty correctly-sized losses and a
ruined account.

| Limit | Default | Halt lasts |
|---|---|---|
| `max_daily_loss_pct` | 5% of the day's opening equity | rest of the UTC day |
| `max_daily_loss_usd` | $5 | rest of the UTC day |
| `max_drawdown_pct` | 15% below the all-time equity high | until resumed |
| `max_trades_per_day` | 10 | rest of the UTC day |
| `max_consecutive_losses` | 4 | rest of the UTC day |
| `max_live_notional_per_position_usd` | $10 | sizing cap, live only |
| `max_live_total_notional_usd` | $60 | sizing cap, live only |

Drawdown is measured peak-to-trough over the life of the account, not within
the day: an intraday high would reset the yardstick every midnight and let an
account bleed 3% a day for a fortnight without ever reporting a drawdown.

The two `max_live_*` caps are absolute cash and apply only in live mode. A
percentage of a paper balance is a number nobody agreed to — 6% of a $100,000
paper account is a $6,000 position, and the first live run must not inherit
it.

Losses trip on a strict `>` and counts on a `>=`, so `0` means "none
tolerated" rather than "disabled". To switch a check off, set it high.

### Reconciliation

Every cycle, each venue is asked what positions it holds and what orders are
resting, and the answer is compared with the local ledger. A disagreement
halts until acknowledged; a venue that cannot be *asked* does not halt, since
a venue that cannot answer cannot accept orders either. Results go to
`reconciliation_runs`.

The cash comparison the roadmap called for is deliberately not implemented:
there is no local cash ledger to compare against, and Alpaca's `available` is
`non_marginable_buying_power`, which moves with margin state, dividends and
settlement timing. Comparing against it would produce mismatches that are not
mismatches, and a false halt is an outage with extra steps.

### Valuation budget

`agent.daily_api_budget` (default **$0.50**) is enforced by a ledger that
reserves each call's estimated cost *before* it is made. The previous check
read the day's total once and then spawned a batch against that number, so ten
concurrent calls could each see "$0.40 of $0.50 spent, fine". A failed call
releases its reservation, so a provider outage cannot consume the day's budget
having produced nothing.

### Backups

An hourly `VACUUM INTO` snapshot into `<data_dir>/backups`, then opened and
`PRAGMA integrity_check`ed — an unverified backup is a guess, and you find out
it was wrong at the worst moment. A snapshot that fails the check is deleted
so a restore reaches for an older one. Retention is 48 hourly plus one per day
for 30 days, as a union, so an agent that was offline for a month does not
come back to an empty directory.

### Discord Alerts

Real-time notifications for:
- Trade placed (market, size, edge, direction)
- Trade resolved (win/loss, P&L)
- Bankroll milestones ($50, $100, $200, $500, $1k, $2k, $5k, $10k)
- Agent state changes (Alive, LowFuel, CriticalSurvival, Dead)
- Daily performance summary

```toml
[monitoring]
discord_enabled = true
```

### Structured Logging

JSON-formatted logs via `tracing`:

```bash
RUST_LOG=info cargo run --release    # Standard
RUST_LOG=debug cargo run --release   # Verbose
```

Every cycle logs: markets scanned, opportunities found, trades placed, API cost, bankroll, agent state, and duration.

## Deployment

### VPS Setup (~$4.50/month Ubuntu 22.04)

```bash
# Upload or clone repo on VPS
sudo bash deploy/setup.sh

# Configure API keys (outside the read-only install dir)
sudo nano /etc/polymarket-agent/env

# Start the service
sudo systemctl start polymarket-agent

# Monitor
sudo systemctl status polymarket-agent
sudo journalctl -u polymarket-agent -f
curl http://localhost:8080/api/health
```

The systemd service includes security hardening:
- `NoNewPrivileges=true`
- `ProtectSystem=strict`
- `ProtectHome=true`
- `ReadWritePaths=/var/lib/polymarket-agent` (database only)
- Automatic restart on failure with 30s delay

## Testing

```bash
# Run all unit and integration tests
cargo test

# Run with output visible
cargo test -- --nocapture

# Run specific module tests
cargo test risk::kelly
cargo test backtesting::engine
cargo test monitoring::metrics

# Lint check
cargo clippy -- -D warnings
```

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| `polymarket-client-sdk` | Official Polymarket CLOB + Gamma API (uses `alloy` for EIP-712 signing) |
| `tokio` | Async runtime |
| `reqwest` | HTTP for Claude, NOAA, ESPN, and other external APIs |
| `sqlx` | Async SQLite with migrations |
| `rust_decimal` | Precise decimal arithmetic for all monetary values |
| `tracing` | Structured logging |
| `governor` | Token-bucket rate limiting |
| `chrono` | Timestamps and date handling |

## Design Decisions

- **`rust_decimal::Decimal`** for all monetary values — never `f64` for money
- **Half-Kelly sizing** — full Kelly is too aggressive; half-Kelly balances growth and drawdown risk
- **State-aware scaling** — position sizes automatically reduce in LowFuel (quarter-Kelly) and freeze in CriticalSurvival
- **Edge-justifies-cost gate** — trades are skipped if projected profit doesn't exceed the API cost to evaluate them
- **Paper trading by default** — the agent never touches real money unless explicitly configured for live mode
- **Official Polymarket SDK** — uses `polymarket-client-sdk` with `alloy` for EIP-712 order signing (not deprecated `ethers-rs`)
- **Limit orders only** — never market orders; protects against slippage and thin order books
- **Liquidity-aware sizing** — position size is capped at 20% of available order book depth at the target price

## Risk Warnings

1. **This is NOT guaranteed profit.** Prediction markets are zero-sum minus fees. Edge decays as markets become efficient.
2. **Regulatory risk.** Polymarket access varies by jurisdiction. Do your own research on legal compliance.
3. **API dependency.** Claude API outages mean the agent cannot value markets. The agent enters survival mode rather than trading blind.
4. **Liquidity risk.** Thin order books mean large positions can't exit cleanly.
5. **Model risk.** Claude can be confidently wrong. The confidence score is self-assessed, not externally calibrated.
6. **Black swan risk.** A single unexpected event can wipe correlated positions.
7. **Jurisdiction.** Polymarket's international CLOB prohibits US persons from trading under its Terms of Service. If you are in the US, do not fund or run live mode against it.
8. **The two live paths are not equally ready, and the difference matters.**

   The **venue path** (Alpaca; `[[venues]]` configured) has the go-live safety
   gate described under *Safety controls* below: confirmed fills, per-cycle
   reconciliation, circuit breakers, a kill switch, a budget ledger, and
   hourly verified backups.

   The **legacy Polymarket path** does not. It still records an order as
   filled the moment an order id comes back (`src/execution/order.rs:160`) and
   still hardcodes a 7-day order expiry regardless of `order_ttl_seconds`
   (`order.rs:220`, `polymarket.rs:488`). The inverted NO-side bug reported
   here previously *has* been fixed. Do not run `--mode live` against
   Polymarket — and if you are a US person, see point 7, which makes the
   question moot.

## License

Private — not for redistribution.
