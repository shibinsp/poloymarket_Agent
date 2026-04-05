# Production Deployment Guide

This guide walks you through deploying the Polymarket Autonomous Trading Agent to a production VPS.

## Prerequisites

- **VPS**: Ubuntu 22.04+ (minimum $4.50/mo — Hetzner, DigitalOcean, Linode)
- **Rust**: 1.88.0+ (installed via rustup)
- **API Keys**: Anthropic (Claude), optionally Polymarket private key for live mode
- **Budget**: $5-72/day for Claude API costs (configurable)

---

## Step 1: Validate Your Setup

Before deploying, run the dry-run validation on your local machine:

```bash
cd polymarket-agent

# Run full validation (config, database, API connectivity, balance)
cargo run -- --dry-run

# Expected output: All systems operational
```

**Checklist:**
- [ ] `cargo build` succeeds
- [ ] `cargo test` passes (145/145)
- [ ] `cargo clippy -- -D warnings` is clean
- [ ] `cargo run -- --dry-run` shows all green checkmarks
- [ ] `ANTHROPIC_API_KEY` is set (required for paper/live)
- [ ] `POLYMARKET_PRIVATE_KEY` is set (required for live mode only)

---

## Step 2: Run Backtest

Validate the full trading pipeline with synthetic data:

```bash
cargo run -- --mode backtest
```

**Expected output:**
```
=== Backtest Results ===
Trades: 333 (333W / 0L, 100.0% win rate)
P&L: $20332.68 total, $20327.68 net (after $5.00 API costs)
ROI: 20327.6% | Sharpe: 2.21
```

**Note:** Synthetic data produces unrealistic win rates. This only validates the pipeline works. Real historical data will give realistic results.

---

## Step 3: Paper Trading (48-72 Hours Minimum)

**DO NOT skip this step.** Paper trading validates the agent with real Claude AI valuations but simulated money.

```bash
# 1. Set up your .env
cp .env.example .env
# Edit .env with your ANTHROPIC_API_KEY

# 2. Ensure config/default.toml has:
# [agent]
# mode = "paper"

# 3. Run the agent
cargo run --release -- --mode paper
```

**Monitor the dashboard:**
```bash
# Open in browser
open http://127.0.0.1:8080

# Or check via CLI
curl http://127.0.0.1:8080/api/health
curl http://127.0.0.1:8080/api/metrics
curl http://127.0.0.1:8080/api/trades
```

**What to watch for:**
- Agent state stays `ALIVE` (balance > $10)
- Trades are being placed (check `/api/trades`)
- API costs stay within daily budget ($5 default)
- No repeated errors in logs

**Minimum duration: 48-72 hours.** Do not go live until you've seen at least 288+ cycles.

---

## Step 4: Deploy to VPS

### 4.1 Server Setup

```bash
# SSH into your VPS
ssh root@your-vps-ip

# Update system
apt update && apt upgrade -y

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env

# Install build dependencies
apt install -y build-essential pkg-config libssl-dev sqlite3
```

### 4.2 Deploy the Agent

```bash
# Clone the repository
git clone https://github.com/shibinsp/poloymarket_Agent.git
cd poloymarket_Agent/polymarket-agent

# Build release binary
cargo build --release

# Set up config
cp .env.example .env
nano .env  # Add your API keys

# Create data directory
mkdir -p /var/lib/polymarket-agent
cp polymarket-agent.db /var/lib/polymarket-agent/
```

### 4.3 Set Up systemd Service

```bash
# Copy service file
sudo cp deploy/polymarket-agent.service /etc/systemd/system/

# Or create manually:
sudo tee /etc/systemd/system/polymarket-agent.service > /dev/null << 'EOF'
[Unit]
Description=Polymarket Autonomous Trading Agent
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root/poloymarket_Agent/polymarket-agent
ExecStart=/root/poloymarket_Agent/polymarket-agent/target/release/polymarket-agent
Restart=on-failure
RestartSec=30
Environment=RUST_LOG=info

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/polymarket-agent

[Install]
WantedBy=multi-user.target
EOF

# Reload systemd
sudo systemctl daemon-reload

# Start the agent
sudo systemctl start polymarket-agent

# Enable auto-start on boot
sudo systemctl enable polymarket-agent

# Check status
sudo systemctl status polymarket-agent

# View logs
sudo journalctl -u polymarket-agent -f
```

### 4.4 Verify Deployment

