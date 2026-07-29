# Phase 6 Tasks: Agent Integration

## Phase 6A: Rust policy enforcement

- [x] T026 Add `skill_policy.rs` with `ActiveSkillPolicy`, `evaluate`, `from_state` (TDD).
- [x] T027 Add `active_skill_policy` to `ToolContext` and agent lifecycle.
- [x] T028 Gate `tool_dispatch::run` on active skill policy.
- [x] T029 Add `n00n-agent` unit tests for policy enforcement.

## Phase 6B: Graph rank, telemetry, execution plans

- [x] T030 Add `skill_telemetry.lua` and `include_telemetry` option.
- [x] T031 Add `graph_rank` with index-presence bonuses.
- [x] T032 Parse `steps` frontmatter and structured `plan=true` output.
- [x] T033 Add Lua spec and `plugin_host` integration tests.

## Phase 6C: Validation

- [x] T034 Run `cargo test -p n00n-agent skill_policy`.
- [x] T035 Run `cargo test -p n00n-lua skill_tool_` and `spec`.
- [x] T036 Regenerate docs (`just gen-docs`).
