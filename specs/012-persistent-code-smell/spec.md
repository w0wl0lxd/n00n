# Feature Specification: Persistent code-smell and comment index

**Feature Branch**: `012-persistent-code-smell`

**Created**: 2026-08-03

**Status**: Draft

**Input**: User description: "Build a persistent code-smell and comment index addon"

## User Scenarios & Testing

### User Story 1 - Index smells from a repo (Priority: P1)

As a developer, I want to build a searchable on-disk index of all conflict markers, TODO/FIXME/HACK comments, and placeholder phrases in my repository so that the agent can recall them without rescanning the full worktree on every query.

**Why this priority**: This makes the `git conflicts` work persistent and queryable, turning a one-off scan into a long-lived project artifact.

**Independent Test**: Run `n00n-smell index <repo>`, then `n00n-smell search <repo> todo` and see results.

**Acceptance Scenarios**:

1. **Given** a repo containing a `// TODO: fix this` comment, **When** `n00n-smell index` is run, **Then** a `.n00n/smells` directory is created and `n00n-smell search todo` returns the comment.
2. **Given** an existing index, **When** `n00n-smell index` is run again, **Then** the old index is replaced with fresh findings.

---

### User Story 2 - Query smells by kind and keyword (Priority: P2)

As a developer, I want to search the smell index by keywords and filter by kind (todo, fixme, hack, placeholder, conflict) so that I can find only the items I care about.

**Why this priority**: Smells are noisy; filtering by kind and keyword makes the output usable.

**Independent Test**: `n00n-smell search <repo> hack --kind hack` returns only hack comments.

**Acceptance Scenarios**:

1. **Given** an indexed repo with TODOs and FIXMEs, **When** searching with `--kind fixme`, **Then** only FIXME findings are returned.
2. **Given** a query with no matches, **Then** the tool reports "No matches." and exits 0.

---

### User Story 3 - Smell tool in n00n (Priority: P3)

As an n00n user, I want a `smell` tool in the agent so the model can reindex and query smells without leaving the chat.

**Why this priority**: Keeps the workflow inside the agent UI and lets the model make decisions based on smell evidence.

**Independent Test**: Call the `smell` tool with `command = "search"` and `query = "todo"` and receive a ranked string.

**Acceptance Scenarios**:

1. **Given** the `smell` tool is registered, **When** `index` is called, **Then** it returns success and creates the index.
2. **Given** an existing index, **When** `search` is called, **Then** it returns ranked smells.

---

### Edge Cases

- What happens when the repo has no smells? The index should still be valid and searches return no matches.
- What happens when the index directory is missing? Search returns an explicit error.
- How does the system handle binary or huge files? The index reuses the `n00n-git conflicts` scanner, which skips binary files and honors `max_file_bytes`.

## Requirements

### Functional Requirements

- **FR-001**: The system MUST be able to scan a repository for conflict markers and code smells using the same logic as `n00n-git conflicts`.
- **FR-002**: The system MUST persist findings in a Tantivy index under `.n00n/smells`.
- **FR-003**: The system MUST support BM25 keyword search over smell content and message fields.
- **FR-004**: The system MUST allow filtering search results by `kind`.
- **FR-005**: The system MUST expose a Rust binary `n00n-smell` with `index` and `search` subcommands.
- **FR-006**: The system MUST expose a Lua API `n00n.smell` and a built-in `smell` tool.
- **FR-007**: The index MUST be replaceable on every `index` run; stale entries must not survive.

### Key Entities

- **SmellFinding**: A document with `path`, `start_line`, `end_line`, `kind`, `message`, `content`, and `language`.
- **SmellIndex**: Tantivy-backed index stored at `.n00n/smells`, supporting open/create/update/search.

## Success Criteria

- **SC-001**: `n00n-smell index <repo>` completes in under 10s on a 100k-line repository.
- **SC-002**: `n00n-smell search <repo> todo` returns TODO findings in under 500ms.
- **SC-003**: The `smell` tool is available in the built-in tool set and works without a `semblem` CLI.
- **SC-004**: All hard gates pass (`cargo clippy --all --tests -- -D warnings`, `cargo nextest run --workspace`).

## Assumptions

- The repository uses `n00n-git conflicts` scanning logic as the source of smells.
- `tantivy` 0.26.1 is available in the workspace.
- The feature is additive; it does not replace `n00n-semble` or `n00n-codegraph`.
