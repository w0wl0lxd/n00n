# Native github tool using gix/gitoxide — Research

## Summary

GitHub issue #236 requests native git and GitHub tools backed by `gix/gitoxide` to replace shell-based `git` and `gh` commands with structured, typed access. The issue proposes a two-phase approach: local git operations via gix, then GitHub remote API access. The workspace already has `reqwest` available for HTTP, and follows a clear pattern for native crates (arbor, codegraph) with Lua plugin registration.

## Existing Patterns and Key Files

**Rust→Lua binding pattern:**
- `n00n-lua/src/api/mod.rs:45-96` — creates n00n global, adds API tables like `arbor`, `codegraph`
- `n00n-lua/src/api/arbor.rs:27-196` — `create_arbor_table()` uses `lua.create_function()` to wrap Rust calls, `value_or_err()` converts JSON to Lua tables via `json_to_lua()`
- `n00n-lua/src/api/codegraph.rs:8-135` — similar pattern with CLI fallback detection

**Lua plugin pattern:**
- `plugins/arbor/init.lua:406-501` — `n00n.api.register_tool()` with schema, description, header/restore/handler functions; dispatches commands to native API; uses `ExploreResult` for live UI updates
- `plugins/codegraph/init.lua:16-170` — similar pattern with timeout handling and output limits

**Configuration and registry:**
- `n00n-config/src/lib.rs:60-88` — `DEFAULT_BUILTINS` list of bundled tool names
- `n00n-agent/src/tools/registry.rs:66-79` — `ToolSource` enum (`Mcp { server }`, `Lua { plugin }`), `ToolRegistry` with lock-free `ArcSwap`

**Workspace crate pattern:**
- `n00n-arbor/Cargo.toml` — minimal dependencies: serde, serde_json, thiserror
- `n00n-codegraph/Cargo.toml` — minimal dependencies: thiserror, tracing, rusqlite, wait-timeout

## Recommended Architecture

**Separate `git` and `github` tools** (not combined). Rationale:
- Local git operations (status, log, diff) are distinct from GitHub remote API (issues, PRs, checks)
- Different permission scopes (`git.read`/`git.write` vs `github.read`/`github.write`)
- Users may want local git without GitHub auth, or vice versa
- Follows existing pattern: arbor and codegraph are separate tools despite both being code analysis

**Crate structure:**
- `n00n-git` crate in workspace, depending on `gix` (with feature flags for status, diff, blame, etc.)
- `n00n-lua/src/api/git.rs` exposing `n00n.git` table (status, log, diff, branches, blame, checkout, commit, add)
- `plugins/git/init.lua` registering `git` tool with commands
- `n00n-github` crate (optional, could live in `n00n-git` or separate) with `reqwest`-based REST client
- `n00n-lua/src/api/github.rs` exposing `n00n.github` table (issues, PRs, comments, checks, repo metadata)
- `plugins/github/init.lua` registering `github` tool

**Add to `DEFAULT_BUILTINS`:** `"git"`, `"github"`

## gix Capabilities Needed

Based on docs.rs for gix 0.86.0 and modular crates:
- **Repository access:** `gix::open(path)` → `Repository`
- **Status:** `gix-status` crate for working tree/index status
- **Log:** commit history via `Repository` methods
- **Diff:** `gix-diff` crate for tree/commit diffs
- **Branches:** branch listing and switching via `gix-reference` module
- **Blame:** `gix-blame` crate for line annotations
- **Checkout:** worktree checkout operations
- **Commit:** staging and committing via `gix-index` and `gix-commit`
- **Add:** staging files to index

Use feature flags to avoid pulling full gix dependency tree: enable only `gix`, `gix-status`, `gix-diff`, `gix-blame`, `gix-reference`, `gix-index`, `gix-commit` as needed.

## GitHub Remote API Approach

**REST over GraphQL:**
- Use GitHub REST API v3 for simplicity; only add GraphQL if complex queries are needed later
- REST is sufficient for issues, PRs, comments, checks, repo metadata

**Authentication:**
- Read token from `GITHUB_TOKEN` env var or n00n config (never log tokens)
- Support `gh` CLI token detection as fallback (read from `gh auth token`)
- Prefer `reqwest` (already in workspace with rustls-tls, http2, stream) over octocrab to minimize dependencies

**Crates:**
- Primary: `reqwest` (workspace dependency, no new crate)
- Optional: `octocrab` if high-level API abstraction is worth the dependency
- Recommendation: start with small `reqwest` client, add octocrab only if complexity grows

**Endpoints:**
- Issues: `/repos/{owner}/{repo}/issues`
- PRs: `/repos/{owner}/{repo}/pulls`
- Comments: `/repos/{owner}/{repo}/issues/{issue_number}/comments`
- Checks: `/repos/{owner}/{repo}/commits/{sha}/check-runs`
- Repo metadata: `/repos/{owner}/{repo}`

## Top 3 User Stories

**P1 (MVP):**
- Agent can query local git status and log without shelling out to `git`, getting structured output instead of parsing text walls.

**P2:**
- Agent can create GitHub issues and PRs with structured inputs (title, body, labels) via native API, with token auth from config/env.

**P3:**
- Agent can perform git operations (add, commit, checkout) behind scoped permissions, with validation and structured error messages.

## Risks and Open Questions

**Risks:**
- gix feature flag complexity: enabling too many sub-crates may bloat dependencies; need careful feature selection
- GitHub API rate limits: need retry logic and rate limit handling
- Token management: secure storage and retrieval without logging

**Open questions:**
- Should write operations (commit, checkout, PR creation) require explicit user confirmation beyond permission scopes?
- Should the GitHub client support both personal access tokens and OAuth app tokens?
- Should git write operations be gated behind a separate `git.write` scope that forces prompts?
- Should `n00n-git` include the GitHub client, or should it be a separate `n00n-github` crate? (Recommendation: separate for clarity)
