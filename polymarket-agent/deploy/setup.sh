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
    # The live config lives outside the checkout (see step 7), so the working
    # tree stays clean and this pull never conflicts.
    su - "$AGENT_USER" -c "cd '$INSTALL_DIR' && git pull"
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
CONFIG_FILE="$ENV_DIR/config.toml"
mkdir -p "$ENV_DIR"
if [ ! -f "$ENV_FILE" ]; then
    cat > "$ENV_FILE" << ENVEOF
# Polymarket Agent Environment Variables
# Fill in your actual keys before starting the service

ANTHROPIC_API_KEY=
POLYMARKET_PRIVATE_KEY=
DISCORD_WEBHOOK_URL=
NOAA_API_TOKEN=
ESPN_API_KEY=
# Required if the dashboard is bound to anything other than 127.0.0.1
DASHBOARD_TOKEN=

# Read the tuned config from outside the git checkout so redeploys never
# touch it. Edit $CONFIG_FILE, not the repo's config/default.toml.
CONFIG_PATH=$CONFIG_FILE
ENVEOF
    chown "root:$AGENT_USER" "$ENV_FILE"
    chmod 640 "$ENV_FILE"
    echo "Created env file at $ENV_FILE — fill in your keys!"
fi

# Seed the live config once from the repo default, pointing the database at
# the writable data directory. On later runs it is left alone, so operator
# tuning survives a redeploy and `git pull` never sees a dirty working tree.
if [ ! -f "$CONFIG_FILE" ]; then
    if [ -f "$INSTALL_DIR/config/default.toml" ]; then
        sed "s|^path = \"polymarket-agent.db\"|path = \"$DATA_DIR/polymarket-agent.db\"|" \
            "$INSTALL_DIR/config/default.toml" > "$CONFIG_FILE"
        chown "root:$AGENT_USER" "$CONFIG_FILE"
        chmod 640 "$CONFIG_FILE"
        echo "Created config at $CONFIG_FILE — review before starting!"
    else
        echo "WARNING: $INSTALL_DIR/config/default.toml not found — cannot seed $CONFIG_FILE"
    fi
else
    echo "Config already exists at $CONFIG_FILE — leaving your settings untouched"
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
echo "  2. Review $CONFIG_FILE (the live config — NOT the repo's config/default.toml)"
echo "  3. Validate the setup (loads $ENV_FILE the same way systemd does):"
echo "     sudo -u $AGENT_USER bash -c 'cd $INSTALL_DIR && set -a && source $ENV_FILE && set +a && ./target/release/polymarket-agent --dry-run'"
echo "  4. Run a backtest first:"
echo "     sudo -u $AGENT_USER bash -c 'cd $INSTALL_DIR && set -a && source $ENV_FILE && set +a && ./target/release/polymarket-agent --mode backtest'"
echo "  5. Start in paper mode:"
echo "     sudo systemctl start polymarket-agent"
echo "  6. Check logs:"
echo "     sudo journalctl -u polymarket-agent -f"
echo "  7. Check health:"
echo "     curl http://localhost:8080/api/health"
echo ""
