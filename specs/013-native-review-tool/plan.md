# Implementation Plan: Native Review Tool

**Branch**: `013-native-review-tool` | **Date**: 2026-08-03 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/013-native-review-tool/spec.md`

---

## Summary

This feature adds a native `review` built-in tool that integrates existing n00n primitives (arbor/codegraph for blast radius, skill system for checklists, subagent.launch for adversarial passes) into a unified interface for reviewing diffs, PRs, and changes. The tool is implemented as a Lua plugin (`plugins/review/init.lua`) with no new Rust crate, using existing n00n APIs for git/github integration, blast-radius analysis, skill loading, and subagent launching. It supports multiple targets (pr, branch, commit, file, diff), focus areas (security, correctness, performance, style, all), depth levels (quick, thorough), and output formats (findings, comment_draft, both).

---

## Technical Context

**Language/Version**: Lua 5.1 (Luau runtime in n00n-lua) for plugin implementation; Rust 2024 edition for any Rust host functions (if needed).

**Primary Dependencies**:
- Existing n00n primitives: `n00n.arbor` (arbor diff), `n00n.codegraph` (affected), `n00n.skill` (skill loading), `n00n.subagent.launch` (subagent spawning).
- Future dependency: `n00n.github` (from issue #236) for PR diff fetching (optional, with fallback to local git).
- Git integration: `git` command via the `bash` plugin or future `n00n.git` crate.

**Storage**: No new persistent storage. Uses existing project-side indexes (`.arbor/`, `.codegraph/`) and skill directories (`~/.claude/skills`, `~/.agents/skills`).

**Testing**: `cargo test -p n00n-lua`, Lua plugin tests in `plugins/review/tests/`, manual smoke tests with fixture repositories.

**Target Platform**: Linux primary; macOS secondary. All operations use existing n00n cross-platform primitives.

**Project Type**: CLI/TUI agent with built-in Lua plugins.

**Performance Goals**:
- `review` with `depth="quick"` must complete within 30 seconds for a typical diff (≤50 files) on the n00n repository.
- `review` with `depth="thorough"` has no strict time limit but should provide progress indicators for long-running operations.

**Constraints**:
- `unsafe_code = "deny"` workspace-wide; no new Rust code means no new unsafe blocks.
- Tool definition token count must not exceed 2000 tokens.
- Permission scopes (review.read, review.write, review.subagent) must be respected.
- No new Rust crate unless absolutely necessary; prefer Lua-only implementation.

**Scale/Scope**: One new Lua plugin (`plugins/review/init.lua`), updates to config files (DEFAULT_BUILTINS, BUNDLED_PLUGINS), and optional helper modules for diff parsing and findings formatting.

---

## Constitution Check

*The project constitution is defined in `AGENTS.md`. The following gates apply before implementation:*

|| Gate | Status | Notes |
||------|--------|-------|
|| No new `unsafe` without review | Pass | No new Rust code planned; implementation is Lua-only. |
|| `cargo clippy --all --tests -- -D warnings` | TBD | Must pass before PR (no new Rust code, so this should be trivial). |
|| `cargo deny check` | TBD | Must pass (no new dependencies, so this should be trivial). |
|| No silent `.ok()` / default fallbacks | Pass | Errors from git/github/arbor/codegraph/skill/subagent will be mapped to clear error messages. |
|| TDD / failing test first | Pass | Each user story will start with a failing Lua test or fixture assertion. |
|| DRY/SRP | Pass | The review plugin has one responsibility: orchestrate review workflow using existing primitives. |
|| No bundled credentials or cloud providers | Pass | GitHub integration uses existing `github` tool with user-supplied credentials; no new credentials. |

---

## Project Structure

### Documentation (this feature)

```text
specs/013-native-review-tool/
├── plan.md              # This file
├── research.md          # Already exists (Phase 0 output)
├── spec.md              # User-facing specification
└── tasks.md             # Phase 2 output (to be created)
```

### Source Code (repository root)

```text
plugins/
├── review/              # New plugin directory
│   ├── init.lua         # Tool registration and handler
│   └── tests/           # Lua plugin tests
│       └── spec.lua     # Test suite
├── lib/
│   └── n00n/
│       └── review_helpers.lua  # Helper module for diff parsing, findings formatting (optional)
n00n-config/src/lib.rs  # Update DEFAULT_BUILTINS list
n00n-lua/src/loader.rs   # Update BUNDLED_PLUGINS list
```

**Structure Decision**: Single Lua plugin with optional helper module. No new Rust crate. The review tool orchestrates existing n00n primitives (arbor, codegraph, skill, subagent, github/git) rather than reimplementing them.

---

## Complexity Tracking

No constitution violations expected. The implementation is a thin orchestration layer over existing n00n primitives:

|| Complexity | Justification | Simpler Alternative Rejected Because |
||------------|---------------|-------------------------------------|
|| Skill loading per focus | Enables specialized review workflows (security vs correctness) | Generic prompt would be less actionable and less focused |
|| Subagent launch for adversarial pass | Provides deeper analysis than a single-pass review | Single-pass review would miss subtle issues and adversarial perspectives |
|| Multiple output formats (findings, comment_draft, both) | Enables programmatic consumption and human-readable output | Single format would limit integration options |

---

## Data Flow

### Review Workflow

1. **Target resolution**: Parse `target` parameter and fetch diff:
   - `pr:<number>`: Call `n00n.github.pr_diff()` if available, else error.
   - `branch:<name>`: Call `git diff <branch>` via bash.
   - `commit:<sha>`: Call `git diff <sha>` via bash.
   - `file:<path>`: Call `git diff <path>` via bash.
   - `diff`: Use provided diff string or call `git diff` via bash.

2. **Diff extraction**: Parse diff to extract changed files and line ranges.

3. **Blast-radius analysis** (if `depth="thorough"`):
   - Call `n00n.arbor.diff()` or `n00n.codegraph.affected()` with changed files.
   - Extract affected symbols, callers, and tests.

4. **Skill loading**: Load skill based on `focus`:
   - `security` → `security-review` skill
   - `correctness` → `code-review` skill
   - `performance` → `code-review` skill (with performance emphasis)
   - `style` → `code-review` skill (with style emphasis)
   - `all` → `adversarial` skill
   - Fallback to default adversarial prompt if skill not found.

5. **Subagent launch** (if `review.subagent` permission granted):
   - Build prompt with diff, blast-radius, and skill content.
   - Call `n00n.subagent.launch()` with structured output schema for findings.
   - Parse findings into structured format (severity, location, suggestion, focus).

6. **Output formatting**:
   - `findings`: Return JSON array of findings.
   - `comment_draft`: Format findings as markdown PR comment.
   - `both`: Return both formats.

### Key Modules

- `plugins/review/init.lua`: Main tool registration and handler.
- `plugins/lib/n00n/review_helpers.lua` (optional): Helper functions for:
  - Diff parsing (extract changed files, line ranges).
  - Findings formatting (severity sorting, markdown generation).
  - Skill fallback logic.
  - Token budget application for large diffs.

---

## Phased Roadmap

### Phase 0: Validation Spikes

1. **Skill system verification**: Test loading `security-review`, `code-review`, and `adversarial` skills from the skill system. Verify frontmatter parsing and content extraction.
2. **Subagent launch verification**: Test `n00n.subagent.launch()` with a simple adversarial prompt and structured output schema. Verify result parsing.
3. **Arbor/codegraph blast-radius verification**: Test `n00n.arbor.diff()` and `n00n.codegraph.affected()` on a fixture repository with changes. Verify output format.
4. **Git diff extraction**: Test `git diff` via bash plugin on a fixture repository. Verify diff parsing to extract changed files.

### Phase 1: Plugin Scaffolding

1. Create `plugins/review/` directory with `init.lua` skeleton.
2. Register `review` tool with schema (target, focus, depth, output).
3. Add `review` to `DEFAULT_BUILTINS` in `n00n-config/src/lib.rs`.
4. Add `review` to `BUNDLED_PLUGINS` in `n00n-lua/src/loader.rs`.
5. Run `cargo check --workspace` to verify config changes.
6. Create `plugins/review/tests/spec.lua` with basic test skeleton.

### Phase 2: Local Diff Review (US1 - P1)

1. Implement target resolution for `diff` target (local `git diff`).
2. Implement diff parsing to extract changed files.
3. Implement blast-radius analysis using `n00n.arbor.diff()` or `n00n.codegraph.affected()`.
4. Implement skill loading for `focus="security"` (load `security-review` skill).
5. Implement subagent launch with diff + blast-radius + skill prompt.
6. Implement structured findings output (severity, location, suggestion, focus).
7. Add tests for local diff review with `focus="security"`.
8. Handle edge cases: no changes, no index, blast-radius failure.

### Phase 3: Skill-Based Focus Selection (US2 - P1)

1. Implement skill loading for all focus values (security, correctness, performance, style, all).
2. Implement skill fallback logic (use default adversarial prompt if skill not found).
3. Implement focus-specific prompt emphasis (e.g., security focus emphasizes security concerns).
4. Add tests for each focus value on the same fixture diff.
5. Verify that loaded skill matches focus parameter.

### Phase 4: Structured Findings Output (US3 - P1)

1. Implement findings JSON schema (severity, location, suggestion, focus).
2. Implement findings severity sorting (critical > high > medium > low).
3. Implement markdown comment draft formatting (grouped by severity, GitHub-compatible).
4. Implement `output` parameter handling (findings, comment_draft, both).
5. Add tests for each output format.
6. Verify findings can be parsed and sorted programmatically.

### Phase 5: GitHub PR Review Integration (US4 - P2)

1. Implement target resolution for `pr:<number>` (call `n00n.github.pr_diff()` if available).
2. Implement target resolution for `branch:<name>` (call `git diff <branch>`).
3. Implement target resolution for `commit:<sha>` (call `git diff <sha>`).
4. Implement target resolution for `file:<path>` (call `git diff <path>`).
5. Add GitHub tool availability check and fallback error.
6. Add tests for each target type (using fixture PR if possible, or mocked github tool).
7. Handle edge cases: GitHub auth failure, missing remote, invalid PR number.

### Phase 6: Depth Control and Performance (US5 - P2)

1. Implement `depth="quick"` logic (skip blast-radius if expensive, limit diff size).
2. Implement `depth="thorough"` logic (full blast-radius, comprehensive subagent prompt).
3. Implement token budget application for large diffs (truncate with clear message).
4. Add progress indicators for long-running operations (using ExploreResult).
5. Add performance tests (measure latency for quick vs thorough on fixture diff).
6. Verify quick review completes within 30 seconds for typical diff.

### Phase 7: Permission Scopes and Safety (US6 - P3)

1. Implement permission checks for `review.read`, `review.write`, `review.subagent`.
2. Implement fallback behavior when `review.subagent` is denied (non-subagent review).
3. Implement fallback behavior when `review.write` is denied (return draft without posting).
4. Implement error when `review.read` is denied (block all review operations).
5. Add tests for each permission scope.
6. Verify clear error messages for unauthorized operations.

### Phase 8: Integration, Validation, and Docs

1. Update tool description to position review as a first-tier code-quality tool.
2. Add prompt hints in `n00n-agent/src/prompts/` to recommend review for code changes.
3. Run `cargo nextest run --workspace`, `cargo clippy --all --tests -- -D warnings`, `cargo deny check`.
4. Create integration test fixture under `tests/fixtures/review-repo/`.
5. Add user-facing docs in `site/docs/` (review tool usage, focus selection, output formats).
6. Measure tool definition token count and verify ≤2000 tokens.
7. Draft PR with performance comparison and feature demo.

---

## Quick Validation Guide

1. Create a fixture repository with staged changes: `mkdir /tmp/review-fixture && cd /tmp/review-fixture && git init && echo "fn main() {}" > main.rs && git add .`
2. Run review with local diff: `n00n "review target='diff' focus='security' output='findings'"`
3. Verify output contains structured findings with severity, location, suggestion fields.
4. Run review with different focus: `n00n "review target='diff' focus='correctness'"`
5. Verify loaded skill matches focus (check logs or skill telemetry).
6. Run review with comment draft: `n00n "review target='diff' output='comment_draft'"`
7. Verify output is valid markdown formatted for GitHub PR comments.
8. Run review with quick depth: `n00n "review target='diff' depth='quick'"`
9. Verify completion within 30 seconds for typical diff.