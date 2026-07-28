# Feature Specification: Q3 2026 Feature Roadmap

**Feature Branch**: `spec/q3-feature-roadmap`

**Created**: 2026-07-27

**Status**: Approved (planning artifact)

**Input**: User description: "Q3 2026 feature roadmap: prioritize and sequence 8 agent-platform features (scripting parity, cache health, token reduction, explore stack, ALMAS coordination, agent lifecycle, treesitter API, provider thinking budget)"

## Background

n00n has a healthy backlog of partially landed work: open PRs (#154, #170, #172), existing specs (`001`–`006`), and one open issue (#155). Several coordination primitives (blackboard, waves, checkpoints, sprint role) already exist on `main` but lack test coverage, docgen sync, and convergence verification against their specs.

This roadmap does **not** implement features. It sequences eight independently shippable slices, each with its own branch, spec reference, verification gate, and dependency wave.

## Prioritized Feature Portfolio

| # | Feature | Priority | Status on main | Primary spec / issue | Est. effort |
|---|---------|----------|----------------|----------------------|-------------|
| F1 | Agent scripting parity (`list`/`status --json`) | P1 | ~80% landed | `specs/006-agent-scripting-parity` | 1 PR |
| F2 | CacheHealth for non-OpenAI providers | P1 | PR #154 open | GitHub #155 | 1 PR |
| F3 | Token profiling reductions (PR2/PR3) | P1 | PR1 crate landed | `specs/004-token-profiling` | 2 PRs |
| F4 | Native explore stack merge | P1 | PR #170 open | `specs/004-native-explore-tools` | 1 merge + follow-ups |
| F5 | ALMAS coordination convergence | P2 | Code landed, tasks stale | `specs/002`, `003`, `001` | 2 PRs |
| F6 | Agent lifecycle (attach / respawn / logs) | P2 | Deferred | New spec `008` (TBD) | 2 PRs |
| F7 | Tree-sitter Lua API completeness | P3 | Stubs in docs | `n00n-lua` API | 1 PR |
| F8 | Local/custom provider thinking budget | P3 | TODO in providers | None | 1 PR |
| F9 | Skill system v2 + agent policy enforcement | P1.5 | PR #172 open | PR #172 | 1 merge |

**Wave 1.5** (after Wave 1 lands, parallel with early Wave 2): F9 skill v2 — promoted per user decision 2026-07-27.

## User Scenarios & Testing

### User Story 1 — Ship scripting parity for automation (Priority: P1)

As an operator integrating n00n into CI or Mission Control-style dashboards, I want stable `n00n agent list --json` and `n00n agent status <id> --json` output with normalized `state`, `--all`, and `--cwd` filters, so I can script agent fleets without parsing NDJSON control wire format.

**Why this priority**: Daemon plane (#149) is merged; scripting is the last mile for headless ops parity with Claude Code / OpenCode.

**Independent Test**: Run `./scripts/smoke-daemon.sh`; assert `n00n agent list --json` returns a JSON array with `id`, `backend`, `state`, `status`; `--cwd` excludes foreign sessions; default list hides terminal workers.

**Acceptance Scenarios**:

1. **Given** a background worker and a live TUI session, **When** I run `n00n agent list --json`, **Then** both appear with stable `state` enum values.
2. **Given** stopped workers on disk, **When** I list without `--all`, **Then** terminal workers are omitted.
3. **Given** `--cwd /path/to/project`, **When** I list, **Then** only agents whose `cwd` matches are returned.

---

### User Story 2 — Cache visibility across providers (Priority: P1)

As a TUI user watching token costs, I want the status-bar cache timer (`⧉`) to reflect Anthropic, OpenRouter, Mistral, and Google cache contracts—not only OpenAI—so I know when my prompt cache expires.

**Why this priority**: #154 introduced the event pipeline; #155 is the natural follow-up with clear acceptance criteria.

**Independent Test**: Mock provider responses per vendor; assert `ProviderEvent::CacheHealth` fires with `valid_until`, `ttl_seconds`, and `hit`; UI badge clears when `valid_until == 0`.

**Acceptance Scenarios**:

1. **Given** an Anthropic turn with `cache_control: ephemeral`, **When** the response returns, **Then** a CacheHealth event emits with 5-minute TTL semantics.
2. **Given** a provider without cache support, **When** a turn completes, **Then** `valid_until == 0` is emitted so the badge disappears.

---

### User Story 3 — Measured token reduction (Priority: P1)

As a maintainer, I want PR2/PR3 token savings backed by `n00n-token-profile` baselines, so regressions are caught in CI and savings are provable.

**Why this priority**: `n00n-token-profile` crate and baselines exist; the spec explicitly defers savings work to follow-up PRs.

**Independent Test**: `cargo nextest run -p n00n-token-profile` passes; intentional baseline updates ship in the same PR as the reduction.

**Acceptance Scenarios**:

1. **Given** a tool-description trim in PR2, **When** CI runs, **Then** `main_tools_schemas` tokens decrease vs baseline without breaking tool_count.
2. **Given** a system-prompt compaction in PR3, **When** CI runs, **Then** `system_prompt` surface stays under hard gate thresholds.

---

### User Story 4 — Native explore tools on main (Priority: P1)

As an agent user, I want `explore` / `codegraph` / `arbor` / `semble` routing landed from #170, so exploration uses native indexes instead of shelling out blindly.

**Why this priority**: Large stacked PR already reviewed in phases; merge unblocks token-efficient exploration per AGENTS.md.

**Independent Test**: Merge #170; `cargo nextest run --workspace`; regenerate docs with `just gen-docs`; smoke `explore` plugin against indexed repo.

**Acceptance Scenarios**:

1. **Given** a `.codegraph/` index, **When** the agent calls explore, **Then** native SQLite path is used (no subprocess for hot path).
2. **Given** missing index, **When** explore is invoked, **Then** actionable error guides user to `codegraph init`.

---

### User Story 5 — ALMAS coordination verified end-to-end (Priority: P2)

As a `team` user running multi-step autonomous workflows, I want waves, checkpoints, blackboard, sprint role, and human escalation tested and documented against specs 001–003, so coordination behavior is trustworthy.

**Why this priority**: Substantial Lua/Rust code exists (`plugins/blackboard`, `plugins/team/waves.lua`, `plugins/lib/n00n/checkpoint.lua`) but spec tasks remain unchecked—risk of silent drift.

**Independent Test**: `cargo nextest run -p n00n-lua`; Lua plugin tests for team/blackboard; manual `n00n team` with `waves=true`, `checkpoints=true`, `human_escalation=true`.

**Acceptance Scenarios**:

1. **Given** `human_escalation=true` and a failing developer step, **When** retries exhaust, **Then** run pauses with `needs_input` and a summary.
2. **Given** `checkpoints=true`, **When** a wave completes, **Then** resume from checkpoint skips completed steps.

---

### User Story 6 — Agent attach, respawn, and log tail (Priority: P2)

As a power user, I want `n00n agent attach`, `n00n agent respawn`, and `n00n agent logs <id>` for stopped or background workers, matching Claude Code ergonomics.

**Why this priority**: Explicitly deferred in spec 006; closes the largest remaining CLI gap after scripting parity.

**Independent Test**: Start background agent, stop it, respawn; attach streams output; logs returns last N lines from worker artifact.

**Acceptance Scenarios**:

1. **Given** a stopped worker with `agent.json`, **When** I `n00n agent respawn <id>`, **Then** a new run continues with preserved config.
2. **Given** a running worker, **When** I `n00n agent logs <id> --tail 50`, **Then** I receive the last 50 log lines without opening the TUI.

---

### User Story 7 — Tree-sitter API stubs implemented (Priority: P3)

As a plugin author, I want `n00n.treesitter` cursor lookup, custom grammar paths, and named query resolution to work as documented, instead of returning `nil`.

**Why this priority**: Docs promise capabilities that currently stub out—violates "Philosophy of not hiding anything."

**Independent Test**: Lua integration tests for each formerly-stubbed API; docgen regenerated.

**Acceptance Scenarios**:

1. **Given** a parsed buffer, **When** I call cursor-based node lookup, **Then** I receive the node at that position.
2. **Given** a custom `.so` grammar path in opts, **When** I load the language, **Then** parsing uses that grammar.

---

### User Story 8 — Thinking budget on local providers (Priority: P3)

As a local-model user, I want thinking/reasoning budget from agent config wired into local and custom OpenAI-compatible providers, so extended thinking works consistently with cloud providers.

**Why this priority**: Small, isolated provider change (`local.rs`, `custom.rs` TODOs).

**Independent Test**: Unit test that `ThinkingConfig` maps to provider request fields when model supports it.

**Acceptance Scenarios**:

1. **Given** a thinking-enabled local model, **When** agent sets a thinking budget, **Then** the provider request includes the budget field.
2. **Given** a model without thinking support, **When** budget is set, **Then** provider ignores it without error.

## Functional Requirements

- **FR-001**: Roadmap MUST sequence features in dependency waves (see plan.md).
- **FR-002**: Each feature MUST have an independent branch, spec reference, and verification command.
- **FR-003**: No feature branch MAY depend on unmerged work from the same wave without explicit stacking notes.
- **FR-004**: Stale task checklists in specs 002/003 MUST be reconciled during F5 (checked or deleted).
- **FR-005**: All production changes MUST pass `cargo clippy --all --tests -- -D warnings` and `cargo nextest run --workspace` (or scoped crate tests where noted).

## Success Criteria

1. Eight features are cataloged with priority, status, branch target, and test plan.
2. Wave 1 (F1–F4) can start immediately with no cross-dependencies.
3. Wave 1.5 (F9 skill v2) starts after Wave 1 or in parallel once #172 rebases cleanly.
4. Wave 2 (F5–F6) starts after F1 lands (scripting parity needed for escalation UX).
5. Wave 3 (F7–F8) is independent and parallelizable.
6. Planning artifacts (`spec.md`, `plan.md`, `research.md`, `tasks.md`) are committed on `spec/q3-feature-roadmap`.

## Assumptions

- Base branch for all work is `origin/main` (currently includes #149 daemon plane).
- Open PRs #154, #170, #172 are rebased onto main before merge, not re-implemented.
- Token profiling PR1 (`n00n-token-profile`) is already on main.
- Constitution file is still template-only; AGENTS.md hard gates apply.

## Out of Scope

- HTTP REST control plane, webhooks, SSE (deferred by design in spec 006).
- Interactive agent dashboard TUI (`claude agents` parity).
- New provider integrations beyond cache-health wiring.
