# Implementation Plan: Skill System V2

## Scope

This document is the **initial-phase** plan (discovery/filtering). Later phases
expanded the delivered scope; see:

- `phase5.md` — progressive loading, validation, ranking, plans
- `phase6-plan.md` / `phase6-spec.md` — agent policy enforcement, graph rank,
  telemetry, structured `steps`

Initial phase deliverables:

- Recursive nested discovery.
- Frontmatter normalization for `paths` and `disable-model-invocation`.
- Optional runtime filtering by `path`.
- Manual-only visibility gate with `include_manual`.
- Integration and unit tests.

## Files

- `plugins/skill/init.lua`
- `plugins/skill/skill_helpers.lua`
- `plugins/skill/tests/spec.lua`
- `n00n-lua/tests/plugin_host.rs`

## Approach

1. Add failing tests for new discovery/filter behavior.
2. Implement helper-layer frontmatter normalization and list/name filtering.
3. Implement recursive scanner and path-scope selection in skill runtime.
4. Validate with focused test targets first, then broader crate checks.
5. Capture residual follow-ups in tasks.
