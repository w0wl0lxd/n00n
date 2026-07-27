#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Runs one easy task as a smoke test for n00n on Terminal-Bench 2.1.
# Guard against indefinite hangs; override HARBOR_TIMEOUT to disable (0=no timeout).
HARBOR_TIMEOUT="${HARBOR_TIMEOUT:-1800}"

MODEL="${1:-devin/swe-1-7}"
ENVIRONMENT="${2:-daytona}"
EXTRA_ARGS=("${@:3}")

trap 'if [[ "$?" -eq 124 ]]; then echo "harbor run timed out after ${HARBOR_TIMEOUT}s" >&2; fi' EXIT

PYTHONPATH="${PWD}${PYTHONPATH:+:$PYTHONPATH}"
export PYTHONPATH

args=(
  -d terminal-bench/terminal-bench-2-1
  -i 'terminal-bench/fix-git'
  -a n00n_agent:n00nAgent
  -m "$MODEL"
  -e "$ENVIRONMENT"
  -k 1
  -n 1
  --env-file .env
  --yes
  "${EXTRA_ARGS[@]}"
)

if [[ "${HARBOR_TIMEOUT}" == "0" ]]; then
  harbor run "${args[@]}"
else
  timeout "${HARBOR_TIMEOUT}" harbor run "${args[@]}"
fi
