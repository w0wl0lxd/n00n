# Data Model: ALMAS coordination visibility

**Feature**: specs/002-agent-coordination-visibility  
**Date**: 2026-07-23  
**Status**: Phase 1 complete

## Overview

This document defines the entities, fields, relationships, validation rules, and state transitions for the ALMAS coordination visibility feature. The data model is implemented as Lua tables persisted via n00n.storage (JSONL for blackboard, JSON for checkpoints).

## Entities

### Blackboard Post

A record on the shared blackboard containing observations, claims, or status updates.

**Fields**:
- `id` (string, required): Unique post identifier (UUID or hash).
- `agent_id` (string, required): ID of the agent that created the post.
- `timestamp` (integer, required): Unix timestamp in seconds.
- `type` (string, required): Post type - one of "observation", "claim", "status", "escalation".
- `content` (string, required): Post content (free-form text or structured JSON).
- `tags` (array of string, optional): Tags for filtering and querying.
- `task_id` (string, optional): Associated task ID for task-scoped posts.

**Validation rules**:
- `id` must be non-empty and unique across all posts.
- `agent_id` must be a valid session ID from n00n.session.live().
- `timestamp` must be a positive integer.
- `type` must be one of the allowed values.
- `content` must be non-empty.
- `tags` array elements must be non-empty strings.
- `task_id` must be non-empty if present.

**State transitions**: Posts are immutable once written. Updates create new posts with the same task_id.

### Task Claim

A blackboard post of type "claim" representing an exclusive task assignment.

**Fields** (extends Blackboard Post):
- `task_id` (string, required): Unique task identifier.
- `agent_id` (string, required): ID of the agent holding the claim.
- `claimed_at` (integer, required): Unix timestamp when the claim was acquired.
- `expires_at` (integer, required): Unix timestamp when the claim expires.
- `status` (string, required): Claim status - one of "claimed", "released", "done", "failed".

**Validation rules**:
- `task_id` must be non-empty and unique across active claims.
- `agent_id` must be a valid session ID.
- `claimed_at` must be less than or equal to `expires_at`.
- `expires_at` must be in the future (relative to claim time).
- `status` must be one of the allowed values.

**State transitions**:
- `claimed` -> `released`: Agent releases the claim voluntarily.
- `claimed` -> `done`: Agent completes the task successfully.
- `claimed` -> `failed`: Agent fails the task; task returns to queue.
- `claimed` -> `claimed` (renewal): Agent renews the claim before expiration.
- Any state -> expired (system): Claim expires if not renewed before `expires_at`.

### Session Record

A visibility entry representing an active or terminated agent session.

**Fields**:
- `session_id` (string, required): Unique session identifier.
- `agent_type` (string, required): Agent type - one of "team", "workflow", "task", "custom".
- `status` (string, required): Session status - one of "running", "paused", "needs_input", "error", "done".
- `last_activity` (integer, required): Unix timestamp of last activity.
- `active_task_id` (string, optional): ID of the task currently being processed.
- `metadata` (object, optional): Free-form metadata (e.g., goal, model, tier).

**Validation rules**:
- `session_id` must be a valid session ID from n00n.session.
- `agent_type` must be one of the allowed values.
- `status` must be one of the allowed values.
- `last_activity` must be a positive integer.
- `active_task_id` must be non-empty if present.

**State transitions**:
- `running` -> `paused`: Agent paused via control plane.
- `running` -> `needs_input`: Agent awaiting human input (escalation).
- `running` -> `error`: Agent encountered an error.
- `running` -> `done`: Agent completed successfully.
- `paused` -> `running`: Agent resumed via control plane.
- `needs_input` -> `running`: Human input provided, agent resumes.
- `error` -> `running`: Agent retried or recovered.
- Any state -> terminated: Session deleted or terminated.

### Policy Rule

A control plane definition restricting agent behavior.

**Fields**:
- `id` (string, required): Unique policy identifier.
- `scope` (object, required): Policy scope - one of:
  - `tag` (string): Applies to agents with this tag.
  - `session_type` (string): Applies to sessions of this type.
  - `agent_id` (string): Applies to a specific agent.
- `restricted_tools` (array of string, optional): Tools that agents in scope cannot use.
- `allowed_tools` (array of string, optional): Tools that agents in scope can use (whitelist mode).
- `paused` (boolean, required): Whether agents in scope are paused.
- `priority` (integer, required): Policy priority (higher wins on conflict).

**Validation rules**:
- `id` must be non-empty and unique.
- `scope` must have exactly one of `tag`, `session_type`, or `agent_id`.
- `restricted_tools` and `allowed_tools` cannot both be non-empty (mutually exclusive).
- Tool names must be valid tool names from the tool registry.
- `priority` must be a non-negative integer.

**State transitions**: Policies are immutable once created. Updates create new policy rules with higher priority.

### Wave

A dispatch stage in wave-based execution.

