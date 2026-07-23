# Tasks: ALMAS expansion - cross-session visibility, blackboard coordination, and agent control plane

**Input**: Design documents from `/specs/002-agent-coordination-visibility/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: TDD approach - write failing tests first, then implement. Tests for each user story.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story?] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- **Lua plugins**: `plugins/<plugin>/init.lua`
- **Shared lib modules**: `plugins/lib/n00n/<module>.lua`
- **Rust API**: `n00n-lua/src/api/<module>.rs`
- **Tests**: `n00n-lua/tests/<feature>.rs` or `plugins/<plugin>/tests/spec.lua`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and basic structure

- [ ] T001 Create plugins/blackboard directory for new blackboard plugin
- [ ] T002 Create plugins/team/waves.lua file for wave dispatch logic
- [ ] T003 Create plugins/team/validation.lua file for dual validation gates
- [ ] T004 Create plugins/lib/n00n/live_context.lua file for visibility API
- [ ] T005 Create plugins/lib/n00n/checkpoint.lua file for lifecycle checkpoints
- [ ] T006 [P] Verify workspace compiles with cargo check --all
- [ ] T007 [P] Verify workspace lints pass with cargo clippy --all --tests -- -D warnings

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [ ] T008 Implement blackboard storage layout in plugins/blackboard/init.lua (create blackboard directory, posts.jsonl, lockfile)
- [ ] T009 Implement JSONL append-only write with file lock in plugins/blackboard/init.lua
- [ ] T010 Implement JSONL read and query functions in plugins/blackboard/init.lua
- [ ] T011 Implement post validation (id, agent_id, timestamp, type, content) in plugins/blackboard/init.lua
- [ ] T012 Implement live_context snapshot composition in plugins/lib/n00n/live_context.lua (combine n00n.session.live + telemetry + blackboard)
- [ ] T013 Implement checkpoint save/load primitives in plugins/lib/n00n/checkpoint.lua (JSON snapshots in run directory)
- [ ] T014 [P] Write Rust integration test for blackboard storage in n00n-lua/tests/blackboard.rs
- [ ] T015 [P] Write Lua plugin test for live_context in plugins/lib/n00n/tests/spec.lua

**Checkpoint**: Foundation ready - user story implementation can now begin in parallel

---

## Phase 3: User Story 1 - Shared blackboard for cross-session coordination (Priority: P1) 🎯 MVP

**Goal**: Agents post observations, claims, and status updates to a shared blackboard for cross-session coordination

**Independent Test**: Start two concurrent team runs, have both agents post to the blackboard, and verify each session can read the other's posts and claims without race conditions.

### Tests for User Story 1

- [ ] T016 [P] [US1] Write failing integration test for blackboard write action in n00n-lua/tests/blackboard.rs
- [ ] T017 [P] [US1] Write failing integration test for blackboard query action in n00n-lua/tests/blackboard.rs
- [ ] T018 [P] [US1] Write failing integration test for concurrent blackboard posts in n00n-lua/tests/blackboard.rs

### Implementation for User Story 1

- [ ] T019 [US1] Implement blackboard write action in plugins/blackboard/init.lua (FR-001)
- [ ] T020 [US1] Implement blackboard read action in plugins/blackboard/init.lua (FR-001)
- [ ] T021 [US1] Implement blackboard query action with filters (type, task_id, tags, agent_id) in plugins/blackboard/init.lua (FR-001)
- [ ] T022 [US1] Register blackboard tool with n00n.api.register_tool in plugins/blackboard/init.lua
- [ ] T023 [US1] Integrate blackboard tool into team plugin in plugins/team/init.lua (post step status)
- [ ] T024 [US1] Add error handling for blackboard unavailable in plugins/blackboard/init.lua
- [ ] T025 [US1] Add logging for blackboard operations in plugins/blackboard/init.lua

**Checkpoint**: At this point, User Story 1 should be fully functional and testable independently

---

## Phase 4: User Story 2 - Atomic task claiming to prevent duplicate work (Priority: P1)

**Goal**: Agents claim tasks atomically from a shared queue so multiple agents never work on the same task simultaneously

**Independent Test**: Spin up three agents, have them all attempt to claim from a task queue with five items, and verify each task is claimed exactly once with no double-claims.

### Tests for User Story 2

- [ ] T026 [P] [US2] Write failing integration test for claim_task atomicity in n00n-lua/tests/blackboard.rs
- [ ] T027 [P] [US2] Write failing integration test for claim timeout and release in n00n-lua/tests/blackboard.rs
- [ ] T028 [P] [US2] Write failing integration test for concurrent claim attempts in n00n-lua/tests/blackboard.rs

### Implementation for User Story 2

- [ ] T029 [US2] Implement claim_task action with file lock atomicity in plugins/blackboard/init.lua (FR-002)
- [ ] T030 [US2] Implement release_task action in plugins/blackboard/init.lua (FR-002)
- [ ] T031 [US2] Implement update_task action (mark done/failed) in plugins/blackboard/init.lua (FR-002)
- [ ] T032 [US2] Implement claim timeout enforcement in plugins/blackboard/init.lua (FR-003)
- [ ] T033 [US2] Implement claim renewal logic in plugins/blackboard/init.lua (FR-003)
- [ ] T034 [US2] Add background task for expired claim cleanup in plugins/blackboard/init.lua
- [ ] T035 [US2] Integrate task claiming into team plugin in plugins/team/init.lua

**Checkpoint**: At this point, User Stories 1 AND 2 should both work independently

---

## Phase 5: User Story 3 - Implicit live-session visibility without explicit joins (Priority: P1)

**Goal**: See all active agent sessions and their current status without manually joining or subscribing to each one

**Independent Test**: Start three concurrent team runs and one workflow run, then query the visibility API and verify all sessions appear with their current status, last activity timestamp, and active task.

### Tests for User Story 3

- [ ] T036 [P] [US3] Write failing integration test for live_context list sessions in n00n-lua/tests/session.rs
- [ ] T037 [P] [US3] Write failing integration test for live_context filtering in n00n-lua/tests/session.rs
- [ ] T038 [P] [US3] Write failing integration test for live_context cache performance in n00n-lua/tests/session.rs

### Implementation for User Story 3

- [ ] T039 [US3] Implement live_context list_sessions function in plugins/lib/n00n/live_context.lua (FR-004)
- [ ] T040 [US3] Implement live_context filter by agent_type, status, time range in plugins/lib/n00n/live_context.lua (FR-005)
- [ ] T041 [US3] Implement live_context cached snapshot with TTL in plugins/lib/n00n/live_context.lua (FR-005)
- [ ] T042 [US3] Extend n00n.session.live to include active_task_id in n00n-lua/src/api/session.rs if needed
- [ ] T043 [US3] Register live_context module as n00n.api module in n00n-lua/src/api/mod.rs
- [ ] T044 [US3] Add visibility tool or command for querying live sessions
- [ ] T045 [US3] Integrate live_context into agent_control for session listing

**Checkpoint**: All user stories should now be independently functional

---

## Phase 6: User Story 4 - Agent control plane with policy enforcement (Priority: P2)

**Goal**: Centralized control plane that enforces policies on agent behavior (e.g., background agents cannot call write tools, paused agents cannot run any tools)

**Independent Test**: Configure a policy that denies write tools to background agents, start a background agent, attempt a write operation, and verify it is blocked with a policy violation error.

### Tests for User Story 4

- [ ] T046 [P] [US4] Write failing integration test for policy set in n00n-lua/tests/agent_control.rs
- [ ] T047 [P] [US4] Write failing integration test for policy evaluation in n00n-lua/tests/agent_control.rs
- [ ] T048 [P] [US4] Write failing integration test for policy violation enforcement in n00n-lua/tests/agent_control.rs

### Implementation for User Story 4

- [ ] T049 [US4] Extend agent_control schema with policy action in plugins/agent_control/init.lua (FR-006)
- [ ] T050 [US4] Implement policy set action in plugins/agent_control/init.lua (FR-007)
- [ ] T051 [US4] Implement policy get action in plugins/agent_control/init.lua
- [ ] T052 [US4] Implement policy delete action in plugins/agent_control/init.lua
- [ ] T053 [US4] Implement policy list action in plugins/agent_control/init.lua
- [ ] T054 [US4] Implement policy storage in state_dir/projects/{pid}/policies/policy.json in plugins/agent_control/init.lua
- [ ] T055 [US4] Implement policy evaluation logic (scope matching, priority sorting, conflict resolution) in plugins/agent_control/init.lua (FR-007)
- [ ] T056 [US4] Implement check_policy helper function in plugins/agent_control/init.lua
- [ ] T057 [US4] Integrate policy check into tool dispatch in plugins/agent_control/init.lua (FR-006)
- [ ] T058 [US4] Add policy error messages (paused, restricted, not allowed) in plugins/agent_control/init.lua

**Checkpoint**: User Story 4 should be fully functional and testable independently

---

## Phase 7: User Story 5 - SPOQ wave dispatch and dual validation gates (Priority: P2)

**Goal**: Supervisor dispatches agents in waves (planning, implementation, validation) with dual validation gates between waves

**Independent Test**: Run a team task with wave dispatch enabled, inject a defect in the implementation wave, and verify the validation wave catches it before the run completes.

### Tests for User Story 5

- [ ] T059 [P] [US5] Write failing Lua test for wave topology computation in plugins/team/tests/spec.lua
- [ ] T060 [P] [US5] Write failing Lua test for validation gate execution in plugins/team/tests/spec.lua
- [ ] T061 [P] [US5] Write failing integration test for wave dispatch with defect detection in n00n-lua/tests/team.rs

### Implementation for User Story 5

- [ ] T062 [US5] Implement wave topology computation from step dependencies in plugins/team/waves.lua (FR-008)
- [ ] T063 [US5] Implement wave execution engine in plugins/team/waves.lua
- [ ] T064 [US5] Implement validation gate runner in plugins/team/validation.lua (FR-009)
- [ ] T065 [US5] Implement dual validation logic (self + cross-wave) in plugins/team/validation.lua (FR-009)
- [ ] T066 [US5] Add wave configuration option to team tool schema in plugins/team/init.lua
- [ ] T067 [US5] Integrate wave dispatch into team autonomous mode in plugins/team/init.lua (FR-008)
- [ ] T068 [US5] Add wave failure handling and retry logic in plugins/team/waves.lua
- [ ] T069 [US5] Integrate validation gates between waves in plugins/team/init.lua (FR-009)

**Checkpoint**: User Story 5 should be fully functional and testable independently

---

## Phase 8: User Story 6 - Human-as-an-Agent escalation for unrecoverable failures (Priority: P2)

**Goal**: Agents escalate to human when encountering failures they cannot resolve, pause with status awaiting_human, and resume after human input

**Independent Test**: Configure a task that requires human approval for a sensitive operation, run it, and verify the agent pauses, surfaces the escalation request, and resumes after human input.

### Tests for User Story 6

- [ ] T070 [P] [US6] Write failing integration test for escalation request posting in n00n-lua/tests/blackboard.rs
- [ ] T071 [P] [US6] Write failing integration test for escalation pause and resume in n00n-lua/tests/agent_control.rs
- [ ] T072 [P] [US6] Write failing integration test for escalation timeout handling in n00n-lua/tests/blackboard.rs

### Implementation for User Story 6

- [ ] T073 [US6] Implement escalation post type in blackboard in plugins/blackboard/init.lua (FR-010)
- [ ] T074 [US6] Implement escalation request creation (run_id, reason, required_input, timeout) in plugins/blackboard/init.lua
- [ ] T075 [US6] Extend agent_control resume to accept human input in plugins/agent_control/init.lua (FR-011)
- [ ] T076 [US6] Implement escalation status tracking (pending, answered, expired) in plugins/blackboard/init.lua
- [ ] T077 [US6] Integrate escalation into team plugin failure handling in plugins/team/init.lua (FR-010)
- [ ] T078 [US6] Add escalation timeout enforcement in plugins/blackboard/init.lua
- [ ] T079 [US6] Implement human input delivery to paused agent in plugins/agent_control/init.lua (FR-011)
- [ ] T080 [US6] Add escalation monitoring in control plane in plugins/agent_control/init.lua

**Checkpoint**: User Story 6 should be fully functional and testable independently

---

## Phase 9: User Story 7 - Versioned run lifecycle and TEA checkpoints (Priority: P3)

**Goal**: Each agent run has a versioned lifecycle with checkpoints (start, wave-complete, error, done) that can be inspected and replayed

**Independent Test**: Run a multi-wave team task, force a failure in the middle, and verify I can inspect the checkpoint history and resume from the last successful wave.

### Tests for User Story 7

- [ ] T081 [P] [US7] Write failing integration test for checkpoint creation in n00n-lua/tests/checkpoint.rs
- [ ] T082 [P] [US7] Write failing integration test for checkpoint history inspection in n00n-lua/tests/checkpoint.rs
- [ ] T083 [P] [US7] Write failing integration test for checkpoint resume in n00n-lua/tests/checkpoint.rs

### Implementation for User Story 7

- [ ] T084 [US7] Implement checkpoint record function (wave_id, timestamp, state_snapshot, artifacts) in plugins/lib/n00n/checkpoint.lua (FR-012)
- [ ] T085 [US7] Implement checkpoint load function in plugins/lib/n00n/checkpoint.lua (FR-013)
- [ ] T086 [US7] Implement checkpoint history listing in plugins/lib/n00n/checkpoint.lua (FR-013)
- [ ] T087 [US7] Implement checkpoint resume with modified parameters in plugins/lib/n00n/checkpoint.lua (FR-013)
- [ ] T088 [US7] Integrate checkpoint creation at lifecycle points in plugins/team/init.lua (start, wave-complete, error, done) (FR-012)
- [ ] T089 [US7] Integrate checkpoint creation in plugins/workflow/init.lua
- [ ] T090 [US7] Add checkpoint storage layout in state_dir/projects/{pid}/runs/{run_id}/checkpoints/ in plugins/lib/n00n/checkpoint.lua
- [ ] T091 [US7] Implement checkpoint corruption handling in plugins/lib/n00n/checkpoint.lua
- [ ] T092 [US7] Add checkpoint inspection tool or command

**Checkpoint**: User Story 7 should be fully functional and testable independently

---

## Phase 10: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [ ] T093 [P] Run cargo clippy --all --tests -- -D warnings and fix all warnings
- [ ] T094 [P] Run cargo nextest run --workspace and ensure all tests pass
- [ ] T095 [P] Run stylua --check on all new Lua files (plugins/blackboard/init.lua, plugins/team/waves.lua, plugins/team/validation.lua, plugins/lib/n00n/live_context.lua, plugins/lib/n00n/checkpoint.lua)
- [ ] T096 Add telemetry correlation for blackboard operations in plugins/blackboard/init.lua
- [ ] T097 Add telemetry correlation for policy enforcement in plugins/agent_control/init.lua
- [ ] T098 Add telemetry correlation for wave dispatch in plugins/team/waves.lua
- [ ] T099 Add telemetry correlation for checkpoints in plugins/lib/n00n/checkpoint.lua
- [ ] T100 Run quickstart.md validation commands and verify smoke test passes
- [ ] T101 Verify all functional requirements (FR-001 to FR-013) are covered by implementation
- [ ] T102 Verify all success criteria (SC-001 to SC-006) are met
- [ ] T103 Add feature flags for optional coordination features in plugins/team/init.lua
- [ ] T104 Update documentation for new coordination features
- [ ] T105 [P] Add benchmark task for SC-004: measure defect rate with wave dispatch vs non-wave dispatch on 20-task benchmark suite
- [ ] T106 [P] Add benchmark task for SC-005: measure escalation success rate across 50 representative scenarios
- [ ] T107 [P] Add benchmark task for SC-006: measure checkpoint recording overhead and verify under 5%
- [ ] T108 Add wave dispatch cycle detection and abort/escalation handling in plugins/team/waves.lua
- [ ] T109 Add non-interactive escalation payload handling in plugins/agent_control/init.lua
- [ ] T110 Add fallback to last known good checkpoint on corruption in plugins/lib/n00n/checkpoint.lua

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Stories (Phase 3-9)**: All depend on Foundational phase completion
  - User stories can then proceed in parallel (if staffed)
  - Or sequentially in priority order (P1 → P2 → P3)
- **Polish (Phase 10)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational (Phase 2) - No dependencies on other stories
- **User Story 2 (P1)**: Can start after Foundational (Phase 2) - Depends on US1 blackboard storage
- **User Story 3 (P1)**: Can start after Foundational (Phase 2) - Depends on US1 blackboard for coordination state
- **User Story 4 (P2)**: Can start after Foundational (Phase 2) - Depends on US3 for session visibility
- **User Story 5 (P2)**: Can start after US1, US2, US3 - Depends on blackboard and claiming
- **User Story 6 (P2)**: Can start after US1, US4 - Depends on blackboard and control plane
- **User Story 7 (P3)**: Can start after US1, US5 - Depends on blackboard and wave dispatch

### Within Each User Story

- Tests MUST be written and FAIL before implementation (TDD)
- Core implementation before integration
- Story complete before moving to next priority

### Parallel Opportunities

- All Setup tasks marked [P] can run in parallel
- All Foundational tasks marked [P] can run in parallel (within Phase 2)
- Once Foundational phase completes, US1, US2, US3 can start in parallel (if team capacity allows)
- All tests for a user story marked [P] can run in parallel
- Polish tasks marked [P] can run in parallel

---

## Parallel Example: User Story 1

```bash
# Launch all tests for User Story 1 together:
Task: "Write failing integration test for blackboard write action in n00n-lua/tests/blackboard.rs"
Task: "Write failing integration test for blackboard query action in n00n-lua/tests/blackboard.rs"
Task: "Write failing integration test for concurrent blackboard posts in n00n-lua/tests/blackboard.rs"

