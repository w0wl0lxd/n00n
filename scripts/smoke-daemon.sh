#!/usr/bin/env bash
# Smoke gate for n00n-daemon / agent control (spec 005).
# Replaces the manual verification checklist with automated tests.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "== smoke: n00n-daemon unit + integration =="
cargo test -p n00n-daemon

echo "== smoke: TUI bridge daemon registration =="
cargo test -p n00n --bins -- tui_bridge

echo "== smoke: agent CLI helpers =="
cargo test -p n00n --bins -- agent::

echo "== smoke: clippy (daemon + binary) =="
cargo clippy -p n00n-daemon -p n00n --tests -- -D warnings

echo "== smoke: CLI list via temp state dir =="
SMOKE_DIR="$(mktemp -d)"
trap 'rm -rf "$SMOKE_DIR"' EXIT
mkdir -p "$SMOKE_DIR/agents/demo"
cat >"$SMOKE_DIR/agents/demo/agent.json" <<EOF
{"id":"demo","session_id":"s1","socket_path":"/tmp/n00n-smoke-missing.sock","pid":1,"status":"running","prompt":"p","model":"test/m","created_at":1,"updated_at":1}
EOF
OUT="$(cargo run -q -p n00n --bin n00n -- agent list --state-dir "$SMOKE_DIR")"
echo "$OUT" | grep -q 'demo'
echo "$OUT" | grep -q 'Background agents'

JSON_OUT="$(cargo run -q -p n00n --bin n00n -- agent list --json --state-dir "$SMOKE_DIR")"
echo "$JSON_OUT" | grep -q '"state"'
echo "$JSON_OUT" | grep -q '"id": "demo"'

echo "ok: daemon smoke gate passed"
