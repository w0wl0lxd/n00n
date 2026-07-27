#!/usr/bin/env bash
# Capture Cursor Connect traffic via mitmproxy.
# Setup: mise run mitm-setup
# Usage: scripts/cursor_capture.sh [dump-dir]
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
MITM="${ROOT}/.tools/mitm/bin/mitmdump"
DUMP_DIR="${1:-${ROOT}/spikes/cursor-capture-$(date +%Y%m%d-%H%M%S)}"

if [[ ! -x "$MITM" ]]; then
  echo "mitmdump not found. Run: mise run mitm-setup" >&2
  exit 1
fi

mkdir -p "$DUMP_DIR"
echo "Writing flows to $DUMP_DIR/flows.mitm"
echo "Point cursor-agent at the proxy, e.g.:"
echo "  HTTPS_PROXY=http://127.0.0.1:8080 HTTP_PROXY=http://127.0.0.1:8080 \\"
echo "  NODE_EXTRA_CA_CERTS=~/.mitmproxy/mitmproxy-ca-cert.pem \\"
echo "  cursor-agent -p --model auto 'say hi'"
exec "$MITM" -p 8080 -w "$DUMP_DIR/flows.mitm" --set stream_large_bodies=1
