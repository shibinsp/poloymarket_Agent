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
  "uptime_seconds": 25200
}
```

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
8. **Live mode is not yet production-safe.** As of 2026-09-17 the live path records orders as filled without confirmation, hardcodes a 7-day order expiry, has no kill switch, drawdown breaker, or daily-loss limit, and submits NO-side entries with an inverted order side. Do not run `--mode live` until the go-live gate in the roadmap is complete.

## License

Private — not for redistribution.
