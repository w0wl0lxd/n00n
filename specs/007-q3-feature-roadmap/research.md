# Research: Q3 2026 Feature Roadmap

**Date**: 2026-07-27

## Methodology

Evidence gathered via:

- **Codegraph** — indexed symbol blast radius for `n00n-token-profile`, agent queue, terminal-bench harness.
- **Semble** — literal search for deferred/stub patterns and CacheHealth references.
- **Exa** — competitor CLI patterns (Mission Control `agents list --json`, CLI-Anything JSON mode).
- **GitHub CLI** — open issues (#155), open PRs (#154, #170, #172), merge state on `main`.
- **Repo inspection** — `specs/` directory inventory, `plugins/team/*`, `n00n-daemon/src/scripting.rs`.

## Current State Summary

### Already on main (post-#149)

| Area | Evidence |
|------|----------|
| Daemon control plane | Merged #149; `n00n-daemon`, `src/cmd/agent.rs` |
| Agent scripting types | `n00n-daemon/src/scripting.rs` — `AgentScriptView`, `normalize_state`, `is_terminal_worker_status` |
| Token profiling PR1 | `n00n-token-profile/` crate + `baselines/cold_start.json` + regression tests |
| Team waves/checkpoints | `plugins/team/waves.lua`, `plugins/team/init.lua` (`run_waves`, checkpoint save/load) |
| Sprint role | `plugins/team/roles.lua` — `sprint` role with weak tier |
| Human escalation flag | `plugins/team/init.lua` — `human_escalation` input, pause on failure paths |
| Blackboard plugin | `plugins/blackboard/init.lua` — posts, claims, query (~640 LOC) |
| Checkpoint lib | `plugins/lib/n00n/checkpoint.lua` — save/load/latest |
| Live context lib | `plugins/lib/n00n/live_context.lua` — exists, integration depth TBD |

### In flight (open PRs)

| PR | Branch | Blocker |
|----|--------|---------|
| #154 | `feat/cache-health-timer` | Anthropic/OpenRouter/Mistral/Google emitters (#155) |
| #170 | stacked explore phases | CI on consolidated tip |
| #172 | `feat/skill-system-v2` | Independent; not in core 8 but worth tracking |
| #169 | codegraph rusqlite | Part of #170 stack |

### Spec drift

- `specs/002-agent-coordination-visibility/tasks.md` — all tasks unchecked despite substantial implementation.
- `specs/003-agent-coordination-execution/tasks.md` — same; wave runner exists in `team/init.lua`.
- `specs/006-agent-scripting-parity` — spec active; CLI wiring partially present in `src/cmd/agent.rs`.
- `specs/001-almas-team-control` — draft; sprint + escalation partially address US1/US2.

### Competitor gaps (from spec 006)

| Capability | n00n after F1 | After F6 |
|------------|---------------|----------|
| `agents --json` | Yes | Yes |
| `--all`, `--cwd` | Yes (F1) | Yes |
| attach / respawn / logs | No | Yes (F6) |
| HTTP control plane | No (by design) | No |

### Tree-sitter stubs (from site docs / source)

- `n00n-lua/src/api/treesitter/mod.rs` — cursor node lookup returns nil
- `n00n-lua/src/api/treesitter/language.rs` — custom grammar `path` unsupported
- `n00n-lua/src/api/treesitter/query.rs` — named built-in queries return nil

### Provider TODOs

- `n00n-providers/src/providers/local.rs:156` — thinking budget not wired
- `n00n-providers/src/providers/custom.rs:296` — same

## Dependency Analysis

```text
Wave 1 (parallel, no inter-deps):
  F1 agent-scripting-parity
  F2 cache-health-non-openai  (stack on #154)
  F3 token-reduction-pr2/pr3
  F4 explore-stack-merge      (#170)

Wave 2 (after F1):
  F5 almas-coordination-convergence
  F6 agent-lifecycle-cli

Wave 3 (parallel):
  F7 treesitter-api-complete
  F8 local-thinking-budget
```

**Rationale for F5 after F1**: Human escalation UX is validated via `n00n agent status --json` showing `needs_input`; scripting parity is the observability prerequisite.

## Risk Register

| Risk | Mitigation |
|------|------------|
| Stale spec tasks mislead implementers | F5 explicitly runs `speckit.converge` against 001–003 |
| #170 merge conflicts with native search worktrees | Rebase stack onto main tip before merge; run full workspace CI |
| Token reduction breaks tool schemas | Hard gate on `main_tools_schemas` tool_count exact match |
| CacheHealth TTL semantics differ per vendor | Per-provider unit tests with fixture responses; document in provider module |

## Decisions

1. **Eight features, three waves** — balances parallel throughput with escalation dependency.
2. **F4 is merge-first, not re-spec** — #170 already has `specs/004-native-explore-tools/plan.md`.
3. **F6 gets a new spec `008-agent-lifecycle-cli`** during implementation (out of roadmap scope).
4. **Skill system v2 (#172) promoted to F9 / Wave 1.5** per user decision 2026-07-27.
