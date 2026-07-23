# Feature Specification: ALMAS expansion - cross-session visibility, blackboard coordination, and agent control plane

**Feature Branch**: `spec/almas-coordination-visibility`

**Created**: 2026-07-23

**Status**: Draft

**Input**: User description: "ALMAS expansion - cross-session visibility, blackboard coordination, and agent control plane"

## Background and research summary

n00n's current multi-agent layer operates within isolated sessions: each `n00n.session` is a self-contained agent loop with its own state, and the `team` plugin coordinates role agents only within a single supervised run. Cross-session coordination does not exist. Two concurrent `team` runs cannot see each other's progress, cannot share a shared task queue, and cannot coordinate resource access. This isolation creates duplicate work, race conditions on file edits, and no mechanism for global progress tracking across parallel workstreams.

The ALMAS paper (arXiv:2510.03463) defines multi-agent coordination principles but assumes a shared workspace and cross-role visibility. n00n implements the roles but lacks the coordination substrate. The MAST failure taxonomy (arXiv:2503.13657) identifies **FM-2.1** (resource conflicts) and **FM-2.3** (concurrency bugs) as critical failure modes in multi-agent systems without proper coordination. SPOQ (arXiv:2606.03115) demonstrates that wave-based topological dispatch and dual validation gates dramatically reduce conflicts by structuring agent interaction as a directed graph with clear handoff points.

The blackboard pattern (agents4science.github.io, agentpatternscatalog.org, redis.io blog, amux.io) provides a proven coordination model: agents post observations and claims to a shared data structure, claim tasks atomically, and observe each other's state without direct messaging. This pattern scales well for concurrent agents and aligns with n00n's Lua plugin architecture. Control Plane as a Tool (arXiv:2505.06817) and the TEA protocol (arXiv:2506.12508) argue for a centralized policy and versioned lifecycle management for agents, tools, and environments. SagaLLM (arXiv:2503.11951) contributes checkpointing and compensating actions for long-running workflows.

Recent SOTA frameworks implement these patterns: LangGraph and CrewAI (context7) provide shared state stores and checkpointable graphs; AutoGen and Letta (context7) offer agent registries and policy-based tool routing; MASFactory (arXiv:2603.06007), Agyn (arXiv:2602.01465), and CodeTeam (arXiv:2606.22082) all use shared task queues and role-based coordination with human oversight.

This spec introduces a shared blackboard, atomic task claiming, implicit cross-session visibility, an agent control plane with policy enforcement, SPOQ-style wave dispatch, Human-as-an-Agent escalation, and versioned run lifecycle checkpoints. These capabilities close the coordination gaps while staying within n00n's existing `n00n.session.*`, `n00n.agent.*`, and Lua plugin architecture.

## Clarifications

- Q: For this implementation pass, which user stories are in scope? A: All P1-P3 user stories are in scope.

## User Scenarios & Testing

### User Story 1 — Shared blackboard for cross-session coordination (Priority: P1)

As a developer running multiple concurrent `team` or `workflow` sessions, I want agents to post observations, claims, and status updates to a shared blackboard, so that sessions can coordinate without direct messaging and avoid duplicate work or file conflicts.

**Why this priority**: Foundational coordination substrate required by all other stories. Directly addresses MAST FM-2.1 (resource conflicts) and FM-2.3 (concurrency bugs). The blackboard pattern is well-established and maps cleanly to n00n's existing `plugins/team/mem.lua` and `n00n.storage`.

**Independent Test**: Start two concurrent `team` runs on the same project, have both agents post to the blackboard, and verify each session can read the other's posts and claims without race conditions.

**Acceptance Scenarios**:

1. **Given** two concurrent `team` sessions, **When** agent A posts a claim for file `src/main.rs`, **Then** agent B reading the blackboard immediately sees the claim and can avoid conflicting edits.
2. **Given** a blackboard with multiple posts, **When** an agent queries for posts tagged with a specific task ID, **Then** only posts matching that tag are returned in chronological order.
3. **Given** the blackboard service is unavailable, **When** an agent attempts to post, **Then** the operation fails gracefully with a clear error and the agent retries or falls back to local state.

---

### User Story 2 — Atomic task claiming to prevent duplicate work (Priority: P1)

