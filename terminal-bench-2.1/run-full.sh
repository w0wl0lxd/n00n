#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Runs the full Terminal-Bench 2.1 benchmark.
# Set HARBOR_TIMEOUT (seconds) to guard against indefinite hangs; 0 disables.
HARBOR_TIMEOUT="${HARBOR_TIMEOUT:-0}"

trap 'if [[ "$?" -eq 124 ]]; then echo "harbor run timed out after ${HARBOR_TIMEOUT}s" >&2; fi' EXIT

if [[ "${HARBOR_TIMEOUT}" == "0" ]]; then
  PYTHONPATH="$(dirname "$0")${PYTHONPATH:+:$PYTHONPATH}" \
    harbor run \
    --config job-config.yaml \
    --env-file .env \
    --yes \
    "$@"
else
  PYTHONPATH="$(dirname "$0")${PYTHONPATH:+:$PYTHONPATH}" \
    timeout "${HARBOR_TIMEOUT}" harbor run \
    --config job-config.yaml \
    --env-file .env \
    --yes \
    "$@"
fi
