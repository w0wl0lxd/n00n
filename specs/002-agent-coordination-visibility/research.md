# Research: ALMAS coordination visibility - Phase 0 decisions

**Feature**: specs/002-agent-coordination-visibility  
**Date**: 2026-07-23  
**Status**: Phase 0 complete

## Overview

This document captures the research decisions for the ALMAS coordination visibility feature. Each unknown from the spec is resolved with a decision and rationale.

## Decisions

### Blackboard: new plugin vs extend memory

**Decision**: Create a new `plugins/blackboard` plugin for a clean task-claiming/coordination API. The memory plugin remains a scratchpad.

**Rationale**: The memory plugin is designed for persistent, project-scoped learnings with simple read/write/delete operations. It lacks atomic claiming, task queues, claim timeouts, and coordination-specific queries. Adding these to memory would bloat its scope and break its simple scratchpad abstraction. A separate blackboard plugin provides a clean coordination API (write, read, claim_task, release_task, update_task, query) that maps directly to the blackboard pattern and can be used by team, workflow, and other plugins without coupling them to memory's storage format.

### Atomicity: file lock vs SQLite

**Decision**: Use file lock/JSONL for MVP. SQLite is overkill and adds a dependency.

**Rationale**: The blackboard needs atomic claim operations to prevent duplicate work. A per-project lockfile combined with JSONL append-only storage provides sufficient atomicity for the MVP scale (10+ concurrent agents, 50+ task queue). SQLite would add a dependency and complexity (schema migrations, connection pooling) that is not justified for this use case. The existing n00n.storage primitives (n00n.fs.read/write with atomic renames) are sufficient. If scale requirements grow beyond the MVP, the storage layer can be swapped without changing the blackboard API.

### Live context source

**Decision**: Combine n00n.session.live() + telemetry events.jsonl + blackboard status. No new event bus.

**Rationale**: The visibility API needs to show active sessions, their status, and coordination state. n00n.session.live() already provides session enumeration with status and metadata. The telemetry module (plugins/lib/n00n/telemetry.lua) already writes events.jsonl for team/workflow runs. The blackboard provides task claims and posts. Combining these sources in a new live_context.lua module provides a complete view without introducing a new event bus infrastructure. This approach reuses existing data paths and avoids the complexity of a pub/sub system.

### Policy enforcement layer

**Decision**: Implement policy in Lua agent_control to restrict forwarding tool calls to paused/restricted sessions. Later gate in n00n-agent tool_dispatch.

**Rationale**: The control plane needs to enforce policies on agent behavior (e.g., background agents cannot call write tools, paused agents cannot run any tools). Implementing the policy layer in Lua agent_control allows rapid iteration and keeps policy logic in the plugin layer where it can be configured per project. The initial enforcement happens at the tool call level in Lua by checking the agent's state and policy table before forwarding the call. Later, a Rust hook in n00n-agent's tool_dispatch can add a second enforcement layer for safety-critical restrictions. This phased approach allows us to validate the policy model before adding Rust complexity.

### Wave dispatch topology

**Decision**: Derive from step dependencies in team plan, compute topological waves.

**Rationale**: SPOQ wave dispatch requires organizing agents into waves (e.g., plan, implement, validate) with dependencies between waves. The team supervisor already produces a plan with ordered steps. By deriving the wave topology from step dependencies (e.g., steps 1-3 are planning wave, steps 4-8 are implementation wave, steps 9-10 are validation wave), we avoid requiring users to manually specify wave structure. A topological sort of the plan steps with configurable wave boundaries provides automatic wave computation. This approach is flexible (supports custom wave configurations) and requires no additional user input for common cases.

### Checkpoint storage

**Decision**: JSON snapshots in run directory under state_dir/projects/{pid}/runs/{run_id}/checkpoints/.

**Rationale**: Checkpoints need to capture agent state, blackboard artifacts, and run metadata at key lifecycle points (start, wave-complete, error, done). The existing team/workflow plugins already use run directories under state_dir/projects/{pid}/ for telemetry and journal files. Storing checkpoints as JSON snapshots in a checkpoints/ subdirectory reuses this layout and keeps all run artifacts together. JSON is sufficient for the state snapshot (agent state, blackboard posts, artifacts) and integrates with the existing n00n.json encode/decode utilities. No new storage infrastructure is required.

## Summary

All research decisions favor simplicity, reuse of existing infrastructure, and phased implementation. The blackboard, wave dispatch, live context, and checkpoint features are implemented as Lua plugins/modules that extend the existing architecture without requiring core Rust changes. Policy enforcement starts in Lua and can be hardened in Rust later. Atomicity uses file locks and JSONL to avoid database dependencies. This approach keeps the feature within the n00n plugin paradigm and allows rapid iteration.
