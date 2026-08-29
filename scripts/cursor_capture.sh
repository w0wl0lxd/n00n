#!/usr/bin/env bash
# Capture Cursor Connect traffic via mitmproxy with streamed-body retention.
#
# Setup (once):  mise run mitm-setup
# Capture:       scripts/cursor_capture.sh
# Proxied agent: scripts/cursor_agent_proxied.sh -p --model auto 'say hi'
# E2E check:     scripts/cursor_capture_e2e.sh
# Export:        scripts/cursor_export_flows.sh spikes/cursor-capture-<stamp>
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
MITM="${ROOT}/.tools/mitm/bin/mitmdump"
ADDON="${ROOT}/scripts/cursor_capture_addon.py"
PORT="${N00N_CURSOR_CAPTURE_PORT:-8080}"
DUMP_DIR="${1:-${ROOT}/spikes/cursor-capture-$(date +%Y%m%d-%H%M%S)}"

die() {
  echo "error: $*" >&2
  exit 1
}

[[ -x $MITM ]] || die "mitmdump not found — run: mise run mitm-setup"
[[ -f $ADDON ]] || die "missing addon: $ADDON"

if command -v ss >/dev/null 2>&1; then
  if ss -ltn "( sport = :$PORT )" 2>/dev/null | rg -q ":$PORT"; then
    die "port $PORT already in use — set N00N_CURSOR_CAPTURE_PORT or stop the other listener"
  fi
fi

mkdir -p "$DUMP_DIR/bodies"
FLOW_FILE="$DUMP_DIR/flows.mitm"
READY_FILE="$DUMP_DIR/READY"
rm -f "$READY_FILE"
: >"$DUMP_DIR/live.log"

cat >"$DUMP_DIR/README.txt" <<EOF
Cursor capture dump
===================
Started: $(date -u +%Y-%m-%dT%H:%M:%SZ)
Port:    $PORT
Flows:   $FLOW_FILE
Bodies:  $DUMP_DIR/bodies/  (written live as flows complete)
Live:    $DUMP_DIR/live.log
Summary: $DUMP_DIR/summary.tsv

Run agent via:
  scripts/cursor_agent_proxied.sh -p --model auto 'Reply with exactly: pong'

Then (optional second turn for checkpoints):
  scripts/cursor_agent_proxied.sh -p --resume --model auto 'what did I ask?'

Validate:
  scripts/cursor_export_flows.sh $DUMP_DIR
  # or full e2e: scripts/cursor_capture_e2e.sh
EOF

cat <<EOF
════════════════════════════════════════════════════════════
 Cursor capture listening on :$PORT
 Dump: $DUMP_DIR
════════════════════════════════════════════════════════════
In another terminal:

  scripts/cursor_agent_proxied.sh -p --model auto 'Reply with exactly: pong'

Watch live:
  tail -f $DUMP_DIR/live.log

You want a line like:  saved … AgentService_Run … req=<nonzero> resp=<nonzero> ★RUN
Ctrl-C here when done.
════════════════════════════════════════════════════════════
EOF

export N00N_CURSOR_CAPTURE_DIR="$DUMP_DIR"
# Touch READY after mitmdump binds — approximate with a background waiter.
(
  for _ in $(seq 1 50); do
    if ss -ltn "( sport = :$PORT )" 2>/dev/null | rg -q ":$PORT"; then
      echo "ready port=$PORT" >"$READY_FILE"
      echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) proxy ready on :$PORT" >>"$DUMP_DIR/live.log"
      exit 0
    fi
    sleep 0.1
  done
  echo "ready-timeout" >"$READY_FILE"
) &

# CRITICAL: stream_large_bodies=1 so AgentService/Run is not held until close,
# AND store_streamed_bodies=true so those bytes are retained for export.
# Without BOTH, you get either hung chats or empty bodies.
exec "$MITM" \
  -p "$PORT" \
  -w "$FLOW_FILE" \
  -s "$ADDON" \
  --set stream_large_bodies=1 \
  --set store_streamed_bodies=true \
  --set connection_strategy=lazy \
  --set http2=true \
  --set flow_detail=1 \
  --set termlog_verbosity=info