```bash
# Check health endpoint
curl http://localhost:8080/health

# Expected response:
# {"status":"ok","agent_state":"ALIVE","cycle_number":1,...}

# Check metrics
curl http://localhost:8080/api/metrics

# Check recent trades
curl http://localhost:8080/api/trades
```

---

## Step 5: Monitoring & Alerts

### Discord Alerts (Optional)

Set `DISCORD_WEBHOOK_URL` in `.env` to receive real-time alerts for:
- Trade placed/closed
- Bankroll milestones
- Agent state changes
- Daily performance summaries

### Log Monitoring

```bash
# Follow live logs
sudo journalctl -u polymarket-agent -f

# Search for errors
sudo journalctl -u polymarket-agent --priority=err

# View last 100 lines
sudo journalctl -u polymarket-agent -n 100 --no-pager
```

### Health Monitoring

Set up an external uptime monitor (UptimeRobot, Healthchecks.io) to ping:
```
http://your-vps-ip:8080/health
```

Alert if:
- Response is not 200
- `agent_state` is `DEAD` or `CRITICAL_SURVIVAL`
- No response for 15+ minutes

---

## Step 6: Going Live (Only After Paper Validation)

**Prerequisites:**
- [ ] 48-72+ hours of profitable paper trading
- [ ] No critical errors in logs
- [ ] API costs within budget
- [ ] You understand the risks and can afford to lose the deposit

### 6.1 Fund Your Polymarket Wallet

1. Go to [Polymarket](https://polymarket.com)
2. Connect your wallet
3. Deposit USDC on Polygon network
4. **Start small: $50-100 maximum for initial testing**

### 6.2 Switch to Live Mode

```bash
# Edit .env
nano .env
# Add: POLYMARKET_PRIVATE_KEY=0x...

# Edit config
nano config/default.toml
# Change: mode = "live"

# Restart
sudo systemctl restart polymarket-agent

# Monitor closely
sudo journalctl -u polymarket-agent -f
curl http://localhost:8080/api/health
```

### 6.3 First Week Live

- **Watch every cycle** for the first 24 hours
- **Check positions** on Polymarket web interface daily
- **Verify orders** are being placed correctly
- **Monitor P&L** — expect losses initially
- **Be ready to stop** immediately if something goes wrong

### 6.4 Emergency Stop

```bash
# Stop the agent immediately
sudo systemctl stop polymarket-agent

# Check current state
curl http://localhost:8080/api/health
curl http://localhost:8080/api/trades

# If live trading: manually check and cancel positions on Polymarket web UI
```

---

## Configuration Reference

### Key Settings (`config/default.toml`)

| Setting | Paper Default | Live Recommended | Notes |
|---------|--------------|------------------|-------|
| `mode` | `paper` | `live` | Only change after validation |
| `cycle_interval_seconds` | `600` | `600` | 10 min cycles |
| `initial_paper_balance` | `100.0` | N/A | Paper only |
| `daily_api_budget` | `5.0` | `5.0` | Max API spend/day |
| `min_edge_threshold` | `0.08` | `0.10` | Higher for live = safer |
| `max_position_pct` | `0.06` | `0.03` | Lower for live = safer |
| `max_total_exposure_pct` | `0.30` | `0.15` | Lower for live = safer |

---

## Troubleshooting

### Agent Dies Immediately
- Check balance: `curl http://localhost:8080/api/health`
- If balance = $0, agent is in Dead state — restart with fresh paper balance

### Claude API Errors
- Verify `ANTHROPIC_API_KEY` is correct
- Check daily API budget isn't exhausted
- Check Anthropic status page for outages

### No Trades Being Placed
- Check logs: `sudo journalctl -u polymarket-agent -n 50`
- Markets may not meet edge threshold (8% default)
- Check if valuation engine is enabled (requires API key)

### High API Costs
- Reduce `max_evaluations_per_cycle` in backtest config
- Increase `min_edge_threshold` to filter more markets
- Lower `daily_api_budget` cap

### Database Corruption
```bash
# Stop agent
sudo systemctl stop polymarket-agent

# Backup
cp polymarket-agent.db polymarket-agent.db.bak

# Delete and restart (loses history)
rm polymarket-agent.db
sudo systemctl start polymarket-agent
```

---

## Cost Breakdown

| Item | Monthly Cost |
|------|-------------|
| VPS (Ubuntu, 1 vCPU, 1GB RAM) | $4.50 |
| Claude API (at $5/day budget) | $150.00 |
| **Total** | **~$154.50/month** |

The agent must earn more than $154.50/month in trading profits just to break even.
