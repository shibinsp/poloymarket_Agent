#!/usr/bin/env bash
# Polymarket Agent VPS Setup Script
# Target: Ubuntu 22.04+ (~$4.50/month VPS)
# Usage: sudo bash setup.sh

set -euo pipefail

AGENT_USER="agent"
INSTALL_DIR="/opt/polymarket-agent"
DATA_DIR="/var/lib/polymarket-agent"

echo "=== Polymarket Agent Setup ==="

# 1. System updates
echo "Step 1: Updating system packages..."
apt-get update -y
apt-get upgrade -y
apt-get install -y build-essential pkg-config libssl-dev curl git

# 2. Create agent user (non-root)
echo "Step 2: Creating agent user..."
if ! id "$AGENT_USER" &>/dev/null; then
    useradd --system --create-home --shell /bin/bash "$AGENT_USER"
    echo "Created user: $AGENT_USER"
else
    echo "User $AGENT_USER already exists"
fi

# 3. Install Rust toolchain
echo "Step 3: Installing Rust toolchain..."
if ! command -v rustup &>/dev/null; then
    su - "$AGENT_USER" -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y'
else
    su - "$AGENT_USER" -c 'rustup update'
fi

# 4. Create directories
echo "Step 4: Creating directories..."
mkdir -p "$INSTALL_DIR"
mkdir -p "$DATA_DIR"
chown "$AGENT_USER:$AGENT_USER" "$INSTALL_DIR"
chown "$AGENT_USER:$AGENT_USER" "$DATA_DIR"

# 5. Clone/update repository
echo "Step 5: Setting up codebase..."
if [ -d "$INSTALL_DIR/.git" ]; then
    echo "Repository exists — pulling latest..."
    su - "$AGENT_USER" -c "cd $INSTALL_DIR && git pull"
else
    echo "Clone your repository to $INSTALL_DIR"
    echo "  git clone <your-repo-url> $INSTALL_DIR"
    echo "  chown -R $AGENT_USER:$AGENT_USER $INSTALL_DIR"
fi

# 6. Build release binary
echo "Step 6: Building release binary..."
if [ -f "$INSTALL_DIR/Cargo.toml" ]; then
    su - "$AGENT_USER" -c "cd $INSTALL_DIR && \$HOME/.cargo/bin/cargo build --release"
    echo "Build successful!"
else
    echo "WARNING: Cargo.toml not found at $INSTALL_DIR — skipping build"
fi

# 7. Setup config
# Secrets live outside the (read-only under ProtectSystem=strict) install dir,
# readable only by root and the agent user.
echo "Step 7: Setting up configuration..."
ENV_DIR="/etc/polymarket-agent"
ENV_FILE="$ENV_DIR/env"
mkdir -p "$ENV_DIR"
if [ ! -f "$ENV_FILE" ]; then
    cat > "$ENV_FILE" << 'ENVEOF'
# Polymarket Agent Environment Variables
# Fill in your actual keys before starting the service

ANTHROPIC_API_KEY=
POLYMARKET_PRIVATE_KEY=
DISCORD_WEBHOOK_URL=
NOAA_API_TOKEN=
ESPN_API_KEY=
# Required if the dashboard is bound to anything other than 127.0.0.1
DASHBOARD_TOKEN=
ENVEOF
    chown "root:$AGENT_USER" "$ENV_FILE"
    chmod 640 "$ENV_FILE"
    echo "Created env file at $ENV_FILE — fill in your keys!"
fi

# Point the database at the writable data directory
if [ -f "$INSTALL_DIR/config/default.toml" ]; then
    sed -i "s|^path = \"polymarket-agent.db\"|path = \"$DATA_DIR/polymarket-agent.db\"|" "$INSTALL_DIR/config/default.toml"
fi

# 8. Install systemd service
echo "Step 8: Installing systemd service..."
cp "$INSTALL_DIR/deploy/polymarket-agent.service" /etc/systemd/system/
systemctl daemon-reload
systemctl enable polymarket-agent

echo ""
echo "=== Setup Complete ==="
echo ""
echo "Next steps:"
echo "  1. Edit $ENV_FILE with your API keys"
echo "  2. Review $INSTALL_DIR/config/default.toml"
echo "  3. Validate the setup:"
echo "     sudo -u $AGENT_USER $INSTALL_DIR/target/release/polymarket-agent --dry-run"
echo "  4. Run a backtest first:"
echo "     sudo -u $AGENT_USER $INSTALL_DIR/target/release/polymarket-agent --mode backtest"
echo "  5. Start in paper mode:"
echo "     sudo systemctl start polymarket-agent"
echo "  6. Check logs:"
echo "     sudo journalctl -u polymarket-agent -f"
echo "  7. Check health:"
echo "     curl http://localhost:8080/api/health"
echo ""
