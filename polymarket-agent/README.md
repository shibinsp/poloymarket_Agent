# Polymarket Autonomous Trading Agent

A self-sustaining autonomous trading agent built in Rust that trades on [Polymarket](https://polymarket.com/) prediction markets. It uses Claude AI for market valuation, Kelly criterion for position sizing, and includes full lifecycle management from market scanning to execution.

## How It Works

Every 10 minutes, the agent runs a cycle:

1. **Scan** — Discovers active markets via the Polymarket CLOB/Gamma API, filtered by volume, spread, and resolution date
2. **Data** — Gathers external context (weather, sports, crypto, news) relevant to each market
3. **Value** — Sends market data to Claude AI to estimate a fair probability
4. **Edge** — Compares the AI's fair value against the market price to find mispriced markets
5. **Size** — Applies half-Kelly criterion with confidence scaling, portfolio constraints, and liquidity checks
6. **Execute** — Places limit orders on Polymarket (paper-simulated by default)
7. **Survive** — Tracks burn rate, monitors bankroll, and transitions through lifecycle states (Alive → LowFuel → CriticalSurvival → Dead)

## Architecture

```
src/
├── main.rs                 # Entry point, mode dispatch (paper/live/backtest)
├── config.rs               # TOML + env config loading
├── agent/
│   ├── lifecycle.rs        # Agent state machine, 10-minute heartbeat loop
│   └── self_funding.rs     # Burn rate, survival checks, cost analysis
├── market/
│   ├── models.rs           # Domain types (Market, OrderBook, Side, AgentState)
│   ├── polymarket.rs       # CLOB API wrapper with paper trading, rate limiting, retry
│   └── scanner.rs          # Market discovery and filtering
├── data/
│   ├── weather.rs          # NOAA weather data
│   ├── sports.rs           # ESPN sports data
│   ├── crypto.rs           # Crypto price feeds
│   └── news.rs             # News headlines
├── valuation/
│   ├── claude.rs           # Claude API client with token/cost tracking
│   ├── fair_value.rs       # Valuation prompt construction and response parsing
│   └── edge.rs             # Edge calculation and threshold gating
├── risk/
│   ├── kelly.rs            # Kelly criterion with half-Kelly, state scaling
│   ├── portfolio.rs        # Portfolio constraints (exposure, concentration, duplicates)
│   └── limits.rs           # Liquidity-adjusted sizing from order book depth
├── execution/
│   ├── order.rs            # Order preparation and placement
│   ├── fills.rs            # Fill tracking and P&L recording
│   └── wallet.rs           # Balance and exposure monitoring
├── monitoring/
│   ├── logger.rs           # Structured JSON logging via tracing
│   ├── metrics.rs          # Performance metrics (Sharpe, win rate, ROI)
│   ├── alerts.rs           # Discord webhook notifications
│   └── health.rs           # HTTP health check endpoint on :9090
├── backtesting/
│   ├── engine.rs           # Backtest replay through full pipeline
│   ├── historical.rs       # CSV loading and synthetic data generation
│   └── results.rs          # P&L tracking, drawdown, Sharpe calculation
└── db/
    ├── store.rs            # SQLite via sqlx (trades, cycles, api_costs)
    └── mod.rs
```

## Prerequisites

- **Rust** >= 1.88.0 (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- **SQLite** (bundled via sqlx, no external install needed)

## Quick Start

```bash
# Clone and build
git clone <your-repo-url>
cd polymarket-agent
cargo build --release

# Copy and fill in your API keys
cp .env.example .env
# Edit .env with your keys

# Run a backtest first (no API keys needed)
# Set mode = "backtest" in config/default.toml
cargo run --release

# Run in paper trading mode (default)
# Set mode = "paper" in config/default.toml
cargo run --release
```

## Configuration

### Environment Variables (`.env`)

| Variable | Required | Description |
|----------|----------|-------------|
| `ANTHROPIC_API_KEY` | Yes (paper/live) | Claude API key for market valuation |
| `POLYMARKET_PRIVATE_KEY` | Yes (live) | Ethereum private key for signing orders |
| `DISCORD_WEBHOOK_URL` | No | Discord webhook for trade/status alerts |
| `NOAA_API_TOKEN` | No | NOAA API for weather market data |
| `ESPN_API_KEY` | No | ESPN API for sports market data |
| `RUST_LOG` | No | Log level filter (default: `info`) |

### Config File (`config/default.toml`)

**Agent:**
| Parameter | Default | Description |
|-----------|---------|-------------|
| `mode` | `"paper"` | `paper`, `live`, or `backtest` |
| `cycle_interval_seconds` | `600` | Time between cycles (10 min) |
| `initial_paper_balance` | `100.0` | Starting balance in paper mode |
| `low_fuel_threshold` | `10.0` | Balance to enter LowFuel state |
| `death_balance_threshold` | `0.0` | Balance to enter Dead state |
| `api_reserve` | `2.0` | Reserved balance for API costs |

**Risk:**
| Parameter | Default | Description |
|-----------|---------|-------------|
| `kelly_fraction` | `0.5` | Half-Kelly (fraction of full Kelly) |
| `max_position_pct` | `0.06` | Max 6% of bankroll per position |
| `max_total_exposure_pct` | `0.30` | Max 30% total portfolio exposure |
| `max_positions_per_category` | `3` | Concentration limit per category |
| `min_position_usd` | `1.0` | Minimum trade size |

**Valuation:**
| Parameter | Default | Description |
|-----------|---------|-------------|
| `claude_model` | `"claude-sonnet-4-20250514"` | Claude model for valuations |
| `min_edge_threshold` | `0.08` | Minimum edge to trade (8%) |

## Operating Modes

### Paper Trading (default)

Simulates all trades locally. Orders fill at limit price. No real money, no API keys for Polymarket needed. Uses Claude API for valuations.

```toml
[agent]
mode = "paper"
```

### Backtest

Replays historical or synthetic market data through the full pipeline without any API calls. Place a CSV file at `data/backtest.csv` or the agent generates 500 synthetic snapshots automatically.

```toml
[agent]
mode = "backtest"
```

**CSV format:**
```
timestamp,market_id,question,category,yes_price,no_price,volume_24h,spread,end_date,resolved_outcome
2025-01-01T00:00:00Z,m1,Will BTC hit 100k?,crypto,0.65,0.35,50000,0.03,2025-01-08T00:00:00Z,1.0
```

### Live Trading

Places real orders on Polymarket. Requires a funded wallet and `POLYMARKET_PRIVATE_KEY`.

```toml
[agent]
mode = "live"
```

## Monitoring

### Health Check

While running in paper or live mode, the agent exposes a health endpoint:

```bash
curl http://localhost:9090/health
```

```json
{
  "status": "healthy",
  "agent_state": "Alive",
  "cycle_number": 42,
  "uptime_seconds": 25200
}
```

### Discord Alerts

Enable Discord notifications for trade events, state changes, bankroll milestones, and daily summaries:

```toml
[monitoring]
discord_enabled = true
```

Set `DISCORD_WEBHOOK_URL` in your `.env`.

### Logs

Structured JSON logs via `tracing`:

```bash
# Follow logs
RUST_LOG=info cargo run --release

# Debug level
RUST_LOG=debug cargo run --release
```

## Deployment (VPS)

For a ~$4.50/month Ubuntu VPS:

```bash
# On the VPS
sudo bash deploy/setup.sh

# Edit API keys
sudo nano /opt/polymarket-agent/.env

# Start the service
sudo systemctl start polymarket-agent

# Check status
sudo systemctl status polymarket-agent
sudo journalctl -u polymarket-agent -f
curl http://localhost:9090/health
```

The systemd service includes security hardening (`NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome=true`).

## Testing

```bash
# Run all 100 unit tests
cargo test

# Run with output
cargo test -- --nocapture

# Run a specific module's tests
cargo test risk::kelly
cargo test backtesting::engine
```

## Design Decisions

- **`rust_decimal`** for all monetary values — never `f64` for money
- **Half-Kelly sizing** — full Kelly is too aggressive; half-Kelly balances growth and drawdown
- **State-aware scaling** — position sizes reduce automatically in LowFuel (25%) and freeze in CriticalSurvival
- **Edge-justifies-cost gate** — trades are skipped if projected profit doesn't exceed the API cost to evaluate them
- **Paper trading by default** — the agent never touches real money unless explicitly configured for live mode
- **Official Polymarket SDK** — uses `polymarket-client-sdk` with `alloy` for EIP-712 order signing (not deprecated `ethers`)

## License

Private — not for redistribution.
