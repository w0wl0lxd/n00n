# Feature Specification: Agent Coordination Execution

**Feature Branch**: `spec/almas-coordination-followup`

**Created**: 2026-07-23

**Status**: Draft

**Input**: User description: "Follow-up work for the ALMAS multi-agent coordination feature: integrate SPOQ wave dispatch, checkpoint persistence, agent control policy enforcement, and enhanced live context as a stacked PR on top of the blackboard foundation."

## User Scenarios & Testing

### User Story 1 - Wave dispatch for staged agent execution (Priority: P1)

A user running `team` with a complex SDLC goal wants the planner, implementer, and reviewer roles to execute in distinct waves rather than a single sequential pass, so that planning quality is validated before implementation and implementation is validated before review.

**Why this priority**: Without staged execution, a poor plan can cause cascading implementation errors. SPOQ research shows wave-based topological dispatch with dual validation gates catches quality issues when they are cheapest to fix.

**Independent Test**: Run `team` with `waves = true` on a goal and observe that product_manager/sprint/planner steps complete before any developer steps, and developer steps complete before tester/reviewer steps, with validation gates between waves.

**Acceptance Scenarios**:

1. **Given** a `team` invocation with `waves = true`, **When** the plan contains only planning roles, **Then** the first wave executes only planning roles and a plan validation gate runs before implementation.
2. **Given** a plan validation gate failure, **When** the gate reports the plan is insufficient, **Then** the implementation wave is deferred and the plan wave is re-run with feedback.
3. **Given** a `team` invocation without `waves`, **When** the plan executes, **Then** behavior remains the existing sequential mode.

---

### User Story 2 - Run checkpoints for failure recovery (Priority: P1)

A user running `team` wants the system to save checkpoints after each wave so that, if the run is interrupted or a wave fails, the run can be resumed from the last successful wave.

**Why this priority**: Long multi-agent runs can fail or be paused. Checkpoint persistence aligns with TEA-style lifecycle patterns and enables `resume` to continue without re-doing completed work.

**Independent Test**: Run `team` with `checkpoints = true`, interrupt after a wave, then resume with the same `run_id` and observe that completed steps are skipped.

**Acceptance Scenarios**:

1. **Given** a `team` run with `checkpoints = true`, **When** a wave completes successfully, **Then** a checkpoint is persisted under `state_dir/projects/{pid}/runs/{run_id}/checkpoints/`.
2. **Given** a previously checkpointed run, **When** `team` is invoked with `resume = run_id`, **Then** it loads the latest checkpoint and continues from the next uncompleted step.
3. **Given** a resumed run, **When** all remaining steps complete, **Then** the final result includes the prior wave results plus the new wave results.

---

### User Story 3 - Agent control policy enforcement (Priority: P2)

A user managing background agents wants to attach policy rules to agents (by tag, session type, or agent id) that restrict or allow specific tools, so that autonomous agents cannot execute disallowed actions.

**Why this priority**: The control plane in the previous PR can store policies but does not enforce them. Enforcement turns policies from metadata into a real trust boundary.

**Independent Test**: Create a policy that restricts `bash` for a tag, start a `team` run for an agent with that tag, and observe that `bash` calls are rejected.

**Acceptance Scenarios**:

1. **Given** a policy rule with `restricted_tools = ["bash"]`, **When** an agent in scope calls `bash`, **Then** the call is rejected with a clear policy violation message.
2. **Given** a policy rule with `allowed_tools = ["read", "write"]`, **When** an agent in scope calls `blackboard`, **Then** the call is rejected because it is not in the allowlist.
3. **Given** an agent that matches no policy, **When** it calls any tool, **Then** the call proceeds normally.

---

### User Story 4 - Enhanced live context for cross-session visibility (Priority: P2)

A user viewing active sessions wants to see each session's current task, active claims, and recent blackboard posts so that they can understand what every agent is doing without joining sessions explicitly.

**Why this priority**: This completes User Story 3 of the previous spec by actually combining `n00n.session.live()` with blackboard state rather than returning only session metadata.

**Independent Test**: Call `live_context.snapshot()` while a `team` run is active and verify the returned entries include the current run_id, active claim, and latest blackboard status post.

