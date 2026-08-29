#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Installs the agent and environment for one easy task without running the agent.
# Guard against indefinite environment-start hangs with a generous timeout.
HARBOR_TIMEOUT="${HARBOR_TIMEOUT:-900}"

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
  --install-only
  --env-file .env
  --yes
  "${EXTRA_ARGS[@]}"
)

if [[ ${HARBOR_TIMEOUT} == "0" ]]; then
  harbor run "${args[@]}"
else
  if command -v timeout >/dev/null 2>&1; then
    timeout "${HARBOR_TIMEOUT}" harbor run "${args[@]}"
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "${HARBOR_TIMEOUT}" harbor run "${args[@]}"
  else
    echo "warning: timeout/gtimeout not available, running without a timeout" >&2
    harbor run "${args[@]}"
  fi
fi