As a developer, I want agents to claim tasks atomically from a shared queue, so that multiple agents never work on the same task simultaneously and work is distributed efficiently.

**Why this priority**: Prevents the most common coordination failure (duplicate work) with minimal complexity. Atomic claiming is a well-understood pattern (redis.io blog, amux.io) and can be implemented with existing `n00n.storage` primitives.

**Independent Test**: Spin up three agents, have them all attempt to claim from a task queue with five items, and verify each task is claimed exactly once with no double-claims.

**Acceptance Scenarios**:

1. **Given** a task queue with unclaimed items, **When** two agents simultaneously attempt to claim the same task, **Then** only one succeeds and the other receives a conflict error or the next available task.
2. **Given** a claimed task, **When** the claiming agent completes or fails the task, **Then** the claim is released and the task is marked done; failed tasks return to the queue with exponential backoff and a maximum of 3 retries.
3. **Given** an agent crashes while holding a claim, **When** a claim timeout expires, **Then** the claim is automatically released and the task becomes available for re-claiming.

---

### User Story 3 — Implicit live-session visibility without explicit joins (Priority: P1)

As a developer, I want to see all active agent sessions and their current status without manually joining or subscribing to each one, so that I can monitor progress and identify stuck or idle agents at a glance.

**Why this priority**: Provides operational visibility with zero configuration. Aligns with Control Plane as a Tool's observability requirements and TEA's lifecycle tracing. Can be built on top of `n00n.session.*` enumeration and the blackboard.

**Independent Test**: Start three concurrent `team` runs and one `workflow` run, then query the visibility API and verify all sessions appear with their current status, last activity timestamp, and active task.

**Acceptance Scenarios**:

1. **Given** multiple active sessions, **When** I query the session list, **Then** each session shows its session ID, agent type, current status (running/paused/error), last activity timestamp, and active task ID.
2. **Given** a session completes or errors, **When** I query the session list, **Then** the session is marked as terminated with its final status and exit reason.
3. **Given** no active sessions, **When** I query the session list, **Then** an empty list is returned without error.

---

### User Story 4 — Agent control plane with policy enforcement (Priority: P2)

As a developer, I want a centralized control plane that enforces policies on agent behavior (e.g., background agents cannot call write tools, paused agents cannot run any tools), so that long-running autonomous agents are safe and predictable.

**Why this priority**: Implements the Control Plane as a Tool pattern and TEA's policy layer. Critical for production safety but can be layered on top of P1 coordination. Builds on existing `plugins/agent_control` infrastructure.

**Independent Test**: Configure a policy that denies write tools to background agents, start a background agent, attempt a write operation, and verify it is blocked with a policy violation error.

**Acceptance Scenarios**:

1. **Given** a policy restricting write tools for agents with tag `background`, **When** a background agent attempts a write operation, **Then** the operation is rejected with a policy violation error and the agent continues without crashing.
2. **Given** a paused agent, **When** it attempts any tool call, **Then** the call is rejected with a `paused` status and the agent waits indefinitely for a `resume` or policy update unless an escalation timeout is configured.
3. **Given** no policy configured, **When** an agent performs any operation, **Then** all operations proceed without restriction.

---

### User Story 5 — SPOQ wave dispatch and dual validation gates (Priority: P2)

As a developer using `team`, I want the supervisor to dispatch agents in waves (e.g., planning wave, implementation wave, validation wave) with dual validation gates between waves, so that errors are caught early and rework is minimized.

**Why this priority**: Directly implements SPOQ's core innovation (wave dispatch + dual validation) which significantly reduces defect rates. Requires coordination infrastructure from P1 but adds value on top.

**Independent Test**: Run a `team` task with wave dispatch enabled, inject a defect in the implementation wave, and verify the validation wave catches it before the run completes.

**Acceptance Scenarios**:

1. **Given** a three-wave dispatch (plan, implement, validate), **When** the planning wave completes, **Then** a validation gate runs and only if it passes does the implementation wave start.
2. **Given** the implementation wave introduces a defect, **When** the validation wave runs, **Then** the defect is detected by failing tests or a rejected code review, the wave fails, and the run returns to the planning wave for correction.
3. **Given** a wave dispatch configuration, **When** a wave completes successfully, **Then** the next wave in the topology starts automatically with the validated artifacts as input.

