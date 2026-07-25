#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Runs one easy task as a smoke test for swe-1-7 on Terminal-Bench 2.1.
exec harbor run \
  -d terminal-bench/terminal-bench-2-1 \
  -i 'terminal-bench/fix-git' \
  -a devin \
  -m swe-1-7 \
  -e daytona \
  -k 1 \
  -n 1 \
  --env-file .env \
  --yes \
  "$@"
