# Implementation Plan: Q3 2026 Feature Roadmap

**Branch**: `spec/q3-feature-roadmap` | **Date**: 2026-07-27 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/007-q3-feature-roadmap/spec.md`

## Summary

Sequence eight independently shippable features across three waves. Wave 1 lands scripting parity, cache health, token reductions, and the explore stack. Wave 2 converges ALMAS coordination against existing specs and adds agent lifecycle CLI. Wave 3 fills tree-sitter API stubs and wires thinking budget for local providers.

Each feature ships as its own branch/PR with TDD, scoped verification, and no `git add -A`.

## Technical Context

**Language/Version**: Rust 2024 workspace (edition 2024), Lua 5.1 (Luau plugins)

**Primary Dependencies**: `n00n-daemon`, `n00n-agent`, `n00n-providers`, `n00n-lua`, `n00n-token-profile`, `n00n-ui`

**Storage**: `~/.n00n/` state dir (agents, blackboard, checkpoints); `.codegraph/` index

**Testing**: `cargo nextest run --workspace`, `./scripts/smoke-daemon.sh`, Lua plugin tests, `just gen-docs`

**Target Platform**: Linux primary; Windows daemon transport exists (#149)

**Performance Goals**: No regression on cold-start token surfaces; explore native path avoids subprocess hot path

**Constraints**: `unsafe_code` deny; no unwrap/expect in production; typed errors only; one worktree per implementing agent

**Scale/Scope**: 8 features, ~10 PRs, 3 waves

## Constitution Check

*GATE: AGENTS.md hard gates apply (constitution template not yet ratified).*

| Gate | Status |
|------|--------|
| TDD for new behavior | Required per feature |
| `cargo clippy --all --tests -- -D warnings` | Required before each PR |
| No silent defaults (`unwrap_or`, `.ok()`) | Required |
| Worktree isolation for parallel agents | Required |
| Conventional Commits, no AI attribution | Required |

**Post-design re-check**: Pass — roadmap is planning-only; no production code in this PR.

## Project Structure

### Documentation (this feature)

```text
specs/007-q3-feature-roadmap/
├── spec.md
├── plan.md              # This file
├── research.md
├── tasks.md
└── checklists/
    └── requirements.md
```

### Per-feature branches (implementation)

```text
Wave 1:
  feat/agent-scripting-parity      → specs/006
  feat/cache-health-non-openai     → #155 (stack #154)
  feat/token-reduction-pr2         → specs/004
  feat/token-reduction-pr3         → specs/004
  (merge) feat/explore-native-stack → specs/004, PR #170

Wave 1.5:
  feat/skill-system-v2             → PR #172

Wave 2:
  feat/almas-coordination-converge → specs/001, 002, 003
  feat/agent-lifecycle-cli         → new specs/008 (to create)

Wave 3:
  feat/treesitter-api-complete     → n00n-lua API
  feat/local-thinking-budget       → n00n-providers
```

**Structure Decision**: Meta-roadmap in `007`; each feature retains or creates its own spec directory. Do not fold implementation into this branch.

## Wave Execution Detail

### Wave 1 — Platform parity (start immediately, parallel)

#### F1: Agent scripting parity

- **Branch**: `feat/agent-scripting-parity`
- **Base**: `origin/main`
- **Worktree**: `~/dev/.n00n-worktrees/agent-scripting-parity` (exists) or fresh
- **Touch**: `src/cmd/agent.rs`, `n00n-daemon/src/scripting.rs`, tests, `scripts/smoke-daemon.sh`
- **Verify**: `./scripts/smoke-daemon.sh`; unit tests for `--all`, `--cwd`, `AgentScriptView` serialization
- **Spec**: `specs/006-agent-scripting-parity/spec.md`

#### F2: CacheHealth non-OpenAI

- **Branch**: `feat/cache-health-non-openai` (stack on `feat/cache-health-timer`)
- **Touch**: `n00n-providers/src/providers/{anthropic,openrouter,mistral,google}.rs`, `n00n-agent` event fanout, UI timer
- **Verify**: Per-provider unit tests; manual TUI badge with mocked TTL
- **Issue**: #155

#### F3: Token reduction PR2 + PR3

- **Branch**: `feat/token-reduction-pr2`, then `feat/token-reduction-pr3`
- **Touch**: tool descriptions, system prompt builder, `n00n-token-profile/baselines/cold_start.json`
- **Verify**: `cargo nextest run -p n00n-token-profile`; baseline update in same PR
- **Spec**: `specs/004-token-profiling/plan.md` PR decomposition section

#### F4: Explore stack merge

- **Action**: Rebase #170 onto main, fix CI, merge
- **Verify**: full workspace `cargo nextest run`; `just gen-docs`; manual `explore` against `.codegraph/`
- **Spec**: `specs/004-native-explore-tools/`

#### F9: Skill system v2 (#172)

- **Action**: Rebase #172 onto post-Wave-1 main; merge policy enforcement
- **Branch**: `feat/skill-system-v2`
- **Verify**: skill plugin tests; agent policy refusal tests per PR #172 test plan
- **Wave**: 1.5 (parallel with early Wave 2 once rebased)

### Wave 2 — Coordination + lifecycle (after F1 merges)

#### F5: ALMAS coordination convergence

- **Branch**: `feat/almas-coordination-converge`
- **Touch**: `plugins/team/*`, `plugins/blackboard/*`, `plugins/lib/n00n/*`, `n00n-lua/tests/`
- **Process**: Run `/speckit.converge` on specs 001–003; check off or delete stale tasks; add missing tests
- **Verify**: `cargo nextest run -p n00n-lua`; team plugin spec tests; autonomous run with `human_escalation`

#### F6: Agent lifecycle CLI

- **Branch**: `feat/agent-lifecycle-cli`
- **Prerequisite**: F1 merged (`status --json` for `needs_input`)
- **Process**: `/speckit.specify` → plan → tasks for `008-agent-lifecycle-cli`
- **Touch**: `src/cmd/agent.rs`, `n00n-daemon`, worker artifacts
- **Commands**: `attach`, `respawn`, `logs --tail N`

### Wave 3 — API completeness (parallel anytime)

#### F7: Tree-sitter API

- **Branch**: `feat/treesitter-api-complete`
- **Touch**: `n00n-lua/src/api/treesitter/{mod,language,query}.rs`, docgen, Lua tests

#### F8: Local thinking budget

- **Branch**: `feat/local-thinking-budget`
- **Touch**: `n00n-providers/src/providers/local.rs`, `custom.rs`, request mapping tests

## Complexity Tracking

No violations — roadmap is documentation-only.

## MCP Tool Usage (planning phase)

| Tool | Use |
|------|-----|
| codegraph | Blast radius for token-profile, agent queue |
| semble | Locate stub/deferred patterns |
| exa | Competitor CLI JSON patterns |
| thoughtbox | Attempted; server JS API unavailable — used manual synthesis |
| context7 | Reserved for F2 provider API docs during implementation |

## Next Steps After This PR

1. Review and approve roadmap spec (gate).
2. Dispatch Wave 1 subagents — one worktree each, commit+push before return.
3. Open stacked draft PRs with `## Test plan` checklists.
4. Run `/speckit.tasks` per feature branch when implementation starts.
