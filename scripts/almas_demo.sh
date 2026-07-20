#!/usr/bin/env bash
# Drive the almas plugin across every mode and the new ibn/quorum/swarm toggles.
#
# Usage:
#   just almas-demo            # run the default matrix
#   ./scripts/almas_demo.sh    # same, directly
#   ALMAS_GOAL="add a retry helper and cover it with tests" ./scripts/almas_demo.sh
#
# Each combo is run with `n00n -p` so the agent invokes the almas tool. Real runs
# need a configured provider (n00n auth). Set ALMAS_MODEL to pin a tier/model.
set -u

cd "$(dirname "$0")/.." || exit 1

GOAL="${ALMAS_GOAL:-Add a small retry helper with unit tests and a short doc comment.}"
MODEL_FLAG=""
if [ -n "${ALMAS_MODEL:-}" ]; then
  MODEL_FLAG="-m ${ALMAS_MODEL}"
fi

# (mode, extra prompt suffix, label) tuples that exercise each branch.
run() {
  local mode="$1"
  local suffix="$2"
  local label="$3"
  local prompt="Use the almas tool with mode=${mode}. ${suffix}Goal: ${GOAL}"

  echo
  echo "==================================================================="
  echo ">> [${label}] mode=${mode}"
  echo ">> prompt: ${prompt}"
  echo "==================================================================="
  # shellcheck disable=SC2086
  n00n -p ${MODEL_FLAG} "${prompt}" \
    || echo "!! [${label}] exited non-zero (see above)"
}

# Supervised: returns the plan for review (no execution, cheapest).
run "supervised" "" "supervised-plan"

# Autonomous: centralized run; tester/reviewer gated by quorum.
run "autonomous" "" "autonomous-quorum-on"

# Autonomous with retrieval off to stress the non-retrieval path.
run "autonomous" "Set use_retrieval=false. " "autonomous-no-retrieval"

# Swarm: ibn gate should fan out for a weak/medium model (multi-step goal).
run "swarm" "Set model_tier=medium. " "swarm-fanout-medium"

# Swarm + strong single-step: ibn gate should fall back to single-agent pass.
run "swarm" "Set model_tier=strong. " "swarm-ibn-fallback-strong"

# Swarm with bounded rounds to keep cost/length small for manual inspection.
run "swarm" "Set model_tier=weak, max_rounds=2. " "swarm-bounded-weak"

echo
echo "Done. Pheromone state (if any) lives under your state dir under"
echo "  projects/*/almas/swarm_pheromone.json"
