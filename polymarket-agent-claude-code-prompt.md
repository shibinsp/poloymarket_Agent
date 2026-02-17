# Claude Code Prompt: Autonomous Polymarket Trading Agent

## Project Overview

Build a self-sustaining autonomous trading agent in **Rust** that trades on **Polymarket** prediction markets. The agent scans markets, estimates fair value using Claude API, identifies mispriced contracts, sizes positions using Kelly Criterion, executes trades, and pays its own API bills from profits. If balance hits $0, the agent shuts down ("dies").

---

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
│  │ (NOAA)   │ │ (injury) │ │ (on-chain+sent.) │    │
│  └──────────┘ └──────────┘ └──────────────────┘    │
├──────────────────────────────────────────────────────┤
│              VALUATION ENGINE                        │
│  Claude API → Fair Value Estimate → Edge Calc        │
├──────────────────────────────────────────────────────┤
│              POSITION SIZING (Kelly Criterion)       │
│  Max 6% bankroll per position                        │
├──────────────────────────────────────────────────────┤
│              EXECUTION ENGINE                        │
│  Polymarket CLOB API → Order placement & tracking    │
└──────────────────────────────────────────────────────┘
```

---

## Step-by-Step Build Instructions

### PHASE 1: Project Scaffolding & Core Infrastructure

```
Create a new Rust project called `polymarket-agent` with the following structure:

polymarket-agent/
├── Cargo.toml
├── .env.example
├── config/
│   └── default.toml          # All tunable parameters
├── src/
│   ├── main.rs               # Entry point, scheduler loop
│   ├── config.rs             # Configuration loading
│   ├── agent/
│   │   ├── mod.rs
│   │   ├── lifecycle.rs      # Start, heartbeat, death logic
│   │   └── self_funding.rs   # API cost tracking, survival logic
│   ├── market/
│   │   ├── mod.rs
│   │   ├── scanner.rs        # Market discovery & filtering
│   │   ├── polymarket.rs     # Polymarket CLOB API client
│   │   └── models.rs         # Market data structures
│   ├── data/
│   │   ├── mod.rs
│   │   ├── weather.rs        # NOAA data parser
│   │   ├── sports.rs         # Injury reports, odds scraping
│   │   ├── crypto.rs         # On-chain metrics, sentiment
│   │   └── news.rs           # General news/event scraper
│   ├── valuation/
│   │   ├── mod.rs
│   │   ├── claude.rs         # Claude API integration for reasoning
│   │   ├── fair_value.rs     # Fair value estimation pipeline
│   │   └── edge.rs           # Edge calculation (fair vs market)
│   ├── risk/
│   │   ├── mod.rs
│   │   ├── kelly.rs          # Kelly Criterion calculator
│   │   ├── portfolio.rs      # Portfolio state & constraints
│   │   └── limits.rs         # Risk limits, max exposure, drawdown
│   ├── execution/
│   │   ├── mod.rs
│   │   ├── order.rs          # Order building & submission
│   │   ├── fills.rs          # Fill tracking & reconciliation
│   │   └── wallet.rs         # Wallet/balance management
│   ├── monitoring/
│   │   ├── mod.rs
│   │   ├── metrics.rs        # P&L, win rate, edge tracking
│   │   ├── logger.rs         # Structured logging
│   │   └── alerts.rs         # Discord/Telegram alerts
│   └── db/
│       ├── mod.rs
│       └── store.rs          # SQLite for trade history & state
├── migrations/
│   └── 001_init.sql
└── tests/
    ├── backtesting.rs
    ├── kelly_tests.rs
    └── integration.rs
```

**Dependencies (Cargo.toml):**
- `tokio` (async runtime, full features)
- `reqwest` (HTTP client with rustls)
- `serde`, `serde_json` (serialization)
- `ethers` (Ethereum/Polygon wallet operations)
- `sqlx` (SQLite async, for trade log)
- `tracing`, `tracing-subscriber` (structured logging)
- `chrono` (timestamps)
- `toml` (config parsing)
- `dotenv` (env vars)
- `rust_decimal` (precise financial math — NEVER use f64 for money)
- `tokio-cron-scheduler` (10-minute loop)
- `hmac`, `sha2` (API auth signing)
- `thiserror`, `anyhow` (error handling)

**CRITICAL**: Use `rust_decimal::Decimal` for ALL monetary values. Never use floating point for money.
```

