#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Installs the agent and environment for one easy task without running the agent.
PYTHONPATH="$(dirname "$0")${PYTHONPATH:+:$PYTHONPATH}" \
  exec harbor run \
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
