#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Runs the full Terminal-Bench 2.1 benchmark.
# Usage: ./run-full.sh [MODEL] [ENVIRONMENT] [N_ATTEMPTS] [N_CONCURRENT] [EXTRA_HARBOR_ARGS...]
#   MODEL:        provider/model spec (default: devin/swe-1-7)
#   ENVIRONMENT:  harbor environment type (default: daytona)
#   N_ATTEMPTS:   attempts per trial (default: 5)
#   N_CONCURRENT: concurrent trials (default: 1)
# Add --agent-timeout-multiplier 2.0 (or higher) if hard tasks time out.
MODEL="${1:-devin/swe-1-7}"
ENVIRONMENT="${2:-daytona}"
N_ATTEMPTS="${3:-5}"
N_CONCURRENT="${4:-1}"
EXTRA_ARGS=("${@:5}")

PYTHONPATH="$(dirname "$0")${PYTHONPATH:+:$PYTHONPATH}" \
  exec harbor run \
  -d terminal-bench/terminal-bench-2-1 \
  -a n00n_agent:n00nAgent \
  -m "$MODEL" \
  -e "$ENVIRONMENT" \
  -k "$N_ATTEMPTS" \
  -n "$N_CONCURRENT" \
  --env-file .env \
  --yes \
  "${EXTRA_ARGS[@]}"
