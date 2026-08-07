# Feature Specification: Native Review Tool

**Feature Branch**: `013-native-review-tool`

**Created**: 2026-08-03

**Status**: Draft

**Input**: User description: "Add a `review` built-in tool for reviewing diffs, PRs, and changes, using blast-radius, skill policy, and subagents to produce focused, evidence-based review output."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Local diff review with blast radius (Priority: P1)

A user reviewing local changes wants to run `review` on a `git diff` to get a severity-sorted list of findings with locations and suggestions, using blast-radius analysis to focus on affected symbols and tests.

**Why this priority**: This is the core MVP functionality that works immediately without GitHub integration, providing value to any user with local changes. It establishes the review tool's primary workflow and data structures.

**Independent Test**: Can be fully tested by creating a fixture repository with staged changes, running `review target="diff"` with `focus="security"`, and verifying it returns structured findings with severity, location, and suggestion fields.

**Acceptance Scenarios**:

1. **Given** a repository with staged changes and no GitHub integration, **When** the user invokes `review` with `target="diff"` and `focus="security"`, **Then** n00n extracts changed files, runs `arbor diff` or `codegraph affected` for blast radius, loads the `security-review` skill, launches an adversarial subagent, and returns a severity-sorted list of findings.
2. **Given** a repository with no changes, **When** the user invokes `review` with `target="diff"`, **Then** n00n returns an empty findings list with a clear message that no changes were detected.
3. **Given** a repository with changes but no `.arbor/` or `.codegraph/` index, **When** the user invokes `review` with `target="diff"`, **Then** n00n triggers indexing automatically, shows a moving status indicator, and proceeds with the review after indexing completes.

---

### User Story 2 - Skill-based review focus selection (Priority: P1)

A user wants to review changes with different focus areas (security, correctness, performance, style) so that the review checklist and subagent prompt are tailored to the specific concern.

**Why this priority**: Focus selection is a key differentiator from generic diff tools. It enables specialized review workflows (e.g., security-focused reviews for sensitive code) and makes the review output more actionable.

**Independent Test**: Can be fully tested by running `review` with different `focus` values on the same fixture diff and verifying that the loaded skill and output emphasis match the focus (e.g., `security` loads `security-review` skill and emphasizes security findings).

**Acceptance Scenarios**:

1. **Given** a repository with changes, **When** the user invokes `review` with `focus="security"`, **Then** n00n loads the `security-review` skill from the skill system and configures the subagent prompt to emphasize security concerns.
2. **Given** a repository with changes, **When** the user invokes `review` with `focus="correctness"`, **Then** n00n loads the `code-review` skill and configures the subagent prompt to emphasize logic errors and edge cases.
3. **Given** a repository with changes, **When** the user invokes `review` with `focus="all"`, **Then** n00n loads the `adversarial` skill and runs a comprehensive review across all focus areas.

---

### User Story 3 - Structured findings output (Priority: P1)

A user wants review output in a structured format (severity, location, suggestion) so that findings can be programmatically consumed, sorted, or converted to PR comments.

**Why this priority**: Structured output is essential for integration with other tools (e.g., PR comment posting, CI checks) and for consistent review quality. It distinguishes the review tool from generic LLM diff analysis.

**Independent Test**: Can be fully tested by running `review` with `output="findings"` on a fixture diff and parsing the output to verify it contains the required fields (severity, location, suggestion) and can be sorted by severity.

**Acceptance Scenarios**:

1. **Given** a repository with changes, **When** the user invokes `review` with `output="findings"`, **Then** n00n returns a JSON array of findings with fields: severity (critical/high/medium/low), location (file:line), suggestion (text), and focus (security/correctness/performance/style).
2. **Given** a repository with changes, **When** the user invokes `review` with `output="comment_draft"`, **Then** n00n returns a markdown-formatted PR comment draft with findings grouped by severity and formatted for GitHub.
3. **Given** a repository with changes, **When** the user invokes `review` with `output="both"`, **Then** n00n returns both the structured findings array and the markdown comment draft.

---

### User Story 4 - GitHub PR review integration (Priority: P2)

A user reviewing a GitHub PR wants to run `review` with `target="pr:<number>"` to fetch the PR diff from GitHub, analyze it with blast radius, and return findings or a comment draft.

