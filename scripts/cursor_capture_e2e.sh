#!/usr/bin/env bash
# End-to-end Cursor capture visibility check.
# Starts mitmdump, drives a short cursor-agent turn through the proxy, exports,
# and fails unless AgentService/Run bodies are non-empty.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

PORT="${N00N_CURSOR_CAPTURE_PORT:-18080}"
export N00N_CURSOR_CAPTURE_PORT="$PORT"
DUMP_DIR="${ROOT}/spikes/cursor-capture-e2e-$(date +%Y%m%d-%H%M%S)"
MITM_PID=""

cleanup() {
  if [[ -n $MITM_PID ]] && kill -0 "$MITM_PID" 2>/dev/null; then
    kill "$MITM_PID" 2>/dev/null || true
    wait "$MITM_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

die() {
  echo "e2e FAIL: $*" >&2
  exit 1
}

[[ -x .tools/mitm/bin/mitmdump ]] || die "run mise run mitm-setup first"
command -v cursor-agent >/dev/null 2>&1 || command -v agent >/dev/null 2>&1 ||
  die "cursor-agent not on PATH"

echo "=== e2e capture dump=$DUMP_DIR port=$PORT ==="
scripts/cursor_capture.sh "$DUMP_DIR" >"$DUMP_DIR.mitm-stdout.log" 2>&1 &
MITM_PID=$!

# Wait for READY / listen
for _ in $(seq 1 100); do
  if [[ -f "$DUMP_DIR/READY" ]] && rg -q "ready port=" "$DUMP_DIR/READY"; then
    break
  fi
  if ! kill -0 "$MITM_PID" 2>/dev/null; then
    die "mitmdump exited early — see $DUMP_DIR.mitm-stdout.log"
  fi
  sleep 0.1
done
[[ -f "$DUMP_DIR/READY" ]] || die "proxy never became ready"

echo "=== driving proxied cursor-agent ==="
set +e
scripts/cursor_agent_proxied.sh -p --model auto --output-format text \
  'Reply with exactly the single word: pong' \
  >"$DUMP_DIR/agent.stdout.txt" 2>"$DUMP_DIR/agent.stderr.txt"
AGENT_EC=$?
set -e
echo "cursor-agent exit=$AGENT_EC"
# Give mitm a moment to finalize streamed bodies
sleep 2

echo "=== stopping proxy ==="
kill "$MITM_PID" 2>/dev/null || true
wait "$MITM_PID" 2>/dev/null || true
MITM_PID=""

echo "=== live log (tail) ==="
tail -n 40 "$DUMP_DIR/live.log" 2>/dev/null || true

echo "=== export + validate ==="
scripts/cursor_export_flows.sh "$DUMP_DIR"
echo "e2e PASS — artifacts in $DUMP_DIR"
