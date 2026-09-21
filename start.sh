#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/polymarket-agent" 2>/dev/null || cd "$SCRIPT_DIR"

echo "======================================================="
echo "    Polymarket Autonomous Trading Agent Launcher"
echo "======================================================="
echo ""

if [ ! -f ".env" ]; then
    echo "[WARNING] .env file not found!"
    if [ -f ".env.example" ]; then
        echo "Creating .env from .env.example..."
        cp .env.example .env
        echo "[INFO] Please edit .env with your LLM_API_KEY (or ANTHROPIC_API_KEY) before running."
        echo ""
    fi
fi

MODE="${1:-}"

if [ -z "$MODE" ]; then
    echo "Select operation mode:"
    echo "  [1] Paper Trading Mode (Simulated funds, live market data - Recommended)"
    echo "  [2] Dry Run / Connectivity Validation"
    echo "  [3] Backtest Mode (Simulated historical runs)"
    echo "  [4] Live Trading Mode (REAL FUNDS / Polygon USDC)"
    echo "  [5] Exit"
    echo ""
    read -p "Enter your choice [1-5] (default: 1): " choice
    choice="${choice:-1}"
    case "$choice" in
        1) MODE="paper" ;;
        2) MODE="dry-run" ;;
        3) MODE="backtest" ;;
        4) MODE="live" ;;
        5) exit 0 ;;
        *) MODE="paper" ;;
    esac
fi

case "$MODE" in
    paper)
        echo ""
        echo "======================================================="
        echo " Starting Agent in PAPER TRADING MODE"
        echo " Dashboard: http://127.0.0.1:8080"
        echo "======================================================="
        cargo run --release -- --mode paper
        ;;
    dry-run|dryrun)
        echo ""
        echo "======================================================="
        echo " Running DRY RUN Validation"
        echo "======================================================="
        cargo run -- --dry-run
        ;;
    backtest)
        echo ""
        echo "======================================================="
        echo " Running BACKTEST Mode"
        echo "======================================================="
        cargo run -- --mode backtest
        ;;
    live)
        echo ""
        echo "======================================================="
        echo " WARNING: STARTING LIVE TRADING MODE (REAL FUNDS)"
        echo "======================================================="
        read -p "Are you sure you want to trade with real funds? (y/N): " confirm
        if [[ ! "$confirm" =~ ^[Yy]$ ]]; then
            echo "Live trading cancelled."
            exit 1
        fi
        cargo run --release -- --mode live
        ;;
    *)
        echo "Unknown mode: $MODE"
        exit 1
        ;;
esac
