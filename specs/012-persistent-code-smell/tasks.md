# Tasks: Persistent code-smell and comment index

**Input**: Design documents from `/specs/012-persistent-code-smell/`

## Phase 1: Setup (Shared Infrastructure)

- [x] T001 Create feature structure and specify docs (`specs/012-persistent-code-smell/`)
- [ ] T002 Add `n00n-smell` to root `Cargo.toml` workspace members and dependencies
- [ ] T003 Create `n00n-smell/Cargo.toml` and `n00n-smell/src/` skeleton

---

## Phase 2: Foundational (Blocking Prerequisites)

- [ ] T004 Implement `SmellError` in `n00n-smell/src/error.rs`
- [ ] T005 Implement `SmellFinding` and `SmellIndex` schema in `n00n-smell/src/lib.rs`
- [ ] T006 Implement `SmellIndex::open_or_create` and `update` using `n00n-git::conflicts::find`
- [ ] T007 Implement `SmellIndex::search` with optional `kind` filter
- [ ] T008 Add unit tests for indexing and searching a temp repo

**Checkpoint**: `n00n-smell` library builds and tests pass in isolation.

---

## Phase 3: User Story 1 - Index smells from a repo (P1)

- [ ] T009 [US1] Implement `n00n-smell` CLI binary in `n00n-smell/src/main.rs` (`index` command)
- [ ] T010 [US1] Test `n00n-smell index <repo>` on the feature worktree

---

## Phase 4: User Story 2 - Query smells by kind and keyword (P2)

- [ ] T011 [US2] Implement `n00n-smell search <repo> <query>` with `--kind` and `--top-k`
- [ ] T012 [US2] Add unit/integration tests for search and kind filtering

---

## Phase 5: User Story 3 - Smell tool in n00n (P3)

- [ ] T013 [US3] Create `n00n-lua/src/api/smell.rs` with `has_index`, `index`, `search`
- [ ] T014 [US3] Register `n00n.smell` in `n00n-lua/src/api/mod.rs`
- [ ] T015 [US3] Create `plugins/smell/init.lua` as a built-in `smell` tool
- [ ] T016 [US3] Add `n00n-smell` docs to `n00n-lua/src/docs.rs` Lua API docs if needed

---

## Phase 6: Polish & Cross-Cutting Concerns

- [ ] T017 Run `cargo clippy --all --tests -- -D warnings`
- [ ] T018 Run `cargo nextest run --workspace`
- [ ] T019 Run `just gen-docs` and `cargo run -p n00n-token-profile --example write_baseline`
- [ ] T020 Commit, push, and open a draft PR

---

## Dependencies & Execution Order

- Phase 1 must happen before Phase 2.
- Phase 2 blocks Phases 3-5.
- Phase 3, 4, and 5 can be done in sequence (CLI before Lua).
- Phase 6 after all implementation.
