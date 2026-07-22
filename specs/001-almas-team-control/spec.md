# Feature Specification: ALMAS role completeness and team/agent-control resilience

**Feature Branch**: `spec/almas-team-control`

**Created**: 2026-07-21

**Status**: Draft

**Input**: User description: "review the project for improvements on the sides of ALMAS, team control, agent control, etc."

## Background and research summary

n00n already ships a partial ALMAS-inspired multi-agent layer: the `team` plugin runs a supervisor that plans SDLC steps and dispatches `product_manager`, `planner`, `developer`, `tester`, and `reviewer` roles; `workflow` provides a sandboxed Lua orchestrator with resume/replay; `agent_control` exposes basic list/status/message/stop operations over `n00n.session`.

The ALMAS paper (arXiv:2510.03463) defines six agile roles: **Sprint Agent** (product/scrum master), **Supervisor Agent**, **Summary Agent**, **Control Agent**, **Developer Agent**, and **Peer Review Agent**. It also proposes three design principles — *Context-aware, Collaborative, Cost-effective* — and explicit human integration with Jira/Bitbucket. n00n is missing the Sprint and Summary agents, has only a lexical/hashing retrieval seam, and has no human-escalation path.

The MAST failure taxonomy (arXiv:2503.13657) identifies 14 failure modes across System Design, Inter-Agent Misalignment, and Task Verification. n00n is vulnerable to:

- **FM-1.1 / FM-1.2** (disobey task/role spec) because requirements are not refined before execution.
- **FM-1.4** (loss of conversation history / context) because it has no summary-based context compression.
- **FM-3.1 / FM-3.2 / FM-3.3** (premature termination, incomplete/incorrect verification) because `run_autonomous` breaks on the first error and `workflow` has no rollback.

Recent SOTA work reinforces the same priorities:

- **SPOQ** (arXiv:2606.03115) shows that wave-based topological dispatch, dual validation gates (planning + code), and Human-as-an-Agent (HaaA) integration dramatically improve task pass rates and reduce defects.
- **SagaLLM** (arXiv:2503.11951) argues for saga-style compensating actions and checkpointing in multi-agent workflows to maintain consistency.
- **Control Plane as a Tool** (arXiv:2505.06817) and the **TEA protocol / AgentOrchestra** (arXiv:2506.12508) call for a single tool-routing/policy abstraction and versioned agent/tool/environment lifecycles.
- **MASFactory** (arXiv:2603.06007), **Agyn** (arXiv:2602.01465), and **CodeTeam** (arXiv:2606.22082) all structure software engineering as role-specialized, graph/team-based coordination with human oversight.
- LangGraph and CrewAI (context7) demonstrate production patterns: conditional routing, checkpoint persistence, human-in-the-loop interrupts, and role-based agent crews.

This spec scopes an MVP that closes the highest-impact ALMAS/SOTA gaps while staying inside n00n's existing Lua-plugin + `n00n.agent.*` / `n00n.session.*` architecture.

## User Scenarios & Testing

### User Story 1 — Sprint Agent refines goals before planning (Priority: P1)

As a developer using `team`, I want a Sprint Agent to clarify my high-level goal and produce acceptance criteria and effort estimates before the supervisor builds a plan, so that downstream role agents receive unambiguous requirements and fewer plans drift.

**Why this priority**: Directly addresses ALMAS's missing Sprint Agent and MAST FM-1.1/1.2. It is an additive, non-breaking change to `plugins/team/roles.lua` and the supervisor step schema.

**Independent Test**: Run `n00n team` on a vague goal and verify the first plan step has role `sprint` with `acceptance_criteria` and an `effort` field.

**Acceptance Scenarios**:

1. **Given** a goal like "make the auth flow better", **When** the supervisor runs, **Then** a `sprint` step rewrites it to a concrete scope with acceptance criteria and an effort estimate.
2. **Given** a goal that is already explicit, **When** the Sprint Agent runs, **Then** it returns the goal unchanged with a minimal acceptance-criteria block.

### User Story 2 — Human escalation on unrecoverable failures (Priority: P1)

As a developer, when `team` autonomous mode cannot recover from a step failure or quorum rejection, I want it to pause, surface a structured summary, and let me steer the run via `agent_control` instead of silently failing.

**Why this priority**: Matches ALMAS human-integration vision, SPOQ HaaA, and MAST FC3. The current `run_autonomous` breaks on the first error (`plugins/team/init.lua:259-262`) with no escalation path.

**Independent Test**: Force a step to fail in `team` autonomous mode and verify the run returns a `needs_input` status with a summary; then send a steering message and resume.