---

### PHASE 2: Polymarket CLOB API Client

```
Build the Polymarket integration layer.

Polymarket uses a Central Limit Order Book (CLOB) on Polygon.
API Base: https://clob.polymarket.com

Implement these endpoints:

1. **Market Discovery**
   GET /markets — list all active markets
   - Parse: condition_id, question, outcomes, tokens, end_date
   - Filter: active markets, sufficient liquidity (>$5k volume), resolves within 30 days

2. **Order Book**
   GET /book?token_id={id} — get current order book
   - Parse: bids, asks, spread, midpoint
   - Calculate: implied probability from midpoint price

3. **Price History**
   GET /prices-history?market={id}&interval=1h — price movement
   - Track: momentum, volatility, recent price action

4. **Order Placement**
   POST /order — place limit orders
   - Build: signed order with EIP-712 signature
   - Use Polygon wallet (ethers-rs) for signing
   - Order types: GTC limit orders (never market orders — protect against slippage)

5. **Position Management**
   GET /positions — current positions
   GET /balance — available USDC balance

**Authentication:**
- API Key + Secret
- L1 signing (Ethereum wallet) for order placement
- L2 signing (CLOB-specific) for order operations
- Implement HMAC-SHA256 request signing

**Rate Limiting:**
- Implement token bucket rate limiter
- Respect Polymarket's limits (track via response headers)
- Add exponential backoff on 429s

**Error Handling:**
- Retry transient failures (network, 5xx) with exponential backoff
- Log and alert on auth failures
- Never retry on insufficient balance (trigger survival check instead)
```

---

### PHASE 3: Data Ingestion Pipelines

```
Build specialized data scrapers for each market category.
Each scraper implements trait `DataSource`:

pub trait DataSource: Send + Sync {
    async fn fetch(&self) -> Result<Vec<DataPoint>>;
    fn category(&self) -> MarketCategory;
    fn freshness_window(&self) -> Duration;
}

### 3a. Weather Markets (NOAA)
- Fetch from: https://api.weather.gov
- Parse: forecasts, historical temp data, precipitation probability
- Markets: "Will temperature in X exceed Y?", "Will hurricane make landfall?"
- Edge source: NOAA updates faster than Polymarket participants react
- Cache forecasts, detect forecast *changes* (these create edge)

### 3b. Sports Markets
- Sources:
  - ESPN API for schedules, live scores
  - Injury report feeds (NBA, NFL official injury reports)
  - Odds comparison sites (for reference fair values)
- Track: late scratches, lineup changes, weather impacts on games
- Edge source: injury report drops → scrape within seconds → trade before market adjusts

### 3c. Crypto Markets
- On-chain: Etherscan/Polygonscan API for whale movements, TVL changes
- Sentiment: Aggregate from crypto-specific feeds
- Metrics: funding rates, open interest, exchange flows
- Markets: "Will BTC exceed $X by date Y?"

### 3d. Political/General News
- News API or RSS feed aggregation
- Focus on events with binary outcomes that map to Polymarket questions
- Detect breaking news before market repricing

**Data Normalization:**
All data sources output a standardized `DataPoint` struct:
- source: String
- category: MarketCategory
- timestamp: DateTime<Utc>
- payload: serde_json::Value (flexible schema)
- confidence: Decimal (self-assessed data quality 0-1)
- relevance_to: Vec<MarketId> (which markets this data informs)
```

---

### PHASE 4: Claude-Powered Valuation Engine

