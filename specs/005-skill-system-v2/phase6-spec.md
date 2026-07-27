# Feature Specification: Skill System V2 — Phase 6 (Agent Integration)

**Feature Branch**: `feat/skill-system-v2`  
**Created**: 2026-07-27  
**Status**: Draft  
**Input**: Enforce skill tool policy in the agent loop, add graph-informed ranking signals, skill telemetry, and structured execution plans.

## User Scenarios & Testing

### User Story 1 — Hard skill policy enforcement (Priority: P1)

As a user running an agent under an active skill with `allowed-tools` / `disallowed-tools`, I want disallowed tool calls blocked by the agent runtime before execution.

**Why this priority**: Soft policy (instructions only) is bypassable; hard enforcement is required for safety envelopes.

**Independent Test**: Load a skill with `allowed-tools: read`, then invoke `bash` in the same session; agent returns policy error without executing bash.

**Acceptance Scenarios**:
1. **Given** a skill with `disallowed-tools: bash`, **When** the agent calls `bash`, **Then** the call is rejected with a skill-policy error.
2. **Given** a skill with `allowed-tools: read, grep`, **When** the agent calls `grep`, **Then** the call proceeds.
3. **Given** a successful `skill` load without tool policy, **When** any tool is called, **Then** no skill policy gate applies.

---

### User Story 2 — Graph-informed ranking signals (Priority: P2)

As an agent with a focus file and indexed project graph, I want skill list ranking boosted when graph indexes are available.

**Why this priority**: Improves relevance without expensive per-list graph queries.

**Independent Test**: With `.codegraph/` or arbor index present, `list=true, rank=true, graph_rank=true` boosts path-scoped skills.

**Acceptance Scenarios**:
1. **Given** arbor index available and path-scoped skill, **When** `graph_rank=true`, **Then** skill score includes graph bonus.
2. **Given** no graph index, **When** `graph_rank=true`, **Then** heuristic ranking still works.

---

### User Story 3 — Skill telemetry (Priority: P2)

As an operator, I want skill list/load events recorded for hit-rate and cost analysis.

**Independent Test**: After skill list/load, telemetry file contains structured event rows.

**Acceptance Scenarios**:
1. **Given** `include_telemetry=true`, **When** skill list runs, **Then** output includes telemetry summary.
2. **Given** skill load, **When** telemetry enabled, **Then** event is appended to project skill telemetry log.

---

### User Story 4 — Structured execution plans (Priority: P2)

As an agent, I want frontmatter `steps` with per-step tool intents so I can follow a skill workflow without loading the full body.

**Independent Test**: Skill with `steps` frontmatter returns structured plan via `plan=true`.

**Acceptance Scenarios**:
1. **Given** `steps` frontmatter, **When** `plan=true`, **Then** output lists steps with tool intents.
2. **Given** no `steps`, **When** `plan=true`, **Then** fallback section/step extraction still works.

## Functional Requirements

- **FR-101**: Agent MUST enforce active skill tool policy in `tool_dispatch` before native tool execution.
- **FR-102**: Successful `skill` tool results MUST update or clear `active_skill_policy` on the agent session.
- **FR-103**: `graph_rank=true` MUST add bounded graph-index bonuses to relevance scoring when indexes exist.
- **FR-104**: Skill telemetry MUST append JSONL events under project state when enabled.
- **FR-105**: `steps` frontmatter MUST normalize to structured step records with optional `tools` and `section`.

## Success Criteria

- **SC-101**: Agent-level policy enforcement integration tests pass.
- **SC-102**: Graph rank and telemetry tests pass in Lua spec and plugin_host.
- **SC-103**: Structured plan tests pass for `steps` frontmatter.
- **SC-104**: All prior `skill_tool_*` tests remain green.
