#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Runs the full Terminal-Bench 2.1 benchmark with swe-1-7 (SWE-1.7 Max).
# Add --agent-timeout-multiplier 2.0 (or higher) if hard tasks time out.
PYTHONPATH="$(dirname "$0")${PYTHONPATH:+:$PYTHONPATH}" \
  exec harbor run \
  --config job-config.yaml \
  --env-file .env \
  --yes \
  "$@"