```
This is the brain of the agent. Claude estimates fair value probabilities.

### 4a. Claude API Client
- Endpoint: https://api.anthropic.com/v1/messages
- Model: claude-sonnet-4-20250514 (best cost/performance ratio for high-frequency calls)
- Track EVERY API call cost:
  - Input tokens × price per token
  - Output tokens × price per token
  - Running total stored in DB
  - Deduct from bankroll in real-time

### 4b. Fair Value Estimation Prompt

For each candidate market, send Claude a structured prompt:

SYSTEM PROMPT:
"You are a prediction market analyst. Given market data and external signals,
estimate the true probability of the outcome. You must respond with ONLY
valid JSON. No explanations outside the JSON structure.

Your response MUST follow this exact schema:
{
  "probability": <float 0.0-1.0>,
  "confidence": <float 0.0-1.0>,
  "reasoning_summary": "<1-2 sentences>",
  "key_factors": ["<factor1>", "<factor2>"],
  "data_quality": "<high|medium|low>",
  "time_sensitivity": "<hours|days|weeks>"
}"

USER PROMPT (constructed per market):
"Market: {market_question}
Current Price: {price} (implied prob: {implied_prob}%)
Resolution Date: {end_date}
Category: {category}

External Data:
{formatted_data_points}

Historical Price (24h): {price_history}
Volume (24h): {volume}
Order Book Depth: {depth}

Estimate the TRUE probability of YES outcome."

### 4c. Edge Calculation
- edge = |claude_fair_value - market_implied_prob|
- Only trade if edge > 8% (configurable threshold)
- Adjust edge threshold by confidence:
  - High confidence: edge > 6% 
  - Medium confidence: edge > 10%
  - Low confidence: skip

### 4d. Cost Optimization
- Batch similar markets to reduce API calls
- Cache valuations for markets with no new data
- Use shorter prompts for quick re-evaluations
- Kill expensive re-evaluations if remaining bankroll < $10
- Track cost-per-trade and ensure it's < expected profit
```

---

### PHASE 5: Kelly Criterion Position Sizing

```
Implement fractional Kelly Criterion for position sizing.

### Formula
kelly_fraction = (p * b - q) / b

Where:
- p = estimated win probability (Claude's fair value)
- q = 1 - p
- b = odds offered (net payout ratio from market price)

### Implementation Details

pub fn kelly_size(
    fair_prob: Decimal,      // Claude's estimate
    market_price: Decimal,   // Current contract price
    confidence: Decimal,     // Claude's confidence
    bankroll: Decimal,       // Current total balance
) -> Decimal {
    let b = (Decimal::ONE / market_price) - Decimal::ONE;  // net odds
    let p = fair_prob;
    let q = Decimal::ONE - p;
    let kelly = (p * b - q) / b;
    
    // Use fractional Kelly (half-Kelly for safety)
    let fraction = Decimal::new(50, 2); // 0.50 = half Kelly
    let adjusted = kelly * fraction * confidence;
    
    // Hard caps
    let max_position = bankroll * Decimal::new(6, 2);  // 6% max
    let min_position = Decimal::new(1, 0);              // $1 minimum
    
    adjusted.max(Decimal::ZERO).min(max_position).max(min_position)
}

### Portfolio Constraints
- Max 6% of bankroll per position (hard cap)
- Max 30% total exposure across all positions
- Max 3 positions in same category
- No position in markets resolving > 14 days out (capital efficiency)
- No position if order book spread > 5% (illiquidity protection)
- Reduce size in correlated markets

### Dynamic Adjustment
- If bankroll < $20: reduce to quarter-Kelly, only highest-edge trades
- If bankroll < $10: survival mode — no new trades, monitor existing
- If bankroll < API cost of next cycle: AGENT DEATH
```

---

### PHASE 6: Execution Engine

```
### Order Strategy
- ALWAYS use limit orders, NEVER market orders
- Place limit at midpoint or better (favorable side of spread)
- Time-in-force: GTC with 5-minute expiry (cancel and re-evaluate)
- If order doesn't fill within 2 cycles: cancel, re-evaluate edge

### Execution Flow
1. Calculate target position size from Kelly
2. Check current positions (avoid doubling up)
3. Build limit order at favorable price
4. Sign with Polygon wallet (ethers-rs)
5. Submit to CLOB API
6. Poll for fill status
7. Log fill details to SQLite
8. Update portfolio state

### Slippage Protection
- Check order book depth before placing
- If position size > 20% of available liquidity at price level: split into smaller orders
- Max slippage tolerance: 2% from midpoint

### Exit Strategy
- Markets auto-resolve (binary outcomes) — no explicit exit needed
- BUT implement early exit if:
  - Edge has disappeared (Claude re-evaluates < 3% edge)
  - Market liquidity collapses
  - Position is profitable > 50% of max payout (take profit)
```