---

### User Story 6 — Human-as-an-Agent escalation for unrecoverable failures (Priority: P2)

As a developer, when an agent encounters a failure it cannot resolve (e.g., ambiguous requirements, permission denied), I want it to escalate to me as a "human agent" so that I can provide input and the run can continue.

**Why this priority**: Implements SPOQ's Human-as-an-Agent pattern and ALMAS's human integration vision. Requires the control plane and visibility from P1/P2 but provides critical resilience.

**Independent Test**: Configure a task that requires human approval for a sensitive operation, run it, and verify the agent pauses, surfaces the escalation request, and resumes after human input.

**Acceptance Scenarios**:

1. **Given** an agent encounters an ambiguous requirement, **When** it cannot resolve after retry, **Then** it posts an escalation request to the blackboard and pauses with status `awaiting_human`.
2. **Given** a paused run awaiting human input, **When** I provide input via the control plane, **Then** the agent resumes with the human input as context and continues execution.
3. **Given** no human input within a timeout, **When** the escalation expires, **Then** the run terminates with a clear error indicating human input was required but not received.

---

### User Story 7 — Versioned run lifecycle and TEA checkpoints (Priority: P3)

As a developer, I want each agent run to have a versioned lifecycle with checkpoints (start, wave-complete, error, done) that can be inspected and replayed, so that I can debug failures and resume from intermediate states.

**Why this priority**: Implements TEA's versioned lifecycle and SagaLLM's checkpointing. Valuable for long-running workflows and debugging but lower priority than the coordination and control features.

**Independent Test**: Run a multi-wave `team` task, force a failure in the middle, and verify I can inspect the checkpoint history and resume from the last successful wave.

**Acceptance Scenarios**:

1. **Given** a multi-wave run, **When** each wave completes, **Then** a checkpoint is recorded with the wave ID, timestamp, state snapshot, and artifacts produced.
2. **Given** a run fails at wave 3, **When** I inspect the lifecycle, **Then** I see checkpoints for waves 1 and 2 and can choose to resume from wave 2 with modified parameters.
3. **Given** a completed run, **When** I query its lifecycle, **Then** I see a complete checkpoint history from start to done with all state transitions.

---

### Edge Cases

- **Blackboard unavailable**: agents fall back to local state and log the unavailability; operations that require coordination fail gracefully with clear errors.
- **Claim timeout race condition**: if a claim expires while an agent is still actively working, the agent can renew its claim before timeout; if renewal fails, the agent detects the lost claim and aborts or re-claims.
- **Concurrent visibility queries**: high query load on the visibility API should not block agent execution; queries are served from a cached snapshot updated periodically.
- **Policy conflict**: if multiple policies apply to an agent and conflict, the most restrictive policy wins and the conflict is logged.
- **Wave dispatch cycle**: if a validation gate fails repeatedly, the wave dispatch detects the cycle and aborts or escalates to human after a configurable retry limit.
- **Human escalation in non-interactive mode**: the run terminates with a structured payload indicating the escalation point; the human can later resume via the control plane with the required input.
- **Checkpoint corruption**: if a checkpoint cannot be loaded, the run aborts with a clear error and falls back to the last known good checkpoint or start.

## Requirements

### Functional Requirements

- **FR-001**: The system MUST provide a shared blackboard data structure that supports posting, reading, and querying observations, claims, and status updates with optional tags and task IDs.
- **FR-002**: The blackboard MUST support atomic claim operations where an agent can claim a task exclusively, renew claims, and release claims upon completion or failure.
- **FR-003**: The blackboard MUST enforce claim timeouts and automatically release expired claims to handle agent crashes.
- **FR-004**: The system MUST provide a visibility API that lists all active sessions with their session ID, agent type, current status, last activity timestamp, and active task ID.
- **FR-005**: The visibility API MUST support filtering by agent type, status, and time range, and MUST return results without blocking active agent execution.
- **FR-006**: The system MUST provide a control plane that enforces policies on agent behavior, including restricted tool sets, allowed tool sets, and pause/resume state.
- **FR-007**: The control plane MUST support policy definitions scoped by agent tag, session type, or individual agent ID; when multiple policies conflict, the most restrictive rule wins.
- **FR-008**: The `team` plugin MUST support wave-based dispatch where agents are organized into waves (e.g., plan, implement, validate) with configurable topologies.
- **FR-009**: Wave dispatch MUST include dual validation gates between waves that validate the output of one wave before starting the next.
- **FR-010**: Agents MUST be able to escalate to human by posting an escalation request to the blackboard and pausing with status `awaiting_human`.
- **FR-011**: The control plane MUST accept human input for paused runs and resume execution with the input as agent context.
- **FR-012**: Each agent run MUST record checkpoints at key lifecycle points (start, wave-complete, error, done) with state snapshots and artifacts.
- **FR-013**: The system MUST support inspecting checkpoint history and resuming runs from a specific checkpoint with modified parameters.

