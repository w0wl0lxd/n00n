#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Runs one easy task as a smoke test for n00n on Terminal-Bench 2.1.
# Usage: ./run-first-task.sh [MODEL] [ENVIRONMENT] [EXTRA_HARBOR_ARGS...]
#   MODEL:       provider/model spec (default: devin/swe-1-7)
#   ENVIRONMENT: harbor environment type (default: daytona)
MODEL="${1:-devin/swe-1-7}"
ENVIRONMENT="${2:-daytona}"
EXTRA_ARGS=("${@:3}")

PYTHONPATH="$(dirname "$0")${PYTHONPATH:+:$PYTHONPATH}" \
  exec harbor run \
  -d terminal-bench/terminal-bench-2-1 \
  -i 'terminal-bench/fix-git' \
  -a n00n_agent:N00nAgent \
  -m "$MODEL" \
  -e "$ENVIRONMENT" \
  -k 1 \
  -n 1 \
  --env-file .env \
  --yes \
  "${EXTRA_ARGS[@]}"
