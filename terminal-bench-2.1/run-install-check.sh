#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Installs the agent and environment for one easy task without running the agent.
# Guard against indefinite environment-start hangs with a generous timeout.
HARBOR_TIMEOUT="${HARBOR_TIMEOUT:-900}"

trap 'if [[ "$?" -eq 124 ]]; then echo "harbor run timed out after ${HARBOR_TIMEOUT}s" >&2; fi' EXIT

PYTHONPATH="$(dirname "$0")${PYTHONPATH:+:$PYTHONPATH}" \
  timeout "${HARBOR_TIMEOUT}" harbor run \
  -d terminal-bench/terminal-bench-2-1 \
  -i 'terminal-bench/fix-git' \
  -a n00n_agent:n00nAgent \
  -m swe-1-7 \
  -e daytona \
  -k 1 \
  -n 1 \
  --install-only \
  --env-file .env \
  --yes \
  "$@"
