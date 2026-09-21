#!/usr/bin/env bash
# Run the agent end to end against dev/mock_venue.py. No credentials needed.
#
#   ./dev/run-local.sh          # starts the mock, then the agent
#
# Ctrl-C stops both. The dashboard is at http://127.0.0.1:8080.
set -euo pipefail
cd "$(dirname "$0")/.."

PORT=18800
if curl -sf -m 1 -o /dev/null "http://127.0.0.1:$PORT/v2/account" 2>/dev/null; then
  echo "mock already listening on $PORT — reusing it"
  MOCK_PID=""
else
  python3 dev/mock_venue.py &
  MOCK_PID=$!
  # The agent's first call fails outright if the mock is not accepting yet,
  # and a failed venue is skipped for the whole run rather than retried.
  for _ in $(seq 1 30); do
    curl -sf -m 1 -o /dev/null "http://127.0.0.1:$PORT/v2/account" && break
    sleep 0.2
  done
  echo "mock started (pid $MOCK_PID)"
fi

cleanup() { [ -n "$MOCK_PID" ] && kill "$MOCK_PID" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

# Credentials are arbitrary: the mock does not check them, but the agent
# refuses to build a venue whose keys are unset, so they have to be present.
CONFIG_PATH=config/local-mock.toml \
LLM_API_KEY=mock \
ALPACA_API_KEY_ID=key \
ALPACA_API_SECRET_KEY=secret \
RUST_LOG="${RUST_LOG:-info}" \
  cargo run --quiet -- "$@"