---

### PHASE 7: Self-Funding & Survival Logic

```
This is what makes the agent "alive" — it must pay for itself.

### Cost Tracking
Track every cost in real-time:
- Claude API calls (input + output tokens × price)
- Polymarket gas fees (Polygon — minimal but tracked)
- VPS cost (amortized: $4.5/month ÷ 30 ÷ 144 = ~$0.001 per 10-min cycle)

### Survival Check (every cycle)
fn survival_check(&self) -> AgentState {
    let balance = self.get_balance();
    let api_cost_next_cycle = self.estimate_next_cycle_cost();
    let unrealized_pnl = self.get_unrealized_pnl();
    
    if balance + unrealized_pnl <= Decimal::ZERO {
        return AgentState::Dead;
    }
    if balance < api_cost_next_cycle {
        return AgentState::CriticalSurvival;
    }
    if balance < Decimal::new(10, 0) {
        return AgentState::LowFuel;
    }
    AgentState::Alive
}

### Behavioral Modes
- **Alive**: Normal operation — scan, evaluate, trade
- **LowFuel**: Reduce scan scope (top 50 markets only), quarter-Kelly sizing
- **CriticalSurvival**: No new trades, monitor existing positions only
- **Dead**: Log final state, send death alert, shutdown gracefully

### API Bill Payment
- Maintain a reserved balance for API costs (min $2 reserved)
- Deduct API costs from tradeable bankroll in real-time
- If projected API cost > projected edge for a trade: SKIP the trade
```

---

### PHASE 8: Monitoring, Logging & Alerts

```
### Structured Logging (tracing crate)
Every action logged with:
- timestamp, cycle_number, action_type
- market_id, edge, position_size, fill_price
- bankroll_before, bankroll_after
- api_cost_this_cycle, cumulative_api_cost
- p&l (realized + unrealized)

### SQLite Schema (migrations/001_init.sql)

CREATE TABLE trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cycle INTEGER NOT NULL,
    market_id TEXT NOT NULL,
    market_question TEXT,
    direction TEXT NOT NULL,  -- 'YES' or 'NO'
    entry_price DECIMAL NOT NULL,
    size DECIMAL NOT NULL,
    edge_at_entry DECIMAL NOT NULL,
    claude_fair_value DECIMAL NOT NULL,
    confidence DECIMAL NOT NULL,
    kelly_raw DECIMAL NOT NULL,
    kelly_adjusted DECIMAL NOT NULL,
    status TEXT DEFAULT 'OPEN',  -- OPEN, FILLED, RESOLVED_WIN, RESOLVED_LOSS, CANCELLED
    pnl DECIMAL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    resolved_at TIMESTAMP
);

CREATE TABLE cycles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cycle_number INTEGER NOT NULL,
    markets_scanned INTEGER,
    opportunities_found INTEGER,
    trades_placed INTEGER,
    api_cost DECIMAL,
    bankroll DECIMAL,
    unrealized_pnl DECIMAL,
    agent_state TEXT,
    duration_ms INTEGER,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE api_costs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider TEXT NOT NULL,  -- 'anthropic', 'polygon', 'noaa'
    input_tokens INTEGER,
    output_tokens INTEGER,
    cost DECIMAL NOT NULL,
    cycle INTEGER,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

### Alerts (Discord webhook or Telegram bot)
- New trade placed (market, size, edge)
- Trade resolved (win/loss, P&L)
- Bankroll milestones ($100, $500, $1000, etc.)
- Agent state changes (especially LowFuel, Critical, Dead)
- Daily summary: P&L, win rate, Sharpe ratio, total API cost
```

---

### PHASE 9: Backtesting & Paper Trading