# Launch all implementation tasks for User Story 1 together:
Task: "Implement blackboard write action in plugins/blackboard/init.lua"
Task: "Implement blackboard read action in plugins/blackboard/init.lua"
Task: "Implement blackboard query action with filters in plugins/blackboard/init.lua"
```

---

## Implementation Strategy

### MVP First (User Stories 1-3 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL - blocks all stories)
3. Complete Phase 3: User Story 1 (Shared blackboard)
4. Complete Phase 4: User Story 2 (Atomic task claiming)
5. Complete Phase 5: User Story 3 (Implicit live-session visibility)
6. **STOP and VALIDATE**: Test P1 stories independently
7. Deploy/demo if ready

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add User Story 1 → Test independently → Deploy/Demo (MVP foundation)
3. Add User Story 2 → Test independently → Deploy/Demo
4. Add User Story 3 → Test independently → Deploy/Demo (P1 complete)
5. Add User Story 4 → Test independently → Deploy/Demo
6. Add User Story 5 → Test independently → Deploy/Demo
7. Add User Story 6 → Test independently → Deploy/Demo
8. Add User Story 7 → Test independently → Deploy/Demo (P1-P3 complete)
9. Each story adds value without breaking previous stories

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: User Story 1 (blackboard)
   - Developer B: User Story 2 (atomic claiming)
   - Developer C: User Story 3 (live context)
3. After P1 complete:
   - Developer A: User Story 4 (policy enforcement)
   - Developer B: User Story 5 (wave dispatch)
   - Developer C: User Story 6 (escalation)
4. After P2 complete:
   - Developer A: User Story 7 (checkpoints)
   - Developer B: Polish and cross-cutting
   - Developer C: Documentation and validation
5. Stories complete and integrate independently

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing (TDD)
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence
- All P1-P3 user stories are in scope per spec clarification
- No new external dependencies required per plan.md
- Feature flags ensure backward compatibility with single-session workflows
