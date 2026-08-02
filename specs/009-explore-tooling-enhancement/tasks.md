# Tasks: Explore Tooling Enhancement

**Input**: Design documents from `/specs/009-explore-tooling-enhancement/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: TDD is required for each new Rust/Lua API. Write failing test first, then implement.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1, US2, US3, US4, US5, US6)
- Include exact file paths in descriptions

## Phase 1: Baseline and Research (Shared Infrastructure)

**Purpose**: Baseline verification and research

- [X] T001 Verify CodeGraph 1.5.0 compatibility by installing CLI and testing new commands on a fixture repo
- [X] T002 Capture tool call latency and tool definition token size baselines for `arbor`, `codegraph`, `explore`, and `semblem` on the n00n repo; store in `tests/fixtures/tooling-baseline.json` (linked to SC-009).
- [ ] T003 Run `cargo nextest run --workspace` to establish green baseline (deferred due to system load - retry needed)
- [ ] T004 Run `cargo clippy --all --tests -- -D warnings` to establish green baseline (deferred due to system load - retry needed)
- [X] T005 Run `cargo deny check` to establish green baseline (pre-existing license error: webpki-roots CDLA-Permissive-2.0 not allowed)
- [X] T006 Run `just explore-health` to verify current tool health (arbor not indexed, codegraph 1.4.1 with index)
- [X] T007 Document research findings in research.md (already documented in spec kit)

**Checkpoint**: Baseline established, research documented

---

## Phase 2: Router + Prompts (Foundational)

**Purpose**: US1 and US2 - smarter router and first-tier prompts

⚠️ CRITICAL: This phase establishes the explore router and prompt hierarchy that all other tools depend on.

### Tests for US1 (Router)

- [X] T008 [P] [US1] Add failing test for new intent `search` in plugins/explore/router.lua
- [X] T009 [P] [US1] Add failing test for new intent `skeleton` in plugins/explore/router.lua
- [X] T010 [P] [US1] Add failing test for new intent `symbol` in plugins/explore/router.lua
- [X] T011 [P] [US1] Add failing test for new intent `impact` in plugins/explore/router.lua
- [X] T012 [P] [US1] Add failing test for new intent `trace` in plugins/explore/router.lua

### Implementation for US1 (Router)

- [X] T013 [US1] Extend plugins/explore/router.lua with new intents: search, skeleton, symbol, impact, trace
- [X] T014 [US1] Update plugins/explore/init.lua schema to include new intents in enum
- [X] T015 [US1] Add routing logic for search intent to semblem in plugins/explore/router.lua
- [X] T016 [US1] Add routing logic for skeleton intent to index in plugins/explore/router.lua
- [X] T017 [US1] Add routing logic for symbol intent to arbor/codegraph in plugins/explore/router.lua
- [X] T018 [US1] Add routing logic for impact intent to arbor/codegraph in plugins/explore/router.lua
- [X] T019 [US1] Add routing logic for trace intent to arbor in plugins/explore/router.lua
- [X] T020 [US1] Create `tests/fixtures/explore-queries.json` with ≥20 labeled queries and a test that validates router classification ≥90% (linked to SC-001).
- [X] T020a [US1] Run tests for new router intents in plugins/explore/router.lua

### Implementation for US2 (Prompts)

- [X] T021 [US2] Update n00n-agent/src/prompt.rs NATIVE_EFFICIENT_TOOLS to remove "optional" qualifiers from arbor/codegraph
- [X] T022 [US2] Update n00n-agent/src/prompts/system.md to position explore/index/arbor/codegraph/semblem before grep/bash
- [X] T023 [US2] Update n00n-agent/src/prompts/general.md to position explore/index/arbor/codegraph/semblem before grep/bash
- [X] T024 [US2] Update n00n-agent/src/prompts/research.md to position explore/index/arbor/codegraph/semblem before grep/bash
- [X] T025 [US2] Update plugins/arbor/init.lua tool description to remove external CLI installation notes
- [X] T026 [US2] Update plugins/codegraph/init.lua tool description to remove external CLI installation notes
- [X] T027 [US2] Update plugins/semblem/init.lua tool description to remove external CLI installation notes (no changes needed)

**Checkpoint**: Router supports new intents, prompts position tools as first-tier

---

## Phase 3: User Story 3 - CodeGraph 1.5.0 Expansion (Priority: P2)

**Goal**: Expose additional CodeGraph commands (callers, callees, impact, affected, node, query, sync)

**Independent Test**: Invoke new CodeGraph commands through n00n-codegraph API and verify results against CLI output

### Tests for US3

- [ ] T028 [P] [US3] Add failing test for callers command in n00n-codegraph/src/lib.rs
- [ ] T029 [P] [US3] Add failing test for callees command in n00n-codegraph/src/lib.rs
- [ ] T030 [P] [US3] Add failing test for impact command in n00n-codegraph/src/lib.rs
- [ ] T031 [P] [US3] Add failing test for affected command in n00n-codegraph/src/lib.rs
- [ ] T031a [P] [US3] Add failing test for files command in n00n-codegraph/src/lib.rs
- [ ] T032 [P] [US3] Add failing test for node command in n00n-codegraph/src/lib.rs
- [ ] T033 [P] [US3] Add failing test for query command in n00n-codegraph/src/lib.rs
- [ ] T034 [P] [US3] Add failing test for sync command in n00n-codegraph/src/lib.rs

### Implementation for US3

- [ ] T035 [US3] Update n00n-codegraph/Cargo.toml to target CodeGraph 1.5.0 (add comment, no dependency change)
- [ ] T036 [US3] Extend n00n-codegraph/src/lib.rs with callers command function
- [ ] T037 [US3] Extend n00n-codegraph/src/lib.rs with callees command function
- [ ] T038 [US3] Extend n00n-codegraph/src/lib.rs with impact command function
- [ ] T039 [US3] Extend n00n-codegraph/src/lib.rs with affected command function
- [ ] T039a [US3] Extend n00n-codegraph/src/lib.rs with files command function
- [ ] T040 [US3] Extend n00n-codegraph/src/lib.rs with node command function
- [ ] T041 [US3] Extend n00n-codegraph/src/lib.rs with query command function
- [ ] T042 [US3] Extend n00n-codegraph/src/lib.rs with sync command function
- [ ] T043 [US3] Extend n00n-codegraph/src/db.rs with native SQLite queries for callers
- [ ] T044 [US3] Extend n00n-codegraph/src/db.rs with native SQLite queries for callees
- [ ] T045 [US3] Extend n00n-codegraph/src/db.rs with native SQLite queries for impact
- [ ] T046 [US3] Extend n00n-codegraph/src/db.rs with native SQLite queries for affected
- [ ] T046a [US3] Extend n00n-codegraph/src/db.rs with native SQLite queries for files
- [ ] T047 [US3] Extend n00n-codegraph/src/db.rs with native SQLite queries for node
- [ ] T048 [US3] Extend n00n-codegraph/src/db.rs with native SQLite queries for query
- [ ] T049 [US3] Add Lua API functions in n00n-lua/src/api/codegraph.rs for new commands
- [ ] T050 [US3] Update plugins/codegraph/init.lua to expose new commands in tool schema
- [ ] T051 [US3] Run tests for new CodeGraph commands in n00n-codegraph/src/lib.rs

**Checkpoint**: CodeGraph 1.5.0 commands exposed and tested

---

## Phase 4: User Story 4 - Arbor Expansion (Priority: P2)

**Goal**: Expose additional Arbor commands (entry-points, file-graph, inspect, path, refactor, check, summary)

**Independent Test**: Invoke new Arbor commands through n00n-arbor API and verify results against CLI output

### Tests for US4

- [ ] T052 [P] [US4] Add failing test for entry-points command in n00n-arbor/src/lib.rs
- [ ] T053 [P] [US4] Add failing test for file-graph command in n00n-arbor/src/lib.rs
- [ ] T054 [P] [US4] Add failing test for inspect command in n00n-arbor/src/lib.rs
- [ ] T055 [P] [US4] Add failing test for path command in n00n-arbor/src/lib.rs
- [ ] T056 [P] [US4] Add failing test for refactor command in n00n-arbor/src/lib.rs
- [ ] T057 [P] [US4] Add failing test for check command in n00n-arbor/src/lib.rs
- [ ] T058 [P] [US4] Add failing test for summary command in n00n-arbor/src/lib.rs
- [ ] T058a [P] [US4] Add failing test for trace command in n00n-arbor/src/lib.rs

### Implementation for US4

- [ ] T059 [US4] Extend n00n-arbor/src/lib.rs with entry-points command function
- [ ] T060 [US4] Extend n00n-arbor/src/lib.rs with file-graph command function
- [ ] T061 [US4] Extend n00n-arbor/src/lib.rs with inspect command function
- [ ] T062 [US4] Extend n00n-arbor/src/lib.rs with path command function
- [ ] T063 [US4] Extend n00n-arbor/src/lib.rs with refactor command function
- [ ] T064 [US4] Extend n00n-arbor/src/lib.rs with check command function
- [ ] T065 [US4] Extend n00n-arbor/src/lib.rs with summary command function
- [ ] T065a [US4] Extend n00n-arbor/src/lib.rs with trace command function
- [ ] T066 [US4] Use native `ArborGraph` from `.arbor/graph.json` to answer `arbor callers`, `callees`, and `trace` when the `arbor` CLI is not on `PATH`; ensure output matches CLI format.
- [ ] T067 [US4] Use `ArborGraph` centrality and module data to implement `arbor map` with a token budget and `arbor entry-points` without the CLI.
- [ ] T068 [US4] Extend `ArborGraph` parsing to support Rust, Python, and Lua files for native fallback; add fixture tests.
- [ ] T069 [US4] Add Lua API functions in n00n-lua/src/api/arbor.rs for new commands
- [ ] T070 [US4] Update plugins/arbor/init.lua to expose new commands in tool schema
- [ ] T071 [US4] Run tests for new Arbor commands in n00n-arbor/src/lib.rs

**Checkpoint**: Arbor commands exposed and tested

---

## Phase 5: User Story 5 - Semblem Hybrid (Priority: P3)

**Goal**: Wrap upstream Semble CLI for remote URLs and content filters, keep native BM25 fallback

**Independent Test**: Invoke Semblem with upstream CLI commands and verify results, test BM25 fallback

### Tests for US5

- [ ] T072 [P] [US5] Add failing test for upstream CLI wrapper in n00n-semble/src/lib.rs
- [ ] T073 [P] [US5] Add failing test for remote URL support in n00n-semble/src/lib.rs
- [ ] T074 [P] [US5] Add failing test for content filter support in n00n-semble/src/lib.rs
- [ ] T075 [P] [US5] Add failing test for BM25 fallback when CLI unavailable in n00n-semble/src/lib.rs

### Implementation for US5

- [ ] T076 [US5] Add upstream Semble CLI wrapper function in n00n-semble/src/lib.rs for Semblem
- [ ] T077 [US5] Add remote git URL support to Semble CLI wrapper in n00n-semble/src/lib.rs for Semblem
- [ ] T078 [US5] Add --content docs/config/all flag support to Semble CLI wrapper in n00n-semble/src/lib.rs for Semblem
- [ ] T079 [US5] Add find-related command support to Semble CLI wrapper in n00n-semble/src/lib.rs for Semblem
- [ ] T080 [US5] Add savings command support to Semble CLI wrapper in n00n-semble/src/lib.rs for Semblem
- [ ] T081 [US5] Add CLI availability check function in n00n-semble/src/lib.rs for Semblem
- [ ] T082 [US5] Update plugins/semblem/init.lua to call upstream Semble CLI when available
- [ ] T083 [US5] Update plugins/semblem/init.lua to fall back to native BM25 when Semble CLI unavailable
- [ ] T084 [US5] Keep existing embedder nag logic for hybrid/semantic modes in n00n-semble/src/lib.rs
- [ ] T085 [US5] Run tests for upstream CLI wrapper and BM25 fallback in n00n-semble/src/lib.rs

**Checkpoint**: Semblem hybrid with upstream CLI and BM25 fallback

---

## Phase 6: User Story 6 - RTK Hardening (Priority: P3)

**Goal**: Cache RTK availability per session, broaden coverage, update prompt hints

**Independent Test**: Verify RTK rewriting occurs, availability is cached, jq/yq pass through

### Tests for US6

- [ ] T086 [P] [US6] Add failing test for RTK availability caching in plugins/bash/init.lua
- [ ] T087 [P] [US6] Add failing test for broader rtk rewrite coverage in plugins/bash/init.lua

### Implementation for US6

- [ ] T088 [US6] Add session-local variable for RTK availability cache in plugins/bash/init.lua
- [ ] T089 [US6] Update rtk_rewrite function to use cached availability in plugins/bash/init.lua
- [ ] T090 [US6] Add `podman`, `docker`, `npm`, `pip`, `python`, and `gh` to the rtk rewrite command table in `plugins/bash/init.lua`; ensure `jq`/`yq` passthrough is unchanged (FR-017).
- [ ] T091 [US6] Verify jq and yq pass through unchanged in plugins/bash/init.lua
- [ ] T092 [US6] Update prompt hints in plugins/bash/init.lua to explicitly recommend rtk-wrapped bash
- [ ] T093 [US6] Run tests for RTK availability caching in plugins/bash/init.lua

**Checkpoint**: RTK hardened with session caching and broader coverage

---

## Phase 7: Docs + Config (Polish & Cross-Cutting Concerns)

**Purpose**: Docs, config, and final verification

- [ ] T094 [P] Update AGENTS.md token-efficient section to reflect new tool hierarchy
- [ ] T095 [P] Update n00n-config/src/lib.rs tool output line budgets if needed
- [ ] T096 [P] Update quickstart.md with validation commands for new features
- [ ] T097 [P] Regenerate site docs with just gen-docs

**Checkpoint**: Documentation updated

---

## Phase 8: Verification

**Purpose**: Final verification and performance checks

- [ ] T098 Run full test suite: cargo nextest run --workspace
- [ ] T099 Run cargo clippy --all --tests -- -D warnings
- [ ] T100 Run cargo deny check
- [ ] T101 Run just explore-health
- [ ] T102 Manual smoke test: explore router with various intents
- [ ] T103 Manual smoke test: new CodeGraph commands (SC-003 smoke test)
- [ ] T104 Manual smoke test: new Arbor commands (SC-004 smoke test)
- [ ] T105 Manual smoke test: Semblem upstream CLI and BM25 fallback (SC-005–SC-006 smoke tests)
- [ ] T106 Manual smoke test: RTK rewriting and caching (SC-007 smoke test)
- [ ] T107 Measure tool definition token sizes against baseline
- [ ] T108 Run final performance regression: compare `arbor`, `codegraph`, `explore`, `semblem`, and `rtk` latency/token size against `tests/fixtures/tooling-baseline.json`; ensure no regression beyond 10% (SC-009).
- [ ] T109 [P] Measure `explore`/`codegraph`/`arbor`/`semblem`/`rtk` tool definition token counts and verify they do not exceed the current baseline (SC-008).
- [ ] T110 [P] Compare final tool call latency against `tests/fixtures/tooling-baseline.json`; ensure ≤10% regression (SC-009).
- [ ] T111 Verify the agent's default prompt lists `explore`, `index`, `arbor`, `codegraph`, and `semblem` before `grep`/`bash` (SC-002).
- [ ] T112 Add a test that `bash` plugin caches `rtk` availability per session and only invokes `rtk` when installed (SC-007).

**Checkpoint**: All tests pass, documentation updated, performance verified

---

## Dependencies & Execution Order

### Phase Dependencies

- **Baseline and Research (Phase 1)**: No dependencies - can start immediately
- **Router + Prompts (Phase 2)**: Depends on Phase 1 completion - BLOCKS all user stories
- **User Stories (Phase 3-6)**: All depend on Phase 2 completion
  - User stories can proceed in parallel (if staffed)
  - Or sequentially in priority order (P1 → P2 → P3)
- **Docs + Config (Phase 7)**: Depends on all desired user stories being complete
- **Verification (Phase 8)**: Depends on Phase 7 completion

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Phase 2 (Router + Prompts) - No dependencies on other stories
- **User Story 2 (P1)**: Can start after Phase 2 (Router + Prompts) - No dependencies on other stories
- **User Story 3 (P2)**: Can start after Phase 2 (Router + Prompts) - No dependencies on other stories
- **User Story 4 (P2)**: Can start after Phase 2 (Router + Prompts) - No dependencies on other stories
- **User Story 5 (P3)**: Can start after Phase 2 (Router + Prompts) - No dependencies on other stories
- **User Story 6 (P3)**: Can start after Phase 2 (Router + Prompts) - No dependencies on other stories

### Within Each User Story

- Tests MUST be written and FAIL before implementation
- Implementation follows tests
- Story complete before moving to next priority

### Parallel Opportunities

- All Setup tasks can run in parallel
- All Foundational tests can run in parallel
- All User Story tests can run in parallel within each story
- All Polish tasks can run in parallel

## Implementation Strategy

### MVP First (User Stories 1-2 Only)

1. Complete Phase 1: Baseline and Research
2. Complete Phase 2: Router + Prompts (US1 + US2)
3. **STOP and VALIDATE**: Test router and prompts independently
4. Deploy/demo if ready

### Incremental Delivery

1. Complete Phase 1 + Phase 2 → Foundation ready
2. Add US1 + US2 → Test independently → Deploy/Demo (MVP!)
3. Add US3 → Test independently → Deploy/Demo
4. Add US4 → Test independently → Deploy/Demo
5. Add US5 → Test independently → Deploy/Demo
6. Add US6 → Test independently → Deploy/Demo
7. Complete Phase 7: Docs + Config → Documentation ready
8. Complete Phase 8: Verification → Final release

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
