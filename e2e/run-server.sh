#!/usr/bin/env bash
# Runs the server for the E2E. The server vendors and serves /pt.js itself
# (server/assets/pt.js, compiled into the binary), so no client build is needed.
# Used as the Playwright webServer command; also runnable standalone.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

exec env \
  DATABASE_URL="${DATABASE_URL:-postgres://fole@localhost/dullahan_e2e}" \
  ADMIN_TOKEN="${ADMIN_TOKEN:-e2e-token}" \
  BIND_ADDR="${BIND_ADDR:-127.0.0.1:3099}" \
  cargo run --manifest-path "$ROOT/server/Cargo.toml"