**Acceptance Scenarios**:

1. **Given** an autonomous `team` run where the `developer` step errors, **When** the error occurs after retry budget is exhausted, **Then** the run pauses and `agent_control status <id>` shows `status: needs_input` with a `summary` field.
2. **Given** a paused run, **When** I call `agent_control` with `action: message` containing corrective instructions, **Then** the run re-plans the remaining steps and continues from the failed step.

### User Story 3 — Saga compensating actions in workflow (Priority: P1)

As a developer writing `workflow` scripts, I can register rollback functions with `compensate(fn)` so that if a later `agent()` call fails, partial side effects are undone before the error is surfaced.

**Why this priority**: Closes the biggest `workflow` resilience gap versus SagaLLM and SPOQ dual-validation. The current `workflow` environment (`plugins/workflow/init.lua:643-710`) has `agent`, `parallel`, `pipeline`, `phase`, and `log` but no compensation primitive.

**Independent Test**: Write a workflow that registers two compensations, fail the third `agent()` call, and verify both rollback functions execute in reverse order and the final error mentions the cleanup.

**Acceptance Scenarios**:

1. **Given** a workflow that calls `compensate(function() ... end)` twice and then a failing `agent()`, **When** the `agent()` errors, **Then** the second compensation runs first, the first compensation runs second, and the original error is reported.
2. **Given** a workflow where a compensation itself fails, **When** rollback is triggered, **Then** the workflow reports the compensation failure alongside the original error but does not crash.

### User Story 4 — Summary Agent + Meta-RAG retrieval (Priority: P2)

As a developer, I want the `team` plugin to retrieve context from a pre-generated summary index of the repository, so that long contexts are compressed into concise, language-agnostic summaries before being passed to role agents.

**Why this priority**: Implements ALMAS Summary Agent and Meta-RAG, reducing MAST FM-1.4 (context loss) and token usage. The current `plugins/team/retrieve.lua` uses lexical grep + hashing-trick vectors (lines 22-128).

**Independent Test**: Run `team` with `use_retrieval=true` and `use_summary=true` on a repo with a summary index, then verify the retrieved context block is shorter than the non-summary fallback while still containing the relevant file references.

**Acceptance Scenarios**:

1. **Given** a populated `state_dir/projects/{pid}/summaries/` index, **When** `retrieve` is called for a step, **Then** it returns top-ranked summary snippets for the relevant code units before falling back to grep.
2. **Given** no summary index, **When** summary retrieval is requested, **Then** the system falls back transparently to the existing lexical/vector retrieval.

### User Story 5 — Structured telemetry for multi-agent runs (Priority: P2)

As an operator, I want `team` and `workflow` runs to emit structured events (start, done, error) to a JSONL log correlated by `run_id`, so that I can trace execution and debug failures.

**Why this priority**: Required for production observability and aligns with Control Plane as a Tool, TEA traceability, and SPOQ's validation gates. Today only a progress UI header is updated.

**Independent Test**: Run a `team` and a `workflow` and verify `events.jsonl` files contain start/done/error events with timestamps, run_ids, and agent names.

**Acceptance Scenarios**:

1. **Given** a `team` autonomous run, **When** each role step starts and completes, **Then** a `step_started` and `step_done` event are appended to the run's JSONL log.
2. **Given** a `workflow` run, **When** an `agent()` call fails, **Then** an `agent_error` event is emitted with the agent label, run_id, and error text.

### User Story 6 — Richer agent_control routing and policy (Priority: P3)

As a developer, I want `agent_control` to expose pause/resume actions and enforce simple policies (e.g., "background agents cannot call write tools"), so that long-running agent teams can be safely steered.

**Why this priority**: Builds toward the Control Plane as a Tool pattern and TEA's versioned lifecycle vision. This is P3 because the P1/P2 items deliver the largest ALMAS/SOTA alignment first.

**Independent Test**: List live agents, pause one, verify its tool surface is restricted, then resume it.

**Acceptance Scenarios**:

1. **Given** a live background agent, **When** `agent_control` `pause` is called, **Then** the agent's next turn blocks for a `resume` message and status shows `paused`.
2. **Given** a paused agent, **When** `agent_control` `resume` is called with a policy flag, **Then** the agent continues with the restricted tool set.

## Edge Cases

- **Sprint Agent receives an already-concrete goal**: returns a lightweight plan with acceptance criteria only.
- **No summary index exists**: summary retrieval falls back to lexical/hashing retrieval without error.
- **Human escalation in non-interactive mode**: the run returns a structured `needs_input` payload and exits gracefully; the user can resume later via `agent_control`.
- **Compensation fails during rollback**: the original error is preserved and the compensation error is appended; the workflow still terminates.
- **Telemetry disk write fails**: events are kept in memory and emitted as a tool annotation; disk errors are logged but do not abort the run.

