# Tasks: Agent Coordination Execution

**Input**: Design documents from `/specs/003-agent-coordination-execution/`

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel
- **[Story]**: User story (US1, US2, US3, US4)

## Phase 2: Foundational

- [ ] T001 [P] Add `waves` and `checkpoints` schema fields to `team` tool in `plugins/team/init.lua`.
- [ ] T002 [P] Add `resume` field handling to load checkpoint in `plugins/team/init.lua`.
- [ ] T003 Expand `plugins/team/waves.lua` to compute waves and provide a runner interface.
- [ ] T004 Expand `plugins/team/validation.lua` to score wave output and return PASS/FAIL with explanation.

## Phase 3: User Story 1 - Wave dispatch (P1)

- [ ] T005 [US1] Run validation gate before each wave in `plugins/team/init.lua`.
- [ ] T006 [US1] Execute plan/implement/validate waves sequentially; skip empty waves.
- [ ] T007 [US1] On validation failure, re-run the preceding wave with feedback up to `max_wave_retries`.
- [ ] T008 [US1] Ensure `waves = false` preserves current sequential behavior.

## Phase 4: User Story 2 - Checkpoints (P1)

- [ ] T009 [US2] Persist checkpoint after each successful wave via `plugins/lib/n00n/checkpoint.lua`.
- [ ] T010 [US2] Load latest checkpoint on `resume` and reconstruct `results`, `total_cost`, `total_usage`, `wave_index`, `step_index`.
- [ ] T011 [US2] Skip completed steps when resuming and append new results to prior results.
- [ ] T012 [P] [US2] Add Rust test for checkpoint round-trip in `n00n-lua/tests/blackboard.rs`.

## Phase 5: User Story 3 - Policy enforcement (P2)

- [ ] T013 [US3] Implement `agent_control.evaluate_policy(agent_id, session_type, tags, tool_name)` in `plugins/agent_control/init.lua`.
- [ ] T014 [US3] Store `tags` on sessions/agents and expose them to policy evaluation.
- [ ] T015 [US3] Wrap `n00n.agent.call_tool` in a policy filter for `team` and `agent_control` usage.
- [ ] T016 [P] [US3] Add tests for restricted and allowed tool policies.

## Phase 6: User Story 4 - Enhanced live context (P2)

- [ ] T017 [US4] Query blackboard for `status` and `claim` posts in `plugins/lib/n00n/live_context.lua`.
- [ ] T018 [US4] Map active claims to session entries using `task_id` or session `id`.
- [ ] T019 [US4] Add `recent_posts` to each live context entry.
- [ ] T020 [US4] Provide `live_context.snapshot()` with graceful fallback if blackboard is unavailable.

## Phase 7: Polish

- [ ] T021 Run `cargo check --all`, `cargo clippy --all --tests -- -D warnings`, `cargo nextest run --workspace`.
- [ ] T022 Update `n00n-docgen/src/gen_tools.rs` if new tools are added.
- [ ] T023 Commit changes with Conventional Commit messages and push `spec/almas-coordination-followup`.
- [ ] T024 Open a stacked draft PR against `spec/almas-coordination-visibility`.