### Key Entities

- **Blackboard Post**: a record containing `{id, agent_id, timestamp, type (observation/claim/status), content, tags, task_id}`.
- **Task Claim**: a blackboard post of type `claim` with `{task_id, agent_id, claimed_at, expires_at, status}`.
- **Session Record**: a visibility entry containing `{session_id, agent_type, status, last_activity, active_task_id, metadata}`.
- **Policy Rule**: a control plane definition containing `{id, scope (tag/session/agent), restricted_tools, allowed_tools, paused, priority}`.
- **Wave**: a dispatch stage containing `{wave_id, agent_types, input_artifacts, validation_gate, output_artifacts}`.
- **Escalation Request**: a blackboard post of type `escalation` with `{run_id, reason, required_input, timeout, status}`.
- **Checkpoint**: a lifecycle record containing `{checkpoint_id, run_id, wave_id, timestamp, state_snapshot, artifacts, status}`.

## Success Criteria

### Measurable Outcomes

- **SC-001**: In a test with 10 concurrent agents claiming from a 50-task queue, no task is simultaneously claimed by more than one agent at any point in time.
- **SC-002**: Visibility API p95 latency is under 100ms for up to 100 active sessions and queries do not block agent execution.
- **SC-003**: Policy enforcement p95 latency is under 50ms from the restricted operation attempt.
- **SC-004**: Wave dispatch with dual validation gates reduces defect injection rate by at least 30% compared to non-wave dispatch on the same 20-task benchmark suite.
- **SC-005**: Human escalation requests are surfaced and paused runs resume successfully after human input in 95% of 50 representative escalation scenarios.
- **SC-006**: Checkpoint recording adds less than 5% overhead to run duration and checkpoints can be loaded and resumed without data loss.

## Assumptions

- All P1-P3 user stories are in scope for this implementation pass.
- n00n keeps its existing `n00n.session.*`, `n00n.agent.*`, and Lua plugin architecture; no core rewrite is required.
- The blackboard is implemented using existing `n00n.storage` primitives; external databases like Redis are out of scope.
- SDLC tool integrations (Jira, GitHub Issues, Bitbucket) remain out of scope as in spec 001.
- Policy enforcement is local to the n00n process; distributed policy across multiple n00n instances is out of scope.
- Checkpoint state snapshots are limited to agent state and blackboard artifacts; full process or filesystem snapshots are out of scope.

## Implementation Notes (non-binding, for planning)

- The blackboard can be implemented as a new `plugins/blackboard` Lua module backed by `n00n.storage` with in-memory caching for reads.
- Atomic claiming can use `n00n.storage` compare-and-swap or a dedicated lock primitive; claim renewal and timeout cleanup should run as a background task via `n00n.async.gather`.
- Visibility API can build on `n00n.session.*` enumeration and the blackboard; consider a cached snapshot updated every few seconds to avoid blocking.
- Control plane policy enforcement can hook into the existing `plugins/agent_control` tool routing layer; policies are loaded from a config file and evaluated before tool execution.
- Wave dispatch extends the `plugins/team` supervisor with a wave topology configuration; validation gates are implemented as additional agent steps between waves.
- Human escalation reuses the `agent_control` pause/resume actions from spec 001; escalation requests are blackboard posts that the control plane monitors.
- Checkpointing can integrate with the existing `plugins/workflow` journal and `n00n.telemetry`; state snapshots are serialized as JSON to the run directory.
- All new coordination features should be optional via feature flags to maintain backward compatibility with single-session workflows.
