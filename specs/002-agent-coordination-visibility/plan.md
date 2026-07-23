# Implementation Plan: ALMAS expansion - cross-session visibility, blackboard coordination, and agent control plane

**Branch**: `spec/almas-coordination-visibility` | **Date**: 2026-07-23 | **Spec**: spec.md

**Input**: Feature specification from `/specs/002-agent-coordination-visibility/spec.md`

## Summary

This feature introduces cross-session coordination for n00n's multi-agent layer through a shared blackboard, atomic task claiming, implicit live-session visibility, an agent control plane with policy enforcement, SPOQ-style wave dispatch, Human-as-an-Agent escalation, and versioned run lifecycle checkpoints. The approach extends the existing Lua plugin architecture with new coordination primitives (blackboard, waves, validation, live context, checkpoint) while reusing n00n.storage for persistence and n00n.session for session management. All coordination features are optional via feature flags to maintain backward compatibility with single-session workflows.

## Technical Context

**Language/Version**: Rust 1.94+, Luau (via n00n-lua)

**Primary Dependencies**: Existing workspace crates (n00n-lua, n00n-storage, n00n-agent), no new external dependencies required

**Storage**: Files under state_dir/projects/{pid}/ for blackboard (JSONL), checkpoints (JSON snapshots), and run directories (existing team/workflow layout)

**Testing**: cargo nextest run --workspace; Lua plugin tests via n00n.api.register_tool

**Target Platform**: Linux

**Project Type**: CLI/TUI with Lua plugin system

**Performance Goals**: Visibility API queries return within 100ms for up to 100 active sessions; policy enforcement within 50ms; checkpoint recording adds less than 5% overhead

**Constraints**: No unwrap/expect/panic in production; workspace clippy must pass; no unsafe without review; all new code must pass workspace lints

**Scale/Scope**: Multi-agent coordination across concurrent sessions within a single project; blackboard scales to 10+ concurrent agents with 50+ task queue

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

The constitution file contains only placeholders; no hard constraints beyond the project AGENTS.md rules. All new code must pass workspace clippy and avoid unwrap/expect/panic; no unsafe blocks without written review. The implementation follows existing n00n patterns (Lua plugins, n00n.storage, n00n.session) and does not introduce new architectural paradigms that would conflict with placeholder principles.

## Project Structure

### Documentation (this feature)

```text
specs/002-agent-coordination-visibility/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── blackboard_tool.md
│   └── agent_control_policy.md
└── tasks.md             # Phase 2 output (NOT created by /speckit.plan)
```

### Source Code (repository root)

```text
plugins/
├── blackboard/          # NEW: Shared coordination substrate
│   └── init.lua
├── team/
│   ├── init.lua         # EXTEND: wave dispatch, escalation hooks
│   ├── waves.lua        # NEW: topological wave computation
│   └── validation.lua   # NEW: dual validation gates
├── agent_control/
│   └── init.lua         # EXTEND: policy enforcement layer
├── lib/n00n/
│   ├── live_context.lua # NEW: visibility API composition
│   └── checkpoint.lua   # NEW: lifecycle checkpoint primitives
n00n-lua/src/api/
├── session.rs           # EXTEND: policy hook if needed for tool_dispatch
└── agent.rs             # EXTEND: policy hook if needed
```

**Structure Decision**: Single Rust workspace with Lua plugins. New coordination modules are added as Lua plugins to stay within the existing plugin architecture and avoid core Rust changes. The blackboard is a separate plugin (not extending memory) because it requires task-claiming semantics and atomicity that the memory plugin does not provide. Wave dispatch and validation are separate team modules to keep the supervisor focused on orchestration. Live context and checkpoint are shared library modules for reuse across team and workflow.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No constitution violations. The new plugins/modules are justified:

- **plugins/blackboard**: New because memory plugin has no task-claiming/atomicity; blackboard requires exclusive claims, timeouts, and coordination-specific queries.
- **plugins/team/waves.lua**: New because wave dispatch requires topological computation from plan step dependencies, which is complex enough to warrant separation.
- **plugins/team/validation.lua**: New because dual validation gates are a distinct concern from step execution and require reusable validation logic.
- **plugins/lib/n00n/live_context.lua**: New because visibility composition (session.live + telemetry + blackboard) is shared across team and workflow.
- **plugins/lib/n00n/checkpoint.lua**: New because checkpoint lifecycle (save, load, resume) is shared across team and workflow runs.