**Why this priority**: GitHub PR review is a common workflow. Integration with the `github` tool (from issue #236) makes the review tool useful for remote collaboration and CI/CD workflows.

**Independent Test**: Can be fully tested by creating a test PR in a fixture repository, running `review` with `target="pr:<number>"`, and verifying it fetches the diff via the `github` tool and returns findings as expected.

**Acceptance Scenarios**:

1. **Given** a repository with a GitHub remote and an open PR, **When** the user invokes `review` with `target="pr:123"` and the `github` tool is available, **Then** n00n fetches the PR diff via `n00n.github.pr_diff()`, runs blast-radius analysis, and returns findings.
2. **Given** a repository with a GitHub remote but the `github` tool is not available, **When** the user invokes `review` with `target="pr:123"`, **Then** n00n returns a clear error suggesting to install the `github` tool or use a local target (branch/commit/diff).
3. **Given** a repository with a GitHub remote, **When** the user invokes `review` with `target="branch:feature-x"`, **Then** n00n fetches the diff between the current branch and `feature-x` via `git diff` and returns findings.

---

### User Story 5 - Depth control and performance (Priority: P2)

A user wants to control review depth (quick vs thorough) so that quick reviews are fast for incremental changes and thorough reviews are comprehensive for critical code.

**Why this priority**: Depth control enables the review tool to scale from fast incremental checks to deep security audits. It addresses performance concerns for large diffs.

**Independent Test**: Can be fully tested by running `review` with `depth="quick"` and `depth="thorough"` on the same fixture diff and measuring latency and output detail.

**Acceptance Scenarios**:

1. **Given** a repository with changes, **When** the user invokes `review` with `depth="quick"`, **Then** n00n runs a focused review on changed files only, skips blast-radius analysis if it would be expensive, and returns findings within 30 seconds for a typical diff.
2. **Given** a repository with changes, **When** the user invokes `review` with `depth="thorough"`, **Then** n00n runs a full review with blast-radius analysis, affected tests, and comprehensive subagent prompting, even if it takes longer.
3. **Given** a repository with a large diff (>100 files), **When** the user invokes `review` with `depth="quick"`, **Then** n00n applies a token budget to limit the diff size and blast-radius scope to stay within the quick time target.

---

### User Story 6 - Permission scopes and safety (Priority: P3)

A user wants the review tool to respect permission scopes (review.read, review.write, review.subagent) so that review operations can be controlled in restricted environments.

**Why this priority**: Permission scopes are required for enterprise and multi-tenant environments where not all agents should be able to launch subagents or post comments.

**Independent Test**: Can be fully tested by configuring permission scopes in `permissions.toml`, running `review` with different operations, and verifying that unauthorized operations are blocked with clear error messages.

**Acceptance Scenarios**:

1. **Given** a repository with the `review.subagent` permission denied, **When** the user invokes `review` with any target, **Then** n00n returns a clear error that adversarial subagent launch is not permitted and falls back to a non-subagent review if possible.
2. **Given** a repository with the `review.write` permission denied, **When** the user invokes `review` with `output="comment_draft"`, **Then** n00n returns the comment draft but does not attempt to post it to GitHub.
3. **Given** a repository with the `review.read` permission denied, **When** the user invokes `review` with any target, **Then** n00n returns a clear error that review operations are not permitted.

---

### Edge Cases

- What happens when the diff is too large for the context window? The tool applies a token budget, truncates the diff with a clear message, and focuses on the most critical files (e.g., by file extension or blast-radius impact).
- What happens when blast-radius analysis fails (e.g., Arbor/CodeGraph unavailable)? The tool falls back to a file-list-only review and reports the fallback in the output.
- What happens when the requested skill is not found? The tool falls back to the `adversarial` skill and reports the fallback.
- What happens when the subagent launch fails (e.g., model unavailable)? The tool returns a clear error and suggests using `depth="quick"` without subagent if available.
- What happens when GitHub authentication fails? The tool returns a clear error with GitHub's error message and suggests using a local target.
- What happens when the repository has no git history? The tool returns a clear error that review requires a git repository and suggests initializing git.
- What happens when multiple review operations run concurrently on the same project? Each operation uses its own subagent session; indexing operations are serialized via existing Arbor/CodeGraph locks.

---

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The `review` tool MUST be a built-in n00n tool registered in `plugins/review/init.lua`.
- **FR-002**: The `review` tool MUST support `target` values: `pr:<number>`, `branch:<name>`, `commit:<sha>`, `file:<path>`, or `diff`.
- **FR-003**: The `review` tool MUST support `focus` values: `security`, `correctness`, `performance`, `style`, or `all`.
- **FR-004**: The `review` tool MUST support `depth` values: `quick` or `thorough`.
- **FR-005**: The `review` tool MUST support `output` values: `findings`, `comment_draft`, or `both`.
- **FR-006**: The `review` tool MUST work with local `git diff` without GitHub integration.
- **FR-007**: The `review` tool MUST integrate with the `github` tool for PR review when available.
- **FR-008**: The `review` tool MUST use `arbor diff` or `codegraph affected` for blast-radius analysis when indexes are available.
- **FR-009**: The `review` tool MUST load review skills from the skill system based on the `focus` parameter.
- **FR-010**: The `review` tool MUST use `n00n.subagent.launch` for adversarial review passes when `review.subagent` permission is granted.
- **FR-011**: The `review` tool MUST return structured findings with fields: severity, location, suggestion, and focus.
- **FR-012**: The `review` tool MUST support markdown-formatted PR comment draft output.
- **FR-013**: The `review` tool MUST respect permission scopes: `review.read`, `review.write`, `review.subagent`.
- **FR-014**: The `review` tool MUST be added to `DEFAULT_BUILTINS` in `n00n-config/src/lib.rs`.
- **FR-015**: The `review` tool MUST be added to `BUNDLED_PLUGINS` in `n00n-lua/src/loader.rs`.

### Key Entities

- **ReviewTarget**: The source of changes to review (PR, branch, commit, file, or raw diff).
- **ReviewFocus**: The review focus area (security, correctness, performance, style, or all) that determines skill selection and prompt emphasis.
- **ReviewDepth**: The review thoroughness level (quick or thorough) that controls blast-radius scope and subagent prompting depth.
- **ReviewOutput**: The output format (findings, comment_draft, or both) that determines the returned data structure.
- **Finding**: A structured review result with fields: severity (critical/high/medium/low), location (file:line), suggestion (text), and focus (security/correctness/performance/style).
- **BlastRadius**: The impact analysis from `arbor diff` or `codegraph affected` that identifies affected symbols, callers, and tests.
- **ReviewSkill**: A skill loaded from the skill system (e.g., `security-review`, `code-review`, `adversarial`) that provides review checklists and prompt templates.

---

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: The `review` tool returns correct structured findings on a fixture repository in 100% of targeted test cases.
- **SC-002**: The `review` tool with `target="diff"` and `depth="quick"` completes within 30 seconds for a typical diff (≤50 files) on the n00n repository.
- **SC-003**: The `review` tool with `focus="security"` loads the `security-review` skill and returns security-focused findings in 100% of test cases.
- **SC-004**: The `review` tool with `output="comment_draft"` returns valid markdown that can be posted as a GitHub PR comment.
- **SC-005**: The `review` tool integrates with the `github` tool for `target="pr:<number>"` when the `github` tool is available.
- **SC-006**: The `review` tool respects permission scopes and blocks unauthorized operations with clear error messages.
- **SC-007**: `cargo test -p n00n-lua` and `cargo test -p n00n-agent` pass with the review tool tests.
- **SC-008**: The review tool definition token count does not exceed 2000 tokens.

---

## Assumptions

- The `github` tool (issue #236) may not be available when the review tool is implemented; the review tool must work with local `git diff` as a fallback.
- The skill system already supports loading skills from `~/.claude/skills` and `~/.agents/skills` with frontmatter metadata.
- The `arbor diff` and `codegraph affected` commands are available through the existing `arbor` and `codegraph` plugins.
- The `n00n.subagent.launch` helper is available in `plugins/lib/n00n/subagent.lua`.
- Review skills (`adversarial`, `security-review`, `code-review`) may or may not exist in the user's skill directories; the tool should fall back to a default adversarial prompt if not found.
- PR comment posting is out of scope for v1; the tool only generates comment drafts.
- The review tool does not need a dedicated Rust crate; it can be implemented entirely in Lua using existing n00n primitives.
