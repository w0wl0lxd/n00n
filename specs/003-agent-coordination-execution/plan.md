# Implementation Plan: Agent Coordination Execution

**Branch**: `spec/almas-coordination-followup` | **Date**: 2026-07-23 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/003-agent-coordination-execution/spec.md`

## Summary

Integrate the ALMAS/SPOQ execution model on top of the blackboard foundation:
- `team` gains opt-in wave dispatch with validation gates and checkpoint persistence.
- `agent_control` policy storage becomes enforced at tool-call time.
- `live_context` queries the blackboard to enrich session visibility.

## Technical Context

**Language/Version**: Rust 2024, Lua 5.1/Luau via mlua

**Primary Dependencies**: Existing n00n workspace crates; no new dependencies.

**Storage**: Files under `n00n.env.state_dir()` via `n00n.fs` and `n00n.json`.

**Testing**: `cargo nextest run --workspace`, `cargo test -p n00n-lua`.

**Target Platform**: Linux command-line agent.

**Project Type**: Rust workspace with Lua built-in plugins.

**Performance Goals**: Keep `team` wave overhead under one additional LLM call per gate.

**Constraints**: No Rust core changes; all behavior implemented in Lua plugins using existing `n00n.*` APIs.

## Constitution Check

- KISS: Each user story is a small, isolated change.
- DRY: Reuse `blackboard`, `checkpoint`, `live_context`, `waves`, `validation` modules.
- SRP: `team` orchestrates, `agent_control` enforces, `live_context` reports.
- No unsafe, no unwrap, no new dependencies.

## Project Structure

### Documentation (this feature)

```text
specs/003-agent-coordination-execution/
├── spec.md
├── checklists/requirements.md
├── plan.md
├── tasks.md
└── research.md
```

### Source Code (repository root)

```text
plugins/
├── team/init.lua           # add waves/checkpoint integration
├── team/waves.lua          # expand grouping, add validation runner
├── team/validation.lua     # gate implementation
├── agent_control/init.lua  # add policy enforcement
├── lib/n00n/live_context.lua # query blackboard
├── lib/n00n/checkpoint.lua   # no changes needed
└── blackboard/init.lua     # no changes needed

n00n-lua/tests/
└── blackboard.rs           # expand for wave/checkpoint tests
```

## Complexity Tracking

No complexity violations expected; all work stays within existing plugin architecture.
