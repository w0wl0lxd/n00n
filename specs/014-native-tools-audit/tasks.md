---

description: "Task list for native tools audit feature"
---

# Tasks: Native Tools Audit

**Input**: Design documents from `/specs/014-native-tools-audit/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md

**Tests**: N/A (documentation-only feature; verification is manual review)

**Organization**: Tasks are grouped by user story to enable independent completion of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- Documentation: `specs/014-native-tools-audit/`
- No source code changes required

## Phase 1: Documentation Completion

**Purpose**: Complete spec.md, plan.md, and review research.md

- [ ] T001 [US1] Write spec.md with user stories, requirements, and success criteria in specs/014-native-tools-audit/spec.md
- [ ] T002 [US1] Write plan.md with technical context and project structure in specs/014-native-tools-audit/plan.md
- [ ] T003 [US1] Review research.md to ensure it aligns with spec.md requirements in specs/014-native-tools-audit/research.md

**Checkpoint**: Documentation complete and ready for review

---

## Phase 2: User Story 1 - Audit methodology documentation (Priority: P1) 🎯 MVP

**Goal**: Deliver a documented audit methodology that can be applied to identify native tool candidates

**Independent Test**: Apply the methodology to the n00n codebase and verify it produces the candidate table in research.md

### Tasks for User Story 1

- [ ] T004 [US1] Verify research.md documents the audit methodology with clear steps for identifying candidates in specs/014-native-tools-audit/research.md
- [ ] T005 [US1] Verify research.md includes the evaluation rubric (frequency, token cost, feasibility, priority) in specs/014-native-tools-audit/research.md
- [ ] T006 [US1] Verify research.md documents the evidence sources (AGENTS.md, skill files, justfile, plugin code) in specs/014-native-tools-audit/research.md

**Checkpoint**: User Story 1 complete - audit methodology documented

---

## Phase 3: User Story 2 - Ranked candidate backlog (Priority: P1)

**Goal**: Deliver a prioritized list of native tool candidates with token-savings estimates and design notes

**Independent Test**: Review the candidate table in research.md and verify it includes all required attributes

### Tasks for User Story 2

- [ ] T007 [US2] Verify research.md candidate table includes columns: tool name, current pattern, frequency, token cost, feasibility, priority in specs/014-native-tools-audit/research.md
- [ ] T008 [US2] Verify research.md includes at least 7 candidates (cargo, just, docs, docker/podman, nix, ssh/remote, npm/pip) in specs/014-native-tools-audit/research.md
- [ ] T009 [US2] Verify research.md top 3-5 tools each have a one-page design note with implementation approach, tool schema, and expected token savings in specs/014-native-tools-audit/research.md
- [ ] T010 [US2] Verify research.md excludes candidates already tracked in active epics (github/gix, tmux) in specs/014-native-tools-audit/research.md

**Checkpoint**: User Story 2 complete - ranked candidate backlog delivered

---

## Phase 4: User Story 3 - Follow-up issue recommendations (Priority: P2)

**Goal**: Create at least one follow-up GitHub issue for the highest-priority candidate

**Independent Test**: Verify a GitHub issue exists for the cargo tool with references to the audit

### Tasks for User Story 3

- [ ] T011 [US3] Create GitHub issue for cargo native tool with title "Native cargo tool for build/test/lint" via gh issue create
- [ ] T012 [US3] Include design note from research.md in the cargo issue body
- [ ] T013 [US3] Reference audit issue #239 and epic #240 in the cargo issue body
- [ ] T014 [US3] Label the cargo issue with appropriate labels (e.g., enhancement, native-tools) via gh issue edit

**Checkpoint**: User Story 3 complete - follow-up issue created

---

## Phase 5: Polish & Cross-Cutting Concerns

**Purpose**: Finalize documentation and prepare for delivery

- [ ] T015 [P] Stage all documentation files (spec.md, plan.md, research.md, tasks.md) in specs/014-native-tools-audit/
- [ ] T016 Commit with message "docs(specs): add 014 native tools audit spec, research, plan, and tasks"
- [ ] T017 Push branch to origin with `git push -u origin 014-native-tools-audit`
- [ ] T018 Open draft PR with title "draft: Audit and expand other native tool opportunities" via gh pr create
- [ ] T019 Verify PR body includes "Part of #240. Addresses #239."

---

## Dependencies & Execution Order

### Phase Dependencies

- **Documentation Completion (Phase 1)**: No dependencies - can start immediately
- **User Story 1 (Phase 2)**: Depends on Documentation Completion
- **User Story 2 (Phase 3)**: Depends on Documentation Completion (can run in parallel with US1)
- **User Story 3 (Phase 4)**: Depends on User Story 2 completion (needs candidate backlog)
- **Polish (Phase 5)**: Depends on all user stories complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Documentation Completion - No dependencies on other stories
- **User Story 2 (P1)**: Can start after Documentation Completion - Can run in parallel with US1
- **User Story 3 (P2)**: Depends on US2 completion (needs the ranked backlog to know which issue to create)

### Within Each User Story

- Tasks are sequential verification steps
- No parallel opportunities within a single user story

### Parallel Opportunities

- T004, T005, T006 (US1 verification tasks) can run in parallel
- T007, T008, T009, T010 (US2 verification tasks) can run in parallel
- T015 (stage files) can run in parallel with other polish tasks

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Documentation Completion
2. Complete Phase 2: User Story 1
3. **STOP and VALIDATE**: Verify audit methodology is documented and applicable
4. Deliver documentation

### Incremental Delivery

1. Complete Documentation Completion → Foundation ready
2. Add User Story 1 → Verify methodology → Deliver
3. Add User Story 2 → Verify candidate backlog → Deliver
4. Add User Story 3 → Create follow-up issue → Deliver
5. Each phase adds value without breaking previous phases

### Sequential Strategy (Recommended for this feature)

Since this is a documentation-only feature with a single implementer:

1. Complete Phase 1: Documentation Completion
2. Complete Phase 2: User Story 1 (verify methodology)
3. Complete Phase 3: User Story 2 (verify candidate backlog)
4. Complete Phase 4: User Story 3 (create follow-up issue)
5. Complete Phase 5: Polish and deliver PR

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- This is a documentation-only feature; no code implementation is required
- Verification is manual review against GitHub issue #239 acceptance criteria
- The follow-up issue (T011-T014) is the only external action beyond documentation
- Commit after each phase or logical group
- Stop at any checkpoint to validate phase independently
- Avoid: vague tasks, missing file paths, incomplete verification steps