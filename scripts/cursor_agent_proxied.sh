#!/usr/bin/env bash
# Run cursor-agent through the local mitmproxy capture proxy with the env
# vars Cursor actually honors (GLOBAL_AGENT_* + TLS verify disabled).
set -euo pipefail

PORT="${N00N_CURSOR_CAPTURE_PORT:-8080}"
PROXY="http://127.0.0.1:${PORT}"

if ! command -v cursor-agent >/dev/null 2>&1 && ! command -v agent >/dev/null 2>&1; then
  echo "error: cursor-agent not on PATH" >&2
  exit 1
fi

AGENT=(cursor-agent)
command -v cursor-agent >/dev/null 2>&1 || AGENT=(agent)

# Footguns avoided here:
# - NODE_EXTRA_CA_CERTS is ignored by cursor-agent's TLS stack
# - HTTPS_PROXY alone is insufficient; GLOBAL_AGENT_HTTP_PROXY is required
# - TLS MITM needs verify disabled for this capture path
export GLOBAL_AGENT_HTTP_PROXY="$PROXY"
export GLOBAL_AGENT_HTTPS_PROXY="$PROXY"
export HTTP_PROXY="$PROXY"
export HTTPS_PROXY="$PROXY"
export ALL_PROXY="$PROXY"
export NODE_TLS_REJECT_UNAUTHORIZED=0
# Avoid proxying localhost/agent-store loops when possible
export NO_PROXY="localhost,127.0.0.1,::1"
export GLOBAL_AGENT_NO_PROXY="localhost,127.0.0.1,::1"

echo "proxied via $PROXY → ${AGENT[*]} $*" >&2
exec "${AGENT[@]}" "$@"