**Fields**:
- `wave_id` (string, required): Unique wave identifier.
- `agent_types` (array of string, required): Agent types in this wave.
- `input_artifacts` (array of string, optional): Artifacts from previous waves.
- `validation_gate` (object, optional): Validation gate configuration.
- `output_artifacts` (array of string, optional): Artifacts produced by this wave.

**Validation rules**:
- `wave_id` must be non-empty and unique within a run.
- `agent_types` must be non-empty and contain valid agent types.
- `input_artifacts` must reference existing artifacts if present.
- `validation_gate` must have a valid validation function if present.
- `output_artifacts` are populated after wave completion.

**State transitions**:
- `pending` -> `running`: Wave starts execution.
- `running` -> `passed`: Wave completes and validation gate passes.
- `running` -> `failed`: Wave fails or validation gate fails.
- `failed` -> `retrying`: Wave is retried after correction.
- `passed` -> `done`: Wave is marked done and next wave starts.

### Escalation Request

A blackboard post of type "escalation" requesting human intervention.

**Fields** (extends Blackboard Post):
- `run_id` (string, required): ID of the run that escalated.
- `reason` (string, required): Human-readable reason for escalation.
- `required_input` (string, required): Description of input needed from human.
- `timeout` (integer, required): Unix timestamp when escalation expires.
- `status` (string, required): Escalation status - one of "pending", "answered", "expired".

**Validation rules**:
- `run_id` must be a valid run ID.
- `reason` must be non-empty.
- `required_input` must be non-empty.
- `timeout` must be in the future.
- `status` must be one of the allowed values.

**State transitions**:
- `pending` -> `answered`: Human provides input via control plane.
- `pending` -> `expired`: Timeout reached without input.
- `answered` -> `resolved`: Agent resumes with human input.

### Checkpoint

A lifecycle record capturing run state at key points.

**Fields**:
- `checkpoint_id` (string, required): Unique checkpoint identifier.
- `run_id` (string, required): ID of the run.
- `wave_id` (string, optional): ID of the wave (if applicable).
- `timestamp` (integer, required): Unix timestamp.
- `state_snapshot` (object, required): Agent state and blackboard artifacts.
- `artifacts` (array of string, optional): Paths to artifacts produced.
- `status` (string, required): Checkpoint status - one of "start", "wave_complete", "error", "done".

**Validation rules**:
- `checkpoint_id` must be non-empty and unique within a run.
- `run_id` must be a valid run ID.
- `wave_id` must be a valid wave ID if present.
- `state_snapshot` must be a valid JSON object.
- `artifacts` must reference existing files if present.
- `status` must be one of the allowed values.

**State transitions**: Checkpoints are immutable once written. New checkpoints are created at lifecycle transitions.

### Live Context Snapshot

A cached snapshot of live sessions and blackboard status for visibility queries.

**Fields**:
- `snapshot_id` (string, required): Unique snapshot identifier.
- `timestamp` (integer, required): Unix timestamp.
- `sessions` (array of Session Record, required): Current live sessions.
- `blackboard_summary` (object, required): Summary of blackboard posts and claims.
- `cache_ttl` (integer, required): Time-to-live in seconds.

**Validation rules**:
- `snapshot_id` must be non-empty.
- `timestamp` must be a positive integer.
- `sessions` must be an array of valid Session Records.
- `blackboard_summary` must contain post counts and active claim counts.
- `cache_ttl` must be positive.

**State transitions**: Snapshots are regenerated periodically or on demand. Old snapshots expire after `cache_ttl`.

## Relationships

- **Blackboard Post** -> **Session Record**: `agent_id` references `session_id`.
- **Task Claim** -> **Session Record**: `agent_id` references `session_id`.
- **Task Claim** -> **Blackboard Post**: Task claims are a subtype of posts.
- **Escalation Request** -> **Checkpoint**: `run_id` references the run's checkpoints.
- **Wave** -> **Checkpoint**: Waves create checkpoints on completion.
- **Session Record** -> **Policy Rule**: Sessions are subject to policies matching their scope.
- **Live Context Snapshot** -> **Session Record**: Snapshots aggregate session records.
- **Live Context Snapshot** -> **Blackboard Post**: Snapshots aggregate blackboard state.

## Storage Layout

```
state_dir/projects/{pid}/
├── blackboard/
│   └── posts.jsonl          # Append-only blackboard posts
├── runs/
│   └── {run_id}/
│       ├── checkpoints/
│       │   ├── {checkpoint_id}.json
│       │   └── ...
│       ├── events.jsonl     # Telemetry events
│       └── journal.jsonl     # Workflow journal
└── policies/
    └── policy.json          # Policy rules (optional)
```

## Validation Summary

All entities follow these validation principles:
- Required fields must be present and non-empty.
- Enum fields must match allowed values.
- Timestamps must be positive integers.
- References (IDs) must be valid and exist in the target entity.
- Arrays must contain valid elements of the specified type.
- Mutually exclusive fields cannot both be present.