## Requirements

### Functional Requirements

- **FR-001**: `plugins/team/roles.lua` adds a `sprint` role with a weak/medium tier and a system prompt that instructs requirement refinement, acceptance criteria, and effort estimation.
- **FR-002**: `plugins/team/init.lua` `PLANNER_OUTPUT` schema accepts `sprint` as a valid role and optionally includes `acceptance_criteria` (string) and `effort` (string) fields on each step.
- **FR-003**: `run_autonomous` in `plugins/team/init.lua` tracks failures, supports a configurable retry budget, and after exhaustion returns a pause envelope instead of `break`ing the loop.
- **FR-004**: `plugins/agent_control/init.lua` adds `pause` and `resume` actions and surfaces `needs_input`/`paused` status for background agents.
- **FR-005**: `plugins/workflow/init.lua` adds `compensate(fn)` and `on_error(fn)` to the sandbox environment; errors trigger registered compensations in LIFO order.
- **FR-006**: A new `plugins/team/summary.lua` module generates structured natural-language summaries per code unit using the existing `index` tool or tree traversal and stores them under `state_dir/projects/{pid}/summaries/`.
- **FR-007**: `plugins/team/retrieve.lua` supports `use_summary` (default false) that queries the summary index first; fallback to lexical/vector retrieval remains unchanged.
- **FR-008**: `plugins/team/init.lua` and `plugins/workflow/init.lua` emit structured telemetry events to `events.jsonl` files under the respective run directories.
- **FR-009**: Telemetry events include `run_id`, `agent_id`/`label`, `event` (`step_started`, `step_done`, `agent_error`, `human_escalation`), and `timestamp`.
- **FR-010**: `plugins/team/tests/spec.lua` and `plugins/workflow/tests/spec.lua` are extended to cover Sprint Agent output schema, human escalation payload, and saga rollback behavior.

### Key Entities

- **Sprint Step**: a plan step with role `sprint` carrying `prompt`, optional `acceptance_criteria`, and optional `effort`.
- **Pause Envelope**: a table returned by `run_autonomous` containing `{paused=true, summary=<string>, failed_step=<number>, agent_id=<string>}`.
- **Summary Index**: a directory of JSON files mapping file paths to natural-language summary strings, keyed by a stable hash.
- **Compensation Stack**: a per-workflow Lua stack of zero-argument rollback functions collected via `compensate(fn)`.
- **Telemetry Log**: an append-only JSONL file of structured event records.

## Success Criteria

### Measurable Outcomes

- **SC-001**: In a sample of 10 `team` supervised runs, at least 8 include a `sprint` step that rewrites vague goals into acceptance-criteria-bearing steps.
- **SC-002**: Human escalation is triggered and produces a resumable `needs_input` status on all injected step failures in `plugins/team/tests/spec.lua`.
- **SC-003**: `workflow` saga rollback tests pass and partial-state errors are reduced to zero in the mock failing-agent test suite.
- **SC-004**: Summary retrieval reduces average per-step context block size by at least 20% on 5 representative tasks while preserving relevant `file:line` references.
- **SC-005**: Every `team` and `workflow` run produces an `events.jsonl` file containing at least `run_started` and `run_done` events.

## Assumptions

- n00n keeps its Lua-plugin architecture and `n00n.agent.*` / `n00n.session.*` primitives; no core rewrite is required.
- LLM providers continue to expose tiered model routing (weak/medium/strong).
- The MVP keeps the existing hashing-trick embedding fallback; real embeddings are out of scope.
- SDLC tool integrations (Jira, GitHub Issues, Bitbucket) are out of MVP scope.
- Human escalation is surfaced through the existing `agent_control` tool and UI session status.

## Implementation Notes (non-binding, for planning)

- Sprint/role changes are localized to `plugins/team/roles.lua` and `plugins/team/init.lua`.
- Human escalation touches `plugins/team/init.lua:253-282` and `plugins/agent_control/init.lua:43-79`.
- Saga compensation touches `plugins/workflow/init.lua:643-710` (build_env) and the error path in `handler` (`plugins/workflow/init.lua:713-806`).
- Summary Agent is a new `plugins/team/summary.lua` module integrated via an optional `use_summary` flag in `plugins/team/retrieve.lua`.
- Telemetry can start as a small module in `plugins/team/telemetry.lua` and `plugins/workflow/telemetry.lua` that writes JSONL through `n00n.fs`.
