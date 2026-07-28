# Tasks: Q3 2026 Feature Roadmap

**Input**: `/specs/007-q3-feature-roadmap/plan.md`

## Format: `[ID] [P?] [Wave] Description`

---

## Phase 1: Roadmap setup (this PR)

- [x] T001 [P] Inventory specs 001–006, open PRs, and issue #155
- [x] T002 [P] Research current implementation via codegraph, semble, gh CLI
- [x] T003 Create `specs/007-q3-feature-roadmap/{spec,plan,research,tasks}.md`
- [x] T004 Create worktree at `/mnt/build/worktrees/n00n-b47d8daf/spec-q3-feature-roadmap` on `spec/q3-feature-roadmap`
- [ ] T005 Commit and push roadmap branch; open draft PR for review
- [ ] T006 User approves wave priorities — **approved: Wave 1 all parallel + F9 as Wave 1.5**

---

## Phase 2: Wave 1 — Platform parity (parallel)

### F1 — Agent scripting parity

- [ ] T010 [P] [W1] Rebase `feat/agent-scripting-parity` onto `origin/main`
- [ ] T011 [W1] Audit `src/cmd/agent.rs` for `--json`, `--all`, `--cwd` gaps vs spec 006
- [ ] T012 [W1] Add/fix unit tests in `n00n-daemon` for `AgentScriptView` and filters
- [ ] T013 [W1] Run `./scripts/smoke-daemon.sh`; fix failures
- [ ] T014 [W1] `cargo clippy -p n00n-daemon -p n00n --tests -- -D warnings`
- [ ] T015 [W1] Open draft PR stacked on main

### F2 — CacheHealth non-OpenAI (#155)

- [ ] T020 [P] [W1] Rebase `feat/cache-health-timer` (#154) onto main
- [ ] T021 [W1] Implement Anthropic CacheHealth emitter (5m / 1h TTL)
- [ ] T022 [P] [W1] Implement OpenRouter cache event translation
- [ ] T023 [P] [W1] Implement Mistral `prompt_cache_key` health (document if no TTL)
- [ ] T024 [P] [W1] Evaluate Google/Gemini cache semantics; emit or `valid_until=0`
- [ ] T025 [W1] Per-provider unit tests; UI badge integration test
- [ ] T026 [W1] Close #155 on merge

### F3 — Token reduction PR2/PR3

- [ ] T030 [P] [W1] Branch `feat/token-reduction-pr2` from main
- [ ] T031 [W1] TDD: failing `n00n-token-profile` test expecting lower `main_tools_schemas` tokens
- [ ] T032 [W1] Trim tool descriptions / examples per spec 004; update baseline
- [ ] T033 [W1] Merge PR2; branch `feat/token-reduction-pr3`
- [ ] T034 [W1] TDD: failing test for `system_prompt` reduction
- [ ] T035 [W1] Compact system prompt static blocks; update baseline

### F4 — Explore stack (#170)

- [ ] T040 [W1] Rebase #170 onto main tip; resolve conflicts
- [ ] T041 [W1] `cargo nextest run --workspace`
- [ ] T042 [W1] `just gen-docs`; verify explore tool docs
- [ ] T043 [W1] Merge #170; delete stacked phase branches if obsolete

### F9 — Skill system v2 (#172) — Wave 1.5

- [ ] T044 [W1.5] Rebase `feat/skill-system-v2` (#172) onto post-Wave-1 main
- [ ] T045 [W1.5] Run skill policy enforcement tests from PR #172
- [ ] T046 [W1.5] Merge #172

---

## Phase 3: Wave 2 — Coordination + lifecycle

### F5 — ALMAS convergence

- [ ] T050 [W2] Run `/speckit.converge` on specs 001, 002, 003
- [ ] T051 [W2] Add `n00n-lua/tests/blackboard.rs` coverage if missing
- [ ] T052 [W2] Add team plugin tests: waves, checkpoints, human_escalation
- [ ] T053 [W2] Sync `specs/002` and `003` task checklists with reality
- [ ] T054 [W2] `cargo nextest run -p n00n-lua`

### F6 — Agent lifecycle CLI

- [ ] T060 [W2] `/speckit.specify` for `008-agent-lifecycle-cli`
- [ ] T061 [W2] Implement `n00n agent attach <id>`
- [ ] T062 [W2] Implement `n00n agent respawn <id>`
- [ ] T063 [W2] Implement `n00n agent logs <id> --tail N`
- [ ] T064 [W2] Integration tests + smoke-daemon extension

---

## Phase 4: Wave 3 — API completeness (parallel)

### F7 — Tree-sitter API

- [ ] T070 [P] [W3] Implement cursor-based node lookup in `n00n-lua/src/api/treesitter/mod.rs`
- [ ] T071 [P] [W3] Support custom grammar `path` in `language.rs`
- [ ] T072 [W3] Implement named built-in query lookup in `query.rs`
- [ ] T073 [W3] Regenerate `site/docs/content/lua-api/`; Lua integration tests

### F8 — Local thinking budget

- [ ] T080 [P] [W3] Map `ThinkingConfig` in `local.rs` request builder
- [ ] T081 [P] [W3] Same for `custom.rs`
- [ ] T082 [W3] Unit tests for supported/unsupported models

---

## Phase 5: Polish

- [ ] T090 Update CHANGELOG fragments for each merged feature
- [ ] T091 Verify all eight features have merged PRs or documented deferral
- [ ] T092 Remove worktree `/mnt/build/worktrees/n00n-b47d8daf/spec-q3-feature-roadmap` after merge
