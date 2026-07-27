#!/usr/bin/env bash
# Capture Cursor Connect traffic via mitmproxy with request/response bodies.
#
# Setup (once):
#   cd /home/w0w/dev/n00n && mise run mitm-setup
#
# Terminal A:
#   scripts/cursor_capture.sh
#
# Terminal B (cursor-agent ignores NODE_EXTRA_CA_CERTS; disable TLS verify for capture):
#   export GLOBAL_AGENT_HTTP_PROXY=http://127.0.0.1:8080
#   export HTTPS_PROXY=http://127.0.0.1:8080
#   export HTTP_PROXY=http://127.0.0.1:8080
#   export NODE_TLS_REJECT_UNAUTHORIZED=0
#   cursor-agent -p --model auto 'say hi'
#   # then a follow-up in the same session for checkpoint blobs:
#   cursor-agent -p --resume --model auto 'what did I just ask?'
#
# After Ctrl-C on Terminal A, export bodies:
#   scripts/cursor_export_flows.sh spikes/cursor-capture-<stamp>
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
MITM="${ROOT}/.tools/mitm/bin/mitmdump"
DUMP_DIR="${1:-${ROOT}/spikes/cursor-capture-$(date +%Y%m%d-%H%M%S)}"

if [[ ! -x "$MITM" ]]; then
  echo "mitmdump not found. Run: mise run mitm-setup" >&2
  exit 1
fi

mkdir -p "$DUMP_DIR"
FLOW_FILE="$DUMP_DIR/flows.mitm"

cat <<EOF
Writing flows to $FLOW_FILE
Bodies are stored (needed for AgentService/Run protobuf RE).

In another terminal, from $ROOT:

  export GLOBAL_AGENT_HTTP_PROXY=http://127.0.0.1:8080
  export HTTPS_PROXY=http://127.0.0.1:8080
  export HTTP_PROXY=http://127.0.0.1:8080
  export NODE_TLS_REJECT_UNAUTHORIZED=0
  cursor-agent -p --model auto 'Reply with exactly: pong'

Expect console noise for localhost probes; ignore those.
Ctrl-C here when done, then:

  scripts/cursor_export_flows.sh $DUMP_DIR
EOF

# Do NOT enable stream_large_bodies without store — that produced "(content missing)".
# Filter view to cursor hosts; still record everything for safety.
exec "$MITM" \
  -p 8080 \
  -w "$FLOW_FILE" \
  --set store_streamed_bodies=true \
  --set connection_strategy=lazy \
  --set flow_detail=1 \
  --view-filter '~d cursor\.sh'