**Acceptance Scenarios**:

1. **Given** an active `team` run with `run_id = X`, **When** `live_context.snapshot()` is called, **Then** the result contains an entry with `active_task_id = X` and a non-empty `recent_posts` list.
2. **Given** a session with an active blackboard claim, **When** `live_context.snapshot()` is called, **Then** the result maps the claim to the session entry.
3. **Given** no active sessions, **When** `live_context.snapshot()` is called, **Then** it returns an empty list without error.

---

### Edge Cases

- What happens when `waves = true` and the plan has no steps in a given wave category? The wave is skipped and a validation gate is not run.
- What happens when `waves = true` and validation fails repeatedly? A configurable maximum retry count (default 3) prevents infinite replan loops.
- What happens when a checkpoint file is corrupted? The run falls back to starting from the beginning of the current wave and logs a warning.
- What happens when two policies apply to the same agent? Higher priority wins; ties are broken by most specific scope (agent_id > session_type > tag).
- What happens when an agent is paused by policy while a tool is in flight? The in-flight tool completes, and the next prompt is paused via `n00n.session.cancel`.
- What happens when `live_context` cannot reach the blackboard? It returns session metadata with empty claims and posts rather than failing.

## Requirements

### Functional Requirements

- **FR-001**: The `team` tool MUST support a `waves` boolean option defaulting to false.
- **FR-002**: When `waves` is true, `team` MUST group plan steps into plan, implement, and validate waves based on role.
- **FR-003**: When `waves` is true, `team` MUST run a validation gate between waves and before the first wave.
- **FR-004**: The validation gate MUST accept the wave name, prior results, and goal, and return PASS or FAIL with an explanation.
- **FR-005**: On validation failure, `team` MUST re-run the preceding wave with feedback up to a configurable maximum retry count.
- **FR-006**: The `team` tool MUST support a `checkpoints` boolean option defaulting to false.
- **FR-007**: When `checkpoints` is true, `team` MUST persist a checkpoint after each successfully completed wave.
- **FR-008**: When `resume` is provided, `team` MUST load the latest checkpoint for that run_id and resume from the next step.
- **FR-009**: The `agent_control` tool MUST enforce `restricted_tools` and `allowed_tools` policies at tool-call time.
- **FR-010**: Policy evaluation MUST support scope by `tag`, `session_type`, and `agent_id` with deterministic precedence.
- **FR-011**: The `live_context` module MUST query the blackboard for active claims and recent status posts and merge them with `n00n.session.live()` output.
- **FR-012**: All new options MUST be documented in the `team` and `agent_control` tool schemas.

### Key Entities

- **Wave**: A group of plan steps, a wave name, and an optional validation gate result.
- **Checkpoint**: A persisted snapshot of a `team` run containing run_id, wave index, step index, results, and timestamp.
- **Policy Rule**: A scoped rule with optional restricted/allowed tool lists, paused flag, and priority.
- **Live Context Entry**: A session enriched with active task_id, claims, and recent blackboard posts.

## Success Criteria

### Measurable Outcomes

- **SC-001**: `team` with `waves = true` produces distinct plan, implement, and validate phases in 100% of plans with those roles.
- **SC-002**: `team` with `checkpoints = true` can resume after interruption and skip completed steps with 0 re-execution of those steps.
- **SC-003**: `agent_control` policies reject at least 95% of disallowed tool calls in targeted tests.
- **SC-004**: `live_context.snapshot()` returns active task and claim information for 100% of sessions that posted to the blackboard.
- **SC-005**: `cargo nextest run --workspace` passes with no new failures introduced by follow-up changes.

## Assumptions

- The blackboard plugin from `specs/002-agent-coordination-visibility` is available and registered as a built-in tool.
- The `team` tool's existing sequential and swarm modes remain the default; wave execution is opt-in.
- Wave dispatch runs within the same session and does not spawn parallel subagents in this iteration.
- Policy enforcement is implemented at the Lua tool-call boundary, not as a Rust middleware change.
- `n00n.agent.call_tool` is the correct primitive for blackboard posts and policy-filtered tool calls.
