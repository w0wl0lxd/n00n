# Tasks: Native Research Tool

**Input**: Design documents from `/specs/012-native-research-tool/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md

**Tests**: TDD is required for each new function. Write failing test first, then implement.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1, US2, US3, US4)
- Include exact file paths in descriptions

## Phase 0: Validation Spikes (Shared Infrastructure)

**Purpose**: Verify APIs and patterns before implementation

- [ ] T001 Verify subagent.launch() API supports only_tools, except_tools, system_append by inspecting plugins/lib/n00n/subagent.lua
- [ ] T002 Verify built-in tools (websearch, webfetch, codegraph, arbor) are available by checking plugins/ directory
- [ ] T003 Check MCP tool availability (arxiv, exa, context7, thoughtbox) in test environment
- [ ] T004 Inspect ToolView.restore() pattern in plugins/websearch/init.lua, plugins/webfetch/init.lua, plugins/codegraph/init.lua

**Checkpoint**: APIs and patterns verified

---

## Phase 1: Plugin Scaffolding (Foundational)

**Purpose**: Create plugin structure and register tool

### Tests for Scaffolding

- [ ] T005 [P] Add failing test for tool registration in plugins/research/tests/spec.lua

### Implementation for Scaffolding

- [ ] T006 Create plugins/research/ directory structure
- [ ] T007 Create plugins/research/init.lua with tool registration, schema, and empty handler
- [ ] T008 Add research to n00n-lua/src/loader.rs BUNDLED_PLUGINS list
- [ ] T009 Run cargo check -p n00n-lua to verify plugin loads without errors
- [ ] T010 Run test for tool registration in plugins/research/tests/spec.lua

**Checkpoint**: Plugin scaffolded and registered

---

## Phase 2: Input Validation and Source Mapping (US1, US2, US3, US4)

**Purpose**: Validate input and map sources to tools

### Tests for Validation

- [ ] T011 [P] Add failing test for empty query validation in plugins/research/tests/spec.lua
- [ ] T012 [P] Add failing test for invalid source enum validation in plugins/research/tests/spec.lua
- [ ] T013 [P] Add failing test for invalid depth enum validation in plugins/research/tests/spec.lua
- [ ] T014 [P] Add failing test for invalid output_format enum validation in plugins/research/tests/spec.lua
- [ ] T015 [P] Add failing test for source mapping (web → websearch/webfetch) in plugins/research/tests/spec.lua
- [ ] T016 [P] Add failing test for source mapping (arxiv → arxiv MCP) in plugins/research/tests/spec.lua

### Implementation for Validation

- [ ] T017 Implement input validation in plugins/research/init.lua handler (query non-empty, sources enum, depth enum, output_format enum)
- [ ] T018 Implement source to tool mapping function in plugins/research/init.lua
- [ ] T019 Run tests for validation in plugins/research/tests/spec.lua
- [ ] T020 Run tests for source mapping in plugins/research/tests/spec.lua

**Checkpoint**: Input validation and source mapping working

---

## Phase 3: System Prompt and Subagent Launch (US1, US2, US3, US4)

**Purpose**: Build system prompt and launch subagent

### Tests for Subagent Launch

- [ ] T021 [P] Add failing test for system prompt builder in plugins/research/tests/spec.lua
- [ ] T022 [P] Add failing test for subagent launch with correct parameters in plugins/research/tests/spec.lua
- [ ] T023 [P] Add failing test for recursive delegation tools exclusion in plugins/research/tests/spec.lua

### Implementation for Subagent Launch

- [ ] T024 Implement system prompt builder in plugins/research/init.lua with anti-hallucination rules and citation requirements
- [ ] T025 Implement subagent launch call in plugins/research/init.lua with subagent_type = "research", only_tools, except_tools, system_append
- [ ] T026 Run test for system prompt builder in plugins/research/tests/spec.lua
- [ ] T027 Run test for subagent launch with correct parameters in plugins/research/tests/spec.lua
- [ ] T028 Run test for recursive delegation tools exclusion in plugins/research/tests/spec.lua

**Checkpoint**: Subagent launches with correct configuration

---

## Phase 4: Output Handling and UI (US1, US2, US3, US4)

**Purpose**: Handle output and render UI

### Tests for Output Handling

- [ ] T029 [P] Add failing test for ToolView.restore() usage in plugins/research/tests/spec.lua
- [ ] T030 [P] Add failing test for cost/usage tracking in plugins/research/tests/spec.lua

### Implementation for Output Handling

- [ ] T031 Implement output handling with ToolView.restore() in plugins/research/init.lua
- [ ] T032 Implement cost/usage tracking in return value in plugins/research/init.lua
- [ ] T033 Run test for ToolView.restore() usage in plugins/research/tests/spec.lua
- [ ] T034 Run test for cost/usage tracking in plugins/research/tests/spec.lua

**Checkpoint**: Output handling and UI working

---

## Phase 5: Graceful Degradation (US4)

**Purpose**: Handle unavailable MCP tools and empty results

### Tests for Graceful Degradation

- [ ] T035 [P] Add failing test for MCP tool availability check in plugins/research/tests/spec.lua
- [ ] T036 [P] Add failing test for graceful degradation when MCP unavailable in plugins/research/tests/spec.lua
- [ ] T037 [P] Add failing test for empty results handling in plugins/research/tests/spec.lua

### Implementation for Graceful Degradation

- [ ] T038 Implement MCP tool availability check in plugins/research/init.lua
- [ ] T039 Implement graceful degradation when MCP tools unavailable in plugins/research/init.lua
- [ ] T040 Implement empty results handling in plugins/research/init.lua
- [ ] T041 Run test for MCP tool availability check in plugins/research/tests/spec.lua
- [ ] T042 Run test for graceful degradation when MCP unavailable in plugins/research/tests/spec.lua
- [ ] T043 Run test for empty results handling in plugins/research/tests/spec.lua

**Checkpoint**: Graceful degradation working

---

## Phase 6: Notebook Creation (US3)

**Purpose**: Create thoughtbox notebooks

### Tests for Notebook Creation

- [ ] T044 [P] Add failing test for notebook creation in plugins/research/tests/spec.lua
- [ ] T045 [P] Add failing test for notebook fallback to bullet_summary in plugins/research/tests/spec.lua

### Implementation for Notebook Creation

- [ ] T046 Implement notebook creation when output_format = "notebook" in plugins/research/init.lua
- [ ] T047 Implement fallback to bullet_summary when thoughtbox MCP unavailable in plugins/research/init.lua
- [ ] T048 Run test for notebook creation in plugins/research/tests/spec.lua
- [ ] T049 Run test for notebook fallback to bullet_summary in plugins/research/tests/spec.lua

**Checkpoint**: Notebook creation working

---

## Phase 7: Permission Scopes (US1, US2, US3, US4)

**Purpose**: Enforce permission scopes

### Tests for Permission Scopes

- [ ] T050 [P] Add failing test for research.subagent permission check in plugins/research/tests/spec.lua
- [ ] T051 [P] Add failing test for research.web permission check in plugins/research/tests/spec.lua
- [ ] T052 [P] Add failing test for research.thoughtbox permission check in plugins/research/tests/spec.lua

### Implementation for Permission Scopes

- [ ] T053 Add permission scopes to tool registration in plugins/research/init.lua (research.subagent, research.web, research.thoughtbox)
- [ ] T054 Implement permission checks in handler in plugins/research/init.lua
- [ ] T055 Run test for research.subagent permission check in plugins/research/tests/spec.lua
- [ ] T056 Run test for research.web permission check in plugins/research/tests/spec.lua
- [ ] T057 Run test for research.thoughtbox permission check in plugins/research/tests/spec.lua

**Checkpoint**: Permission scopes enforced

---

## Phase 8: Integration and Validation (Cross-Cutting)

**Purpose**: Integration, docs, and final verification

- [ ] T058 Update tool description in plugins/research/init.lua to position research as first-tier tool
- [ ] T059 Add research to NATIVE_EFFICIENT_TOOLS list in n00n-agent/src/prompt.rs (optional)
- [ ] T060 Run cargo test -p n00n-lua
- [ ] T061 Run cargo clippy --all --tests -- -D warnings
- [ ] T062 Run cargo deny check
- [ ] T063 Create integration test fixture with small repo for single-source tests
- [ ] T064 Create integration test fixture with small repo for multi-source tests
- [ ] T065 Measure token efficiency against manual multi-tool chains
- [ ] T066 Draft PR with performance comparison

**Checkpoint**: Integration complete, ready for PR

---

## Dependencies & Execution Order

### Phase Dependencies

- **Validation Spikes (Phase 0)**: No dependencies - can start immediately
- **Plugin Scaffolding (Phase 1)**: Depends on Phase 0 completion - BLOCKS all user stories
- **Validation and Source Mapping (Phase 2)**: Depends on Phase 1 completion
- **System Prompt and Subagent Launch (Phase 3)**: Depends on Phase 2 completion
- **Output Handling and UI (Phase 4)**: Depends on Phase 3 completion
- **Graceful Degradation (Phase 5)**: Depends on Phase 4 completion
- **Notebook Creation (Phase 6)**: Depends on Phase 4 completion (can run in parallel with Phase 5)
- **Permission Scopes (Phase 7)**: Depends on Phase 4 completion (can run in parallel with Phase 5 and Phase 6)
- **Integration and Validation (Phase 8)**: Depends on all previous phases

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Phase 4 (Output Handling) - No dependencies on other stories
- **User Story 2 (P1)**: Can start after Phase 4 (Output Handling) - No dependencies on other stories
- **User Story 3 (P2)**: Can start after Phase 6 (Notebook Creation) - No dependencies on other stories
- **User Story 4 (P2)**: Can start after Phase 5 (Graceful Degradation) - No dependencies on other stories

### Within Each Phase

- Tests MUST be written and FAIL before implementation
- Implementation follows tests
- Phase complete before moving to next phase

### Parallel Opportunities

- All Phase 0 tasks can run in parallel
- All Phase 2 tests can run in parallel
- All Phase 3 tests can run in parallel
- All Phase 4 tests can run in parallel
- All Phase 5 tests can run in parallel
- All Phase 6 tests can run in parallel
- All Phase 7 tests can run in parallel
- Phase 5, Phase 6, and Phase 7 can run in parallel after Phase 4

## Implementation Strategy

### MVP First (User Stories 1-2 Only)

1. Complete Phase 0: Validation Spikes
2. Complete Phase 1: Plugin Scaffolding
3. Complete Phase 2: Validation and Source Mapping
4. Complete Phase 3: System Prompt and Subagent Launch
5. Complete Phase 4: Output Handling and UI
6. **STOP and VALIDATE**: Test single-source and multi-source research
7. Deploy/demo if ready

### Incremental Delivery

1. Complete Phase 0 + Phase 1 + Phase 2 + Phase 3 + Phase 4 → Foundation ready
2. Add US1 + US2 → Test independently → Deploy/Demo (MVP!)
3. Add US4 → Test independently → Deploy/Demo
4. Add US3 → Test independently → Deploy/Demo
5. Complete Phase 7: Permission Scopes → Security ready
6. Complete Phase 8: Integration and Validation → Final release

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
