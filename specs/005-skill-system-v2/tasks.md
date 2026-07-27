# Tasks: Skill System V2

## Phase 1: Tests First

- [x] T001 Add frontmatter normalization tests in `plugins/skill/tests/spec.lua`.
- [x] T002 Add integration tests for nested discovery in `n00n-lua/tests/plugin_host.rs`.
- [x] T003 Add integration tests for manual-only visibility in `n00n-lua/tests/plugin_host.rs`.
- [x] T004 Add integration tests for `paths` filtering in `n00n-lua/tests/plugin_host.rs`.

## Phase 2: Implementation

- [x] T005 Implement frontmatter normalization (`paths`, `disable-model-invocation`) in `plugins/skill/skill_helpers.lua`.
- [x] T006 Implement recursive nested skill scan in `plugins/skill/init.lua`.
- [x] T007 Implement `path`-based filtering and `include_manual` visibility gate in `plugins/skill/init.lua`.
- [x] T008 Extend tool schema with `path` and `include_manual` options in `plugins/skill/init.lua`.

## Phase 3: Validation

- [x] T009 Run `cargo test -p n00n-lua skill_tool_`.
- [x] T010 Run `cargo test -p n00n-lua spec`.
- [x] T011 Run `cargo check -p n00n-lua`.

## Follow-up (One-up roadmap)

- [x] T012 Add skill discovery cache (mtime + invalidation).
- [x] T013 Add conflict diagnostics for duplicate names with precedence report.
- [x] T014 Add telemetry hooks (skill hit-rate, token-cost impact).

## Phase 4: Progressive loading and safety envelope (P1)

- [x] T015 Defer skill body reads to load time (discovery stores metadata only).
- [x] T016 Add progressive load via `preview` and `section` parameters.
- [x] T017 Parse and surface `allowed-tools` / `disallowed-tools` frontmatter.
- [x] T018 Add `validate=true` list mode for skill lint diagnostics.
- [x] T019 Regenerate tool docs for new skill parameters.

## Phase 5: Policy enforcement and context routing

- [x] T020 Add `skill_policy` module with evaluate + call_tool wrapper.
- [x] T021 Return `state.active_skill` and policy instructions on skill load.
- [x] T022 Add context-aware skill ranking (`rank=true` + `path` + `tags`).
- [x] T023 Add plan extraction mode (`plan=true`).
- [x] T024 Add unit and integration tests for phase 5 behavior.
- [x] T025 Regenerate docs and validate.