```
BEFORE running with real money, build:

### Backtester
- Replay historical Polymarket data
- Simulate: market scan → Claude valuation → Kelly sizing → execution at historical prices
- Track: simulated P&L, max drawdown, win rate, edge accuracy
- Run at least 500 simulated trades before going live

### Paper Trading Mode
- Same pipeline, but orders are simulated (not submitted)
- Track what WOULD have happened
- Run for 48-72 hours before committing real capital
- Compare paper results to live to detect execution issues

### Config Flag
[mode]
mode = "paper"  # Options: "paper", "live", "backtest"
```

---

### PHASE 10: Deployment

```
### VPS Setup (Ubuntu 22.04, ~$4.50/month)
- Install Rust toolchain
- Clone repo, build with: cargo build --release
- Run as systemd service with auto-restart
- SQLite DB at /var/lib/polymarket-agent/trades.db

### systemd Service File
[Unit]
Description=Polymarket Trading Agent
After=network.target

[Service]
Type=simple
User=agent
WorkingDirectory=/opt/polymarket-agent
ExecStart=/opt/polymarket-agent/target/release/polymarket-agent
Restart=on-failure
RestartSec=30
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target

### Environment Variables (.env)
ANTHROPIC_API_KEY=sk-ant-...
POLYMARKET_API_KEY=...
POLYMARKET_API_SECRET=...
POLYGON_PRIVATE_KEY=...    # Wallet for signing orders
DISCORD_WEBHOOK_URL=...     # Alerts
NOAA_API_TOKEN=...
ESPN_API_KEY=...

### Health Checks
- Heartbeat endpoint (tiny HTTP server on localhost:9090/health)
- External uptime monitor (UptimeRobot free tier)
- Auto-restart on crash (systemd handles this)
```

---

## Configuration File (config/default.toml)

```toml
[agent]
mode = "paper"                    # paper | live | backtest
cycle_interval_seconds = 600      # 10 minutes
death_balance_threshold = 0.0
low_fuel_threshold = 10.0
api_reserve = 2.0

[scanning]
max_markets = 1000
min_volume_24h = 5000.0
max_resolution_days = 14
categories = ["weather", "sports", "crypto", "politics"]

[valuation]
claude_model = "claude-sonnet-4-20250514"
min_edge_threshold = 0.08         # 8%
high_confidence_edge = 0.06       # 6% for high confidence
low_confidence_edge = 0.10        # 10% for low confidence
cache_ttl_seconds = 300

[risk]
kelly_fraction = 0.5              # Half-Kelly
max_position_pct = 0.06           # 6% of bankroll
max_total_exposure_pct = 0.30     # 30% of bankroll
max_positions_per_category = 3
max_spread_pct = 0.05             # 5% spread limit
min_position_usd = 1.0

[execution]
order_type = "limit"
order_ttl_seconds = 300           # 5-minute expiry
max_slippage_pct = 0.02
max_retries = 3

[monitoring]
log_level = "info"
discord_enabled = true
daily_summary_hour = 9            # 9 AM UTC
```

---

## Critical Risk Warnings

1. **This is NOT guaranteed profit.** Prediction markets are zero-sum minus fees. Edge decays as markets become efficient.
2. **Regulatory risk.** Polymarket access varies by jurisdiction. DYOR on legal compliance.
3. **API dependency.** Claude API outages = blind agent. Build fallback logic.
4. **Liquidity risk.** Thin books mean large positions can't exit cleanly.
5. **Model risk.** Claude can be confidently wrong. The confidence score is self-assessed, not calibrated.
6. **Black swan risk.** A single unexpected event can wipe correlated positions.
7. **Past performance ≠ future results.** $50 → $2,980 is likely survivorship bias or extremely lucky variance.

---

## Getting Started with Claude Code

Paste this entire document into Claude Code and say:

> "Build this project phase by phase. Start with Phase 1 (scaffolding) and Phase 2 (Polymarket client). After each phase, verify it compiles and write unit tests before moving to the next phase. Use `cargo clippy` and `cargo test` after every phase."

Then iterate through phases 3-10, testing each before proceeding.
