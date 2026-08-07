---

description: "Task list for native GitHub tool using gix/gitoxide"
---

# Tasks: Native GitHub Tool Using gix/gitoxide

**Input**: Design documents from `/specs/011-native-github-tool/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md

**Tests**: Tests are REQUIRED for this feature - unit tests for git operations, integration tests for GitHub API.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- Workspace root: `n00n-git/`, `n00n-lua/`, `plugins/`, `n00n-config/`, `Cargo.toml`
- Tests: `n00n-git/tests/` for unit tests, `n00n-lua/tests/` for integration tests

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and basic structure

- [ ] T001 Add `n00n-git` to workspace members in root `Cargo.toml`
- [ ] T002 Add `gix` dependency to workspace `Cargo.toml` with feature flags (max-performance, status, diff, blame, reference, index, commit)
- [ ] T003 Create `n00n-git/Cargo.toml` with dependencies: gix (workspace), thiserror, serde, serde_json
- [ ] T004 Create `n00n-git/src/lib.rs` with module declarations and public error types
- [ ] T005 Create `n00n-git/src/error.rs` with GitError enum using thiserror
- [ ] T006 Create `plugins/git/` directory
- [ ] T007 Create `plugins/github/` directory

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [ ] T008 Implement `n00n-git/src/status.rs` with `status(path)` function using gix-status
- [ ] T009 Implement `n00n-git/src/log.rs` with `log(path, count)` function using gix commit iteration
- [ ] T010 Implement `n00n-git/src/diff.rs` with `diff(path, ref_a, ref_b)` function using gix-diff
- [ ] T011 Implement `n00n-git/src/branches.rs` with `branches(path)` function using gix-reference
- [ ] T012 Implement `n00n-git/src/blame.rs` with `blame(path, file)` function using gix-blame
- [ ] T013 Implement `n00n-git/src/write.rs` with `add(path, files)`, `commit(path, message)`, `checkout(path, branch)` functions
- [ ] T014 Add `n00n-git` to `n00n-lua/Cargo.toml` dependencies
- [ ] T015 Add `reqwest` to `n00n-lua/Cargo.toml` dependencies if not already present
- [ ] T016 Update `n00n-lua/src/api/mod.rs` to register `n00n.git` and `n00n.github` tables
- [ ] T017 Define permission scopes `git.read`, `git.write`, `github.read`, `github.write` in `n00n-config/src/lib.rs`
- [ ] T018 Add `"git"` and `"github"` to `DEFAULT_BUILTINS` in `n00n-config/src/lib.rs`

**Checkpoint**: Foundation ready - user story implementation can now begin in parallel

---

## Phase 3: User Story 1 - Local git operations via gix (Priority: P1) 🎯 MVP

**Goal**: Enable agents to query local git status, log, diff, and branches without shelling out to git CLI.

**Independent Test**: Run git tool commands (status, log, diff, branches) on a temporary git repository fixture and verify structured JSON output matches expected git state, without requiring git CLI on PATH.

### Tests for User Story 1 ⚠️

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [ ] T019 [P] [US1] Unit test for `status()` in `n00n-git/tests/status_test.rs` using tempfile
- [ ] T020 [P] [US1] Unit test for `log()` in `n00n-git/tests/log_test.rs` using tempfile
- [ ] T021 [P] [US1] Unit test for `diff()` in `n00n-git/tests/diff_test.rs` using tempfile
- [ ] T022 [P] [US1] Unit test for `branches()` in `n00n-git/tests/branches_test.rs` using tempfile
- [ ] T023 [P] [US1] Integration test for git Lua API in `n00n-lua/tests/git_api_test.rs`

### Implementation for User Story 1

- [ ] T024 [P] [US1] Create `n00n-lua/src/api/git.rs` with `create_git_table()` function
- [ ] T025 [US1] Implement `git_status()` Lua host function in `n00n-lua/src/api/git.rs` (depends on T008, T024)
- [ ] T026 [US1] Implement `git_log()` Lua host function in `n00n-lua/src/api/git.rs` (depends on T009, T024)
- [ ] T027 [US1] Implement `git_diff()` Lua host function in `n00n-lua/src/api/git.rs` (depends on T010, T024)
- [ ] T028 [US1] Implement `git_branches()` Lua host function in `n00n-lua/src/api/git.rs` (depends on T011, T024)
- [ ] T029 [US1] Implement `git_blame()` Lua host function in `n00n-lua/src/api/git.rs` (depends on T012, T024)
- [ ] T030 [US1] Add error handling and permission checks for git.read in `n00n-lua/src/api/git.rs`
- [ ] T031 [US1] Create `plugins/git/init.lua` with git tool registration following arbor/codegraph pattern
- [ ] T032 [US1] Add structured logging for git operations in `n00n-lua/src/api/git.rs`

**Checkpoint**: At this point, User Story 1 should be fully functional and testable independently

---

## Phase 4: User Story 2 - GitHub remote API access (Priority: P1) 🎯 MVP

**Goal**: Enable agents to create, list, and query GitHub objects via native API calls instead of shelling out to gh.

**Independent Test**: Run github tool commands (list_issues, create_issue, list_prs) against a test repository using a test token, verifying structured JSON output matches GitHub API responses, without requiring gh CLI.

### Tests for User Story 2 ⚠️

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [ ] T033 [P] [US2] Integration test for GitHub client in `n00n-lua/tests/github_client_test.rs` (requires test token)
- [ ] T034 [P] [US2] Integration test for list_issues in `n00n-lua/tests/github_api_test.rs`
- [ ] T035 [P] [US2] Integration test for create_issue in `n00n-lua/tests/github_api_test.rs`
- [ ] T036 [P] [US2] Integration test for list_prs in `n00n-lua/tests/github_api_test.rs`

### Implementation for User Story 2

- [ ] T037 [P] [US2] Create `n00n-lua/src/api/github.rs` with GitHubClient struct using reqwest
- [ ] T038 [US2] Implement token authentication from GITHUB_TOKEN env var and n00n config in `n00n-lua/src/api/github.rs` (depends on T037)
- [ ] T039 [US2] Implement `gh_auth_token()` fallback detection in `n00n-lua/src/api/github.rs` (depends on T037)
- [ ] T040 [US2] Implement rate limit handling in GitHubClient in `n00n-lua/src/api/github.rs` (depends on T037)
- [ ] T041 [US2] Implement `list_issues()` function in `n00n-lua/src/api/github.rs` (depends on T037)
- [ ] T042 [US2] Implement `create_issue()` function in `n00n-lua/src/api/github.rs` (depends on T037)
- [ ] T043 [US2] Implement `get_issue()` function in `n00n-lua/src/api/github.rs` (depends on T037)
- [ ] T044 [US2] Implement `list_prs()` function in `n00n-lua/src/api/github.rs` (depends on T037)
- [ ] T045 [US2] Implement `get_pr()` function in `n00n-lua/src/api/github.rs` (depends on T037)
- [ ] T046 [US2] Implement `get_repo()` function in `n00n-lua/src/api/github.rs` (depends on T037)
- [ ] T047 [US2] Create `create_github_table()` function in `n00n-lua/src/api/github.rs`
- [ ] T048 [US2] Implement Lua host functions for GitHub operations in `n00n-lua/src/api/github.rs` (depends on T041-T046, T047)
- [ ] T049 [US2] Add error handling and permission checks for github.read in `n00n-lua/src/api/github.rs`
- [ ] T050 [US2] Create `plugins/github/init.lua` with github tool registration following arbor/codegraph pattern
- [ ] T051 [US2] Add structured logging for GitHub operations in `n00n-lua/src/api/github.rs`

**Checkpoint**: At this point, User Stories 1 AND 2 should both work independently

---

## Phase 5: User Story 3 - Git write operations with scoped permissions (Priority: P2)

**Goal**: Enable agents to perform git write operations (add, commit, checkout) behind scoped permissions.

**Independent Test**: Run git write commands (add, commit, checkout) on a temporary repository with git.write permission granted, verifying operations succeed and modify repository state correctly, and fail when permission is denied.

### Tests for User Story 3 ⚠️

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [ ] T052 [P] [US3] Unit test for `add()` in `n00n-git/tests/write_test.rs` using tempfile
- [ ] T053 [P] [US3] Unit test for `commit()` in `n00n-git/tests/write_test.rs` using tempfile
- [ ] T054 [P] [US3] Unit test for `checkout()` in `n00n-git/tests/write_test.rs` using tempfile
- [ ] T055 [P] [US3] Integration test for git.write permission enforcement in `n00n-lua/tests/git_permissions_test.rs`

### Implementation for User Story 3

- [ ] T056 [US3] Implement `git_add()` Lua host function in `n00n-lua/src/api/git.rs` (depends on T013, T024)
- [ ] T057 [US3] Implement `git_commit()` Lua host function in `n00n-lua/src/api/git.rs` (depends on T013, T024)
- [ ] T058 [US3] Implement `git_checkout()` Lua host function in `n00n-lua/src/api/git.rs` (depends on T013, T024)
- [ ] T059 [US3] Add permission checks for git.write in `n00n-lua/src/api/git.rs`
- [ ] T060 [US3] Update `plugins/git/init.lua` to register write commands (add, commit, checkout)
- [ ] T061 [US3] Add filesystem locking for concurrent git operations in `n00n-git/src/write.rs`

**Checkpoint**: At this point, User Stories 1, 2, AND 3 should all work independently

---

## Phase 6: User Story 4 - GitHub write operations with scoped permissions (Priority: P2)

**Goal**: Enable agents to create GitHub pull requests and comments via native API calls behind scoped permissions.

**Independent Test**: Run GitHub write commands (create_pr, add_comment) on a test repository with github.write permission granted, verifying operations succeed and return correct responses, and fail when permission is denied.

### Tests for User Story 4 ⚠️

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [ ] T062 [P] [US4] Integration test for create_pr in `n00n-lua/tests/github_write_test.rs` (requires test token)
- [ ] T063 [P] [US4] Integration test for add_comment in `n00n-lua/tests/github_write_test.rs` (requires test token)
- [ ] T064 [P] [US4] Integration test for github.write permission enforcement in `n00n-lua/tests/github_permissions_test.rs`

### Implementation for User Story 4

- [ ] T065 [US4] Implement `create_pr()` function in `n00n-lua/src/api/github.rs` (depends on T037)
- [ ] T066 [US4] Implement `add_comment()` function in `n00n-lua/src/api/github.rs` (depends on T037)
- [ ] T067 [US4] Implement Lua host functions for create_pr and add_comment in `n00n-lua/src/api/github.rs` (depends on T065, T066, T047)
- [ ] T068 [US4] Add permission checks for github.write in `n00n-lua/src/api/github.rs`
- [ ] T069 [US4] Update `plugins/github/init.lua` to register write commands (create_pr, add_comment)

**Checkpoint**: All user stories should now be independently functional

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [ ] T070 [P] Run `cargo fmt --all` to format all code
- [ ] T071 [P] Run `cargo clippy --all --tests -- -D warnings` to lint all code
- [ ] T072 [P] Run `cargo nextest run --workspace` to verify all tests pass
- [ ] T073 [P] Run `cargo deny check` to verify supply chain
- [ ] T074 Add documentation comments to all public APIs in `n00n-git/src/`
- [ ] T075 Add documentation comments to all public APIs in `n00n-lua/src/api/git.rs` and `n00n-lua/src/api/github.rs`
- [ ] T076 Verify tool definitions in `plugins/git/init.lua` and `plugins/github/init.lua` match spec requirements
- [ ] T077 Verify permission scopes are correctly enforced in all API functions
- [ ] T078 Add error messages for edge cases (invalid repo path, missing token, rate limits, bare repos)
- [ ] T079 Verify gix feature flags in workspace Cargo.toml minimize dependency bloat
- [ ] T080 Test git operations without git CLI on PATH to verify no external dependency
- [ ] T081 Test GitHub operations without gh CLI on PATH to verify no external dependency

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Stories (Phase 3-6)**: All depend on Foundational phase completion
  - User Story 1 (P1) and User Story 2 (P1) can proceed in parallel after Foundational
  - User Story 3 (P2) and User Story 4 (P2) can proceed in parallel after Foundational
- **Polish (Phase 7)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational (Phase 2) - No dependencies on other stories
- **User Story 2 (P1)**: Can start after Foundational (Phase 2) - No dependencies on other stories
- **User Story 3 (P2)**: Can start after Foundational (Phase 2) - Extends User Story 1 with write operations
- **User Story 4 (P2)**: Can start after Foundational (Phase 2) - Extends User Story 2 with write operations

### Within Each User Story

- Tests MUST be written and FAIL before implementation
- Core functions before Lua host functions
- Lua host functions before plugin registration
- Error handling and permission checks after core implementation
- Story complete before moving to next priority

### Parallel Opportunities

- All Setup tasks (T001-T007) can run in parallel
- All Foundational tasks (T008-T018) can run in parallel within Phase 2
- User Story 1 tests (T019-T023) can run in parallel
- User Story 2 tests (T033-T036) can run in parallel
- User Story 3 tests (T052-T055) can run in parallel
- User Story 4 tests (T062-T064) can run in parallel
- User Story 1 and User Story 2 can be implemented in parallel by different developers
- User Story 3 and User Story 4 can be implemented in parallel by different developers
- All Polish tasks (T070-T081) can run in parallel

---

## Parallel Example: User Story 1

```bash
# Launch all tests for User Story 1 together:
Task: "Unit test for status() in n00n-git/tests/status_test.rs"
Task: "Unit test for log() in n00n-git/tests/log_test.rs"
Task: "Unit test for diff() in n00n-git/tests/diff_test.rs"
Task: "Unit test for branches() in n00n-git/tests/branches_test.rs"
Task: "Integration test for git Lua API in n00n-lua/tests/git_api_test.rs"

# Launch all Lua host functions for User Story 1 together:
Task: "Implement git_status() Lua host function"
Task: "Implement git_log() Lua host function"
Task: "Implement git_diff() Lua host function"
Task: "Implement git_branches() Lua host function"
Task: "Implement git_blame() Lua host function"
```

---

## Implementation Strategy

### MVP First (User Stories 1 and 2 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL - blocks all stories)
3. Complete Phase 3: User Story 1
4. Complete Phase 4: User Story 2
5. **STOP and VALIDATE**: Test User Stories 1 and 2 independently
6. Deploy/demo if ready

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add User Story 1 → Test independently → Deploy/Demo (MVP part 1!)
3. Add User Story 2 → Test independently → Deploy/Demo (MVP part 2!)
4. Add User Story 3 → Test independently → Deploy/Demo
5. Add User Story 4 → Test independently → Deploy/Demo
6. Each story adds value without breaking previous stories

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: User Story 1
   - Developer B: User Story 2
3. After P1 stories complete:
   - Developer A: User Story 3
   - Developer B: User Story 4
4. Stories complete and integrate independently

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence
- GitHub integration tests require test token and repository; use environment variables
- Git unit tests use tempfile and do not require network access
- Follow existing patterns from arbor and codegraph for consistency