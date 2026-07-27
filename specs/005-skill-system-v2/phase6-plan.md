# Implementation Plan: Skill System V2 Phase 6

**Branch**: `feat/skill-system-v2` | **Date**: 2026-07-27 | **Spec**: phase6-spec.md

## Summary

Wire skill tool policy into the Rust agent dispatch path, add lightweight graph-index ranking bonuses, skill telemetry JSONL, and structured `steps` frontmatter for execution plans.

## Technical Context

- **Language**: Rust 1.94+ (n00n-agent), Luau (plugins/skill)
- **Testing**: `cargo test -p n00n-agent skill_policy`, `cargo test -p n00n-lua skill_tool_`, Lua spec
- **No new crates.io dependencies**

## Project Structure

```text
n00n-agent/src/
├── skill_policy.rs          # NEW: ActiveSkillPolicy + evaluate
├── agent/tool_dispatch.rs   # EXTEND: policy gate
├── agent/run.rs             # EXTEND: policy lifecycle from skill results
└── tools/mod.rs             # EXTEND: ToolContext.active_skill_policy

plugins/skill/
├── skill_policy.lua         # existing
├── skill_telemetry.lua      # NEW
├── skill_helpers.lua        # EXTEND: graph rank, steps, plans
├── init.lua                 # EXTEND: graph_rank, include_telemetry
└── tests/spec.lua           # EXTEND

specs/005-skill-system-v2/
├── phase6-spec.md
├── phase6-research.md
├── phase6-plan.md
├── phase6-data-model.md
├── phase6-tasks.md
├── phase6-quickstart.md
├── checklists/phase6-requirements.md
└── contracts/skill_policy_enforcement.md
```

## Approach

1. TDD `skill_policy` Rust module with unit tests.
2. Thread policy through `ToolContext` and agent lifecycle.
3. Add Lua telemetry, graph rank, steps parsing with tests.
4. Regenerate docs; run full skill test suite.
