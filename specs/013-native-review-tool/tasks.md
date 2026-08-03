# Tasks: Native Review Tool

**Input**: Design documents from `/specs/013-native-review-tool/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md

**Tests**: TDD is required for each new Lua API. Write failing test first, then implement.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1, US2, US3, US4, US5, US6)
- Include exact file paths in descriptions

## Phase 1: Validation Spikes (Shared Infrastructure)

**Purpose**: Verify existing primitives work as expected before implementation

- [ ] T001 Verify skill system can load `security-review`, `code-review`, and `adversarial` skills from skill directories
- [ ] T002 Verify `n00n.subagent.launch()` works with structured output schema for findings
- [ ] T003 Verify `n00n.arbor.diff()` and `n00n.codegraph.affected()` return blast-radius data on fixture repo
- [ ] T004 Verify `git diff` via bash plugin returns parseable diff output on fixture repo

**Checkpoint**: Existing primitives verified

---

## Phase 2: Plugin Scaffolding (Foundational)

**Purpose**: Basic plugin structure and config registration

⚠️ CRITICAL: This phase establishes the plugin that all user stories depend on.

### Tests for Plugin Scaffolding

- [ ] T005 [P] [Scaffold] Add failing test for review tool registration in plugins/review/tests/spec.lua
- [ ] T006 [P] [Scaffold] Add failing test for review tool schema validation in plugins/review/tests/spec.lua

### Implementation for Plugin Scaffolding

- [ ] T007 [Scaffold] Create plugins/review/ directory with init.lua skeleton
- [ ] T008 [Scaffold] Register review tool with schema (target, focus, depth, output) in plugins/review/init.lua
- [ ] T009 [Scaffold] Add review to DEFAULT_BUILTINS in n00n-config/src/lib.rs
- [ ] T010 [Scaffold] Add review to BUNDLED_PLUGINS in n00n-lua/src/loader.rs
- [ ] T011 [Scaffold] Run cargo check --workspace to verify config changes
- [ ] T012 [Scaffold] Create plugins/review/tests/spec.lua with basic test skeleton
- [ ] T013 [Scaffold] Run tests for review tool registration in plugins/review/tests/spec.lua

**Checkpoint**: Plugin scaffolded and registered

---

## Phase 3: User Story 1 - Local Diff Review (Priority: P1) 🎯 MVP

**Goal**: Review local git diff with blast-radius and adversarial subagent

**Independent Test**: Run review with target="diff" on fixture repo and verify structured findings

### Tests for US1

- [ ] T014 [P] [US1] Add failing test for diff target resolution in plugins/review/tests/spec.lua
- [ ] T015 [P] [US1] Add failing test for diff parsing to extract changed files in plugins/review/tests/spec.lua
- [ ] T016 [P] [US1] Add failing test for blast-radius analysis in plugins/review/tests/spec.lua
- [ ] T017 [P] [US1] Add failing test for skill loading for security focus in plugins/review/tests/spec.lua
- [ ] T018 [P] [US1] Add failing test for subagent launch with findings output in plugins/review/tests/spec.lua
- [ ] T019 [P] [US1] Add failing test for structured findings output in plugins/review/tests/spec.lua

### Implementation for US1

- [ ] T020 [US1] Implement target resolution for diff target (local git diff) in plugins/review/init.lua
- [ ] T021 [US1] Implement diff parsing to extract changed files in plugins/review/init.lua
- [ ] T022 [US1] Implement blast-radius analysis using n00n.arbor.diff or n00n.codegraph.affected in plugins/review/init.lua
- [ ] T023 [US1] Implement skill loading for focus="security" (load security-review skill) in plugins/review/init.lua
- [ ] T024 [US1] Implement subagent launch with diff + blast-radius + skill prompt in plugins/review/init.lua
- [ ] T025 [US1] Implement structured findings output (severity, location, suggestion, focus) in plugins/review/init.lua
- [ ] T026 [US1] Handle edge case: no changes detected in plugins/review/init.lua
- [ ] T027 [US1] Handle edge case: no arbor/codegraph index (trigger indexing) in plugins/review/init.lua
- [ ] T028 [US1] Handle edge case: blast-radius analysis failure in plugins/review/init.lua
- [ ] T029 [US1] Run tests for local diff review in plugins/review/tests/spec.lua

**Checkpoint**: Local diff review works with security focus

---

## Phase 4: User Story 2 - Skill-Based Focus Selection (Priority: P1)

**Goal**: Load different skills based on focus parameter

**Independent Test**: Run review with different focus values and verify skill loading

### Tests for US2

- [ ] T030 [P] [US2] Add failing test for correctness focus skill loading in plugins/review/tests/spec.lua
- [ ] T031 [P] [US2] Add failing test for performance focus skill loading in plugins/review/tests/spec.lua
- [ ] T032 [P] [US2] Add failing test for style focus skill loading in plugins/review/tests/spec.lua
- [ ] T033 [P] [US2] Add failing test for all focus skill loading in plugins/review/tests/spec.lua
- [ ] T034 [P] [US2] Add failing test for skill fallback when skill not found in plugins/review/tests/spec.lua

### Implementation for US2

- [ ] T035 [US2] Implement skill loading for focus="correctness" (load code-review skill) in plugins/review/init.lua
- [ ] T036 [US2] Implement skill loading for focus="performance" (load code-review with performance emphasis) in plugins/review/init.lua
- [ ] T037 [US2] Implement skill loading for focus="style" (load code-review with style emphasis) in plugins/review/init.lua
- [ ] T038 [US2] Implement skill loading for focus="all" (load adversarial skill) in plugins/review/init.lua
- [ ] T039 [US2] Implement skill fallback logic (default adversarial prompt if skill not found) in plugins/review/init.lua
- [ ] T040 [US2] Implement focus-specific prompt emphasis in plugins/review/init.lua
- [ ] T041 [US2] Run tests for skill-based focus selection in plugins/review/tests/spec.lua

**Checkpoint**: All focus values load appropriate skills

---

## Phase 5: User Story 3 - Structured Findings Output (Priority: P1)

**Goal**: Return findings in structured format and markdown comment draft

**Independent Test**: Run review with different output formats and verify parsing

### Tests for US3

- [ ] T042 [P] [US3] Add failing test for findings JSON schema in plugins/review/tests/spec.lua
- [ ] T043 [P] [US3] Add failing test for findings severity sorting in plugins/review/tests/spec.lua
- [ ] T044 [P] [US3] Add failing test for comment draft markdown formatting in plugins/review/tests/spec.lua
- [ ] T045 [P] [US3] Add failing test for both output format in plugins/review/tests/spec.lua

### Implementation for US3

- [ ] T046 [US3] Implement findings JSON schema (severity, location, suggestion, focus) in plugins/review/init.lua
- [ ] T047 [US3] Implement findings severity sorting (critical > high > medium > low) in plugins/review/init.lua
- [ ] T048 [US3] Implement markdown comment draft formatting (grouped by severity) in plugins/review/init.lua
- [ ] T049 [US3] Implement output parameter handling (findings, comment_draft, both) in plugins/review/init.lua
- [ ] T050 [US3] Run tests for structured findings output in plugins/review/tests/spec.lua

**Checkpoint**: All output formats work correctly

---

## Phase 6: User Story 4 - GitHub PR Review Integration (Priority: P2)

**Goal**: Fetch PR diffs via github tool and review them

**Independent Test**: Run review with pr target and verify diff fetching

### Tests for US4

- [ ] T051 [P] [US4] Add failing test for pr target resolution in plugins/review/tests/spec.lua
- [ ] T052 [P] [US4] Add failing test for branch target resolution in plugins/review/tests/spec.lua
- [ ] T053 [P] [US4] Add failing test for commit target resolution in plugins/review/tests/spec.lua
- [ ] T054 [P] [US4] Add failing test for file target resolution in plugins/review/tests/spec.lua
- [ ] T055 [P] [US4] Add failing test for github tool availability check in plugins/review/tests/spec.lua

### Implementation for US4

- [ ] T056 [US4] Implement target resolution for pr:<number> (call n00n.github.pr_diff if available) in plugins/review/init.lua
- [ ] T057 [US4] Implement target resolution for branch:<name> (call git diff <branch>) in plugins/review/init.lua
- [ ] T058 [US4] Implement target resolution for commit:<sha> (call git diff <sha>) in plugins/review/init.lua
- [ ] T059 [US4] Implement target resolution for file:<path> (call git diff <path>) in plugins/review/init.lua
- [ ] T060 [US4] Add github tool availability check and fallback error in plugins/review/init.lua
- [ ] T061 [US4] Handle edge case: GitHub auth failure in plugins/review/init.lua
- [ ] T062 [US4] Handle edge case: missing remote in plugins/review/init.lua
- [ ] T063 [US4] Handle edge case: invalid PR number in plugins/review/init.lua
- [ ] T064 [US4] Run tests for GitHub PR review integration in plugins/review/tests/spec.lua

**Checkpoint**: All target types work correctly

---

## Phase 7: User Story 5 - Depth Control and Performance (Priority: P2)

**Goal**: Control review depth with quick vs thorough modes

**Independent Test**: Run review with different depth values and measure latency

### Tests for US5

- [ ] T065 [P] [US5] Add failing test for quick depth logic in plugins/review/tests/spec.lua
- [ ] T066 [P] [US5] Add failing test for thorough depth logic in plugins/review/tests/spec.lua
- [ ] T067 [P] [US5] Add failing test for token budget application in plugins/review/tests/spec.lua

### Implementation for US5

- [ ] T068 [US5] Implement depth="quick" logic (skip blast-radius if expensive, limit diff size) in plugins/review/init.lua
- [ ] T069 [US5] Implement depth="thorough" logic (full blast-radius, comprehensive subagent prompt) in plugins/review/init.lua
- [ ] T070 [US5] Implement token budget application for large diffs (truncate with message) in plugins/review/init.lua
- [ ] T071 [US5] Add progress indicators for long-running operations (using ExploreResult) in plugins/review/init.lua
- [ ] T072 [US5] Run performance tests (measure latency for quick vs thorough) in plugins/review/tests/spec.lua
- [ ] T073 [US5] Verify quick review completes within 30 seconds for typical diff

**Checkpoint**: Depth control works and performance targets met

---

## Phase 8: User Story 6 - Permission Scopes and Safety (Priority: P3)

**Goal**: Respect permission scopes for review operations

**Independent Test**: Configure permission scopes and verify blocking

### Tests for US6

- [ ] T074 [P] [US6] Add failing test for review.read permission check in plugins/review/tests/spec.lua
- [ ] T075 [P] [US6] Add failing test for review.write permission check in plugins/review/tests/spec.lua
- [ ] T076 [P] [US6] Add failing test for review.subagent permission check in plugins/review/tests/spec.lua

### Implementation for US6

- [ ] T077 [US6] Implement permission check for review.read in plugins/review/init.lua
- [ ] T078 [US6] Implement permission check for review.write in plugins/review/init.lua
- [ ] T079 [US6] Implement permission check for review.subagent in plugins/review/init.lua
- [ ] T080 [US6] Implement fallback when review.subagent denied (non-subagent review) in plugins/review/init.lua
- [ ] T081 [US6] Implement fallback when review.write denied (return draft without posting) in plugins/review/init.lua
- [ ] T082 [US6] Implement error when review.read denied (block all operations) in plugins/review/init.lua
- [ ] T083 [US6] Run tests for permission scopes in plugins/review/tests/spec.lua

**Checkpoint**: Permission scopes respected with clear errors

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Docs, config, and final verification

- [ ] T084 [P] Update tool description to position review as first-tier code-quality tool in plugins/review/init.lua
- [ ] T085 [P] Add prompt hints in n00n-agent/src/prompts/system.md to recommend review for code changes
- [ ] T086 [P] Add prompt hints in n00n-agent/src/prompts/general.md to recommend review for code changes
- [ ] T087 [P] Add prompt hints in n00n-agent/src/prompts/research.md to recommend review for code changes
- [ ] T088 Create integration test fixture under tests/fixtures/review-repo/
- [ ] T089 Add user-facing docs in site/docs/ for review tool usage
- [ ] T090 Measure tool definition token count and verify ≤2000 tokens
- [ ] T091 Run cargo nextest run --workspace
- [ ] T092 Run cargo clippy --all --tests -- -D warnings
- [ ] T093 Run cargo deny check

**Checkpoint**: Documentation updated, all checks pass

---

## Dependencies & Execution Order

### Phase Dependencies

- **Validation Spikes (Phase 1)**: No dependencies - can start immediately
- **Plugin Scaffolding (Phase 2)**: Depends on Phase 1 completion - BLOCKS all user stories
- **User Stories (Phase 3-8)**: All depend on Phase 2 completion
  - User stories can proceed in parallel (if staffed)
  - Or sequentially in priority order (P1 → P2 → P3)
- **Polish (Phase 9)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Phase 2 (Plugin Scaffolding) - No dependencies on other stories
- **User Story 2 (P1)**: Can start after Phase 2 (Plugin Scaffolding) - May integrate with US1 but should be independently testable
- **User Story 3 (P1)**: Can start after Phase 2 (Plugin Scaffolding) - Depends on US1 for findings structure
- **User Story 4 (P2)**: Can start after Phase 2 (Plugin Scaffolding) - No dependencies on other stories
- **User Story 5 (P2)**: Can start after Phase 2 (Plugin Scaffolding) - No dependencies on other stories
- **User Story 6 (P3)**: Can start after Phase 2 (Plugin Scaffolding) - No dependencies on other stories

### Within Each User Story

- Tests MUST be written and FAIL before implementation
- Implementation follows tests
- Story complete before moving to next priority

### Parallel Opportunities

- All Validation Spikes tasks can run in parallel
- All Plugin Scaffolding tests can run in parallel
- All User Story tests can run in parallel within each story
- All Polish tasks can run in parallel

## Implementation Strategy

### MVP First (User Stories 1-3 Only)

1. Complete Phase 1: Validation Spikes
2. Complete Phase 2: Plugin Scaffolding (CRITICAL - blocks all stories)
3. Complete Phase 3: User Story 1 (Local Diff Review)
4. Complete Phase 4: User Story 2 (Skill-Based Focus Selection)
5. Complete Phase 5: User Story 3 (Structured Findings Output)
6. **STOP and VALIDATE**: Test MVP independently
7. Deploy/demo if ready

### Incremental Delivery

1. Complete Phase 1 + Phase 2 → Foundation ready
2. Add US1 + US2 + US3 → Test independently → Deploy/Demo (MVP!)
3. Add US4 → Test independently → Deploy/Demo
4. Add US5 → Test independently → Deploy/Demo
5. Add US6 → Test independently → Deploy/Demo
6. Complete Phase 9: Polish → Documentation ready
7. Final release

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- No new Rust crate unless absolutely necessary
- Prefer Lua-only implementation using existing n00n primitives