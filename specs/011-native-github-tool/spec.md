# Feature Specification: Native GitHub Tool Using gix/gitoxide

**Feature Branch**: `011-native-github-tool`

**Created**: 2026-08-03

**Status**: Draft

**Input**: GitHub issue #236: Add native `github` (and local `git`) tools backed by an embedded `gix/gitoxide` crate, so agents can query and mutate repositories and GitHub objects without shelling out to `gh` or `git` for everything.

## User Scenarios & Testing

### User Story 1 - Local git operations via gix (Priority: P1)

A user analyzing a codebase wants the agent to query local git status, log, diff, and branches without shelling out to the `git` CLI, so that operations return structured, typed data instead of parsing text walls.

**Why this priority**: Local git operations are the foundation for any repository-aware agent. Replacing shell-based `git` commands with native gix calls eliminates text-parsing complexity, reduces token overhead, and provides structured error handling. This is the first milestone in the issue and enables subsequent GitHub remote operations.

**Independent Test**: Can be fully tested by invoking the `git` tool commands (status, log, diff, branches) on a temporary git repository fixture and verifying structured JSON output matches expected git state, without requiring the `git` CLI on PATH.

**Acceptance Scenarios**:

1. **Given** a temporary git repository with staged and unstaged changes, **When** the user invokes `git` with command `status` and a repository path, **Then** n00n returns structured working tree and index status as a Lua table with file paths, states (added, modified, deleted), and stage information.
2. **Given** a repository with commit history, **When** the user invokes `git` with command `log`, a path, and a count, **Then** n00n returns a list of commits with SHA, author, date, and message as structured data.
3. **Given** a repository with two branches, **When** the user invokes `git` with command `branches` and a path, **Then** n00n returns the list of branch names and their HEAD SHAs.
4. **Given** a repository with changes between two commits, **When** the user invokes `git` with command `diff`, a path, and two refs, **Then** n00n returns the diff as structured line additions and deletions.

---

### User Story 2 - GitHub remote API access (Priority: P1)

A user managing GitHub issues and pull requests wants the agent to create, list, and query GitHub objects via native API calls instead of shelling out to `gh`, so that operations use structured JSON, respect rate limits, and handle authentication securely.

**Why this priority**: GitHub remote operations are critical for agents that interact with repositories. Native API access provides typed responses, better error handling, and eliminates the dependency on the `gh` CLI. This is the second milestone in the issue and completes the core feature.

**Independent Test**: Can be fully tested by invoking the `github` tool commands (issues list, issue create, PR list) against a test repository using a test token, verifying structured JSON output matches GitHub API responses, without requiring the `gh` CLI.

**Acceptance Scenarios**:

1. **Given** a GitHub repository and a valid token, **When** the user invokes `github` with command `list_issues` and owner/repo, **Then** n00n returns a list of issues with numbers, titles, states, and authors as structured data.
2. **Given** a GitHub repository and a valid token, **When** the user invokes `github` with command `create_issue`, owner/repo, title, and body, **Then** n00n creates the issue and returns the created issue number and URL.
3. **Given** a GitHub repository and a valid token, **When** the user invokes `github` with command `list_prs` and owner/repo, **Then** n00n returns a list of pull requests with numbers, titles, states, and authors as structured data.
4. **Given** a GitHub repository and a valid token, **When** the user invokes `github` with command `get_repo` and owner/repo, **Then** n00n returns repository metadata (description, language, stars, forks) as structured data.

---

### User Story 3 - Git write operations with scoped permissions (Priority: P2)

A user wants the agent to perform git write operations (add, commit, checkout) behind scoped permissions, so that destructive operations are explicitly authorized and validated.

**Why this priority**: Write operations are powerful and potentially destructive. Implementing them with scoped permissions ensures safety and user control. This extends the MVP to full git manipulation capabilities.

**Independent Test**: Can be fully tested by invoking git write commands (add, commit, checkout) on a temporary repository with the `git.write` permission granted, verifying operations succeed and modify repository state correctly, and fail when permission is denied.

**Acceptance Scenarios**:

1. **Given** a temporary git repository with unstaged changes and the `git.write` permission, **When** the user invokes `git` with command `add`, a path, and file paths, **Then** n00n stages the files and returns success.
2. **Given** a repository with staged changes and the `git.write` permission, **When** the user invokes `git` with command `commit`, a path, and a message, **Then** n00n creates a commit and returns the new commit SHA.
3. **Given** a repository with multiple branches and the `git.write` permission, **When** the user invokes `git` with command `checkout`, a path, and a branch name, **Then** n00n switches to the branch and returns success.
4. **Given** a repository without the `git.write` permission, **When** the user invokes any git write command, **Then** n00n returns a permission denied error without modifying the repository.

---

### User Story 4 - GitHub write operations with scoped permissions (Priority: P2)

A user wants the agent to create GitHub pull requests and comments via native API calls behind scoped permissions, so that GitHub mutations are explicitly authorized and validated.

**Why this priority**: GitHub write operations (PR creation, commenting) are high-value agent actions. Implementing them with scoped permissions ensures safety and completes the GitHub tool surface.

**Independent Test**: Can be fully tested by invoking GitHub write commands (create_pr, add_comment) on a test repository with the `github.write` permission granted, verifying operations succeed and return correct responses, and fail when permission is denied.

**Acceptance Scenarios**:

1. **Given** a GitHub repository with a feature branch and the `github.write` permission, **When** the user invokes `github` with command `create_pr`, owner/repo, head, base, title, and body, **Then** n00n creates the PR and returns the PR number and URL.
2. **Given** a GitHub issue and the `github.write` permission, **When** the user invokes `github` with command `add_comment`, owner/repo, issue number, and comment body, **Then** n00n adds the comment and returns the comment ID.
3. **Given** a GitHub repository without the `github.write` permission, **When** the user invokes any GitHub write command, **Then** n00n returns a permission denied error without modifying GitHub state.

---

### Edge Cases

- What happens when the repository path does not exist or is not a git repository? The tool returns a clear error indicating the path is invalid or not a git repository.
- What happens when the GitHub token is missing or invalid? The tool returns a clear authentication error and suggests configuring `GITHUB_TOKEN` or n00n config.
- What happens when GitHub API rate limits are exceeded? The tool returns a rate limit error with retry-after information and logs the event.
- What happens when gix fails to open a repository due to corruption? The tool returns a structured error with the gix error message and suggests running `git fsck`.
- What happens when a git operation is interrupted (e.g., merge conflict)? The tool returns a structured error indicating the conflict state and suggests manual resolution.
- What happens when the user has both `gh` CLI token and `GITHUB_TOKEN` configured? The tool prefers `GITHUB_TOKEN` or n00n config, falling back to `gh auth token` only if explicitly configured.
- What happens when concurrent git operations target the same repository? The tool uses filesystem locks to serialize operations and prevent corruption.
- What happens when a git write operation is attempted on a bare repository? The tool returns a clear error indicating bare repositories do not support working tree operations.

## Requirements

### Functional Requirements

- **FR-001**: The `git` tool MUST be a built-in n00n tool that does not require the `git` CLI on PATH.
- **FR-002**: The `git` tool MUST support commands: `status`, `log`, `diff`, `branches`, `blame`, `add`, `commit`, `checkout`.
- **FR-003**: The `git` tool MUST use the `gix` crate for all git operations.
- **FR-004**: The `git` tool MUST return structured data (Lua tables) for all commands, not text output.
- **FR-005**: The `git` tool MUST respect the `git.read` permission for read operations (status, log, diff, branches, blame).
- **FR-006**: The `git` tool MUST respect the `git.write` permission for write operations (add, commit, checkout).
- **FR-007**: The `github` tool MUST be a built-in n00n tool that does not require the `gh` CLI on PATH.
- **FR-008**: The `github` tool MUST support commands: `list_issues`, `create_issue`, `get_issue`, `list_prs`, `create_pr`, `get_pr`, `add_comment`, `get_repo`.
- **FR-009**: The `github` tool MUST use the `reqwest` crate for HTTP requests to the GitHub REST API v3.
- **FR-010**: The `github` tool MUST return structured data (Lua tables) for all commands, not text output.
- **FR-011**: The `github` tool MUST respect the `github.read` permission for read operations (list_issues, get_issue, list_prs, get_pr, get_repo).
- **FR-012**: The `github` tool MUST respect the `github.write` permission for write operations (create_issue, create_pr, add_comment).
- **FR-013**: The `github` tool MUST read authentication from `GITHUB_TOKEN` environment variable or n00n config, never logging tokens.
- **FR-014**: The `github` tool MUST support optional fallback to `gh auth token` if configured.
- **FR-015**: The `github` tool MUST handle GitHub API rate limits and return retry-after information.
- **FR-016**: A new `n00n-git` crate MUST be created in the workspace, depending on `gix` with feature flags for status, diff, blame, reference, index, and commit.
- **FR-017**: The `n00n-lua/src/api/git.rs` module MUST expose the `n00n.git` Lua table with host functions for git operations.
- **FR-018**: The `plugins/git/init.lua` plugin MUST register the `git` tool with the agent.
- **FR-019**: The `n00n-lua/src/api/github.rs` module MUST expose the `n00n.github` Lua table with host functions for GitHub operations.
- **FR-020**: The `plugins/github/init.lua` plugin MUST register the `github` tool with the agent.
- **FR-021**: The `DEFAULT_BUILTINS` list in `n00n-config` MUST include `"git"` and `"github"`.
- **FR-022**: Permission scopes `git.read`, `git.write`, `github.read`, and `github.write` MUST be defined in `n00n-config`.
- **FR-023**: Tests MUST use temporary git repositories and MUST NOT require network access for git unit tests.
- **FR-024**: GitHub API tests MUST use a test repository and test token, and MUST be marked as integration tests.

### Key Entities

- **GitRepository**: A git repository opened via `gix::open(path)`, providing access to status, log, diff, branches, and write operations.
- **GitStatus**: Structured working tree and index status containing file paths, states (added, modified, deleted, untracked), and stage information.
- **GitCommit**: A commit object with SHA, author, date, message, and parent SHAs.
- **GitBranch**: A branch reference with name and HEAD SHA.
- **GitDiff**: Structured diff between two refs containing line additions and deletions per file.
- **GitHubClient**: An HTTP client using `reqwest` for GitHub REST API v3 requests, with authentication and rate limit handling.
- **GitHubIssue**: A GitHub issue with number, title, state, author, body, labels, and metadata.
- **GitHubPullRequest**: A GitHub pull request with number, title, state, author, head ref, base ref, and metadata.
- **GitHubComment**: A GitHub comment with ID, author, body, and creation timestamp.
- **GitHubRepository**: GitHub repository metadata with description, language, stars, forks, and owner information.

## Success Criteria

### Measurable Outcomes

- **SC-001**: The `git` tool returns correct structured results for status, log, diff, and branches commands on a fixture repository in 100% of unit test cases.
- **SC-002**: The `github` tool returns correct structured results for list_issues, get_issue, list_prs, and get_repo commands on a test repository in 100% of integration test cases.
- **SC-003**: Git unit tests pass without the `git` CLI installed on PATH.
- **SC-004**: GitHub integration tests pass without the `gh` CLI installed on PATH.
- **SC-005**: The `git` and `github` tools are listed in `DEFAULT_BUILTINS` and registered in the agent by default.
- **SC-006**: Permission scopes `git.read`, `git.write`, `github.read`, and `github.write` are defined and enforced correctly.
- **SC-007**: Tool call latency for git operations is no worse than the external `git` CLI baseline.
- **SC-008**: GitHub API requests handle rate limits correctly and return retry-after information.
- **SC-009**: The combined token size of the `git` and `github` tool definitions is no larger than the current baseline after removing external-installation notes.

## Assumptions

- The `gix` crate version 0.86+ provides stable APIs for status, diff, blame, reference, index, and commit operations.
- The GitHub REST API v3 is sufficient for the required operations (issues, PRs, comments, repo metadata); GraphQL is not needed for v1.
- The `reqwest` crate (already in workspace) is sufficient for GitHub HTTP requests; `octocrab` is not required initially.
- Users will configure `GITHUB_TOKEN` or n00n config for GitHub authentication; the tool will not bundle default credentials.
- Git write operations will be gated behind the `git.write` permission scope, which may require user prompts in the UI.
- GitHub write operations will be gated behind the `github.write` permission scope, which may require user prompts in the UI.
- The workspace `Cargo.toml` will manage `gix` as a workspace dependency with feature flags to avoid pulling the full dependency tree.
- The `n00n-git` crate will contain only git-specific logic; GitHub client logic will live in `n00n-lua/src/api/github.rs` or a separate crate if complexity grows.
- Tests will use `tempfile` for creating temporary git repositories to ensure isolation and cleanup.
- GitHub integration tests will use environment variables for test tokens and repository names to avoid hardcoding credentials.
