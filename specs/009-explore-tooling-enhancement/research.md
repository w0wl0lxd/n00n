# Research: Explore Tooling Enhancement

**Feature**: Explore Tooling Enhancement  
**Date**: 2026-08-01

## CodeGraph 1.5.0 Upgrade Decision

### Current State

- Installed CLI: 1.4.1
- Latest upstream: 1.5.0 (released 2026-07-21)
- Current `n00n-codegraph` only exposes `explore` command
- Native SQLite queries via `db.rs` for `.codegraph/codegraph.db`

### 1.5.0 Features

- Rust engine (near-instant sync)
- Nix/ArkTS support
- Context hook for custom indexing
- New commands: `callers`, `callees`, `impact`, `affected`, `node`, `query`, `sync`, `files`

### Upgrade Path

- No Rust dependency changes required (CLI is external)
- SQLite schema is backward-compatible
- Native queries can be extended for new commands
- CLI fallback for commands not yet supported natively

### Rationale

Upgrading to 1.5.0 provides access to additional commands that make CodeGraph a more complete structural exploration tool. The Rust engine improves sync performance. Native SQLite queries reduce latency and CLI dependency. The upgrade is low-risk since the CLI is external and the schema is compatible.

### External Sources

- CodeGraph CHANGELOG: <https://github.com/colbymchenry/codegraph>
- Release notes for 1.5.0: Rust engine migration, Nix/ArkTS support, context hook

## Arbor 2.5.0 Expansion

### Current State

- Installed CLI: 2.5.0 (already current)
- Current `n00n-arbor` exposes: `callers`, `callees`, `map`, `diff`, `query`, `status`
- Native in-memory `graph_json.rs` and `graph_query.rs` for `callers`, `callees`, `trace_path`
- Missing upstream commands: `entry-points`, `file-graph`, `inspect`, `path`, `refactor`, `check`, `summary`, `audit`

### 2.5.0 Features

- All missing commands are available in the CLI
- Output format is stable JSON
- Native graph.json is still supported for in-memory queries

### Expansion Path

- Add CLI wrappers for missing commands
- Keep native in-memory fallback for callers/callees/trace_path
- Improve GraphIndex for better offline support

### Rationale

Arbor 2.5.0 is already current, so exposing additional commands is low-risk. The CLI wrappers provide access to advanced workflows (entry-points, file-graph, refactor) that are useful for project orientation and refactoring. Native in-memory fallbacks ensure basic functionality works even without the CLI.

### External Sources

- Arbor releases: <https://github.com/Anandb71/arbor/releases>
- Arbor documentation: <https://arbor-cli.org>

## Semblem Hybrid Decision

### Current State

- `n00n-semble` wraps native `n00n-search` Tantivy index under `.n00n/search/`
- Supports `bm25` only
- `hybrid` and `semantic` return `Error::NotSupported`
- Plugin nags with fallback when semantic mode requested

### Upstream Semble CLI v0.5.1

- Supports `--content docs/config/all` for content filtering
- Supports remote git URLs
- Supports `find-related` for similarity search
- Supports `savings` for token savings analysis
- BM25 + semantic hybrid search with embedders

### Hybrid Path

- Wrap upstream `semble` CLI as default engine
- Keep native `n00n-search` BM25 as fallback for offline/no-embedder cases
- Add CLI detection and availability check
- Support `--content` flags and remote URLs

### Rationale

The upstream CLI provides features not available in the native wrapper (remote URLs, content filters, savings). Wrapping it while keeping BM25 fallback provides the best of both worlds: advanced semantic search when available, offline keyword search when not. This aligns with the user's hybrid path decision.

### External Sources

- Semble repository: <https://github.com/MinishLab/semble>
- Semble documentation: <https://semble.dev>

## RTK Bash-Only Scope

### Current State

- `plugins/bash/init.lua` already rewrites commands via `rtk rewrite`
- 2s timeout for availability check
- Respects `no_rtk` from CLI/config
- `n00n-ui` shell does not use rtk

### Hardening Opportunities

- Cache rtk availability per session (currently checks on every call)
- Broaden rewrite coverage (add more command patterns)
- Ensure `jq`/`yq` pass through unchanged (already implemented)
- Update prompt hints to explicitly recommend rtk-wrapped bash

### Scope Decision

- Keep rtk in `bash` plugin only
- Do NOT apply rtk to `n00n-ui` shell
- Do NOT apply rtk to `code_execution` by default

### Rationale

The user chose bash-only scope for RTK. Hardening the existing integration with session caching and broader coverage improves token efficiency without expanding scope. The `n00n-ui` shell and `code_execution` are out of scope per user decision.

## Router Design

### Current State

- `plugins/explore/router.lua` routes to `index` (file), `arbor` (relations), `codegraph` (cross_file)
- Intent detection is regex-only
- Does NOT route to `semblem`, `arbor map` for open-ended skeleton, or `codegraph node` for symbol drill-down

### Enhancement Opportunities

- Add intents: `search`, `skeleton`, `symbol`, `impact`, `trace`
- Route to `semblem` for keyword/natural-language search
- Route to `arbor map` for project orientation
- Route to `codegraph node` for symbol drill-down
- Improve regex patterns for better intent detection

### Rationale

A smarter router reduces the cognitive load on the agent and users. Adding more intents and backends makes explore a more comprehensive entry point for codebase questions. The regex-based approach is simple and effective for the majority of queries; false positives can be corrected with explicit `intent` parameter.

## Tool Output Line Budgets

### Current State

- `arbor`, `codegraph`, and `explore` share one budget in `n00n-config`
- No per-tool budgets
- `output_limits` module provides shared limits

### Consideration

- Consider adding per-tool budgets for finer control
- Arbor map may need higher budget for project orientation
- CodeGraph explore may need higher budget for cross-file structure
- Semblem search may need lower budget for snippet results

### Rationale

Per-tool budgets would allow finer control over output size for different tools. However, the current shared budget works well in practice. Adding per-tool budgets is optional and can be done in Phase 6 if needed based on testing.

## External Tool Availability

### Detection Strategy

- CodeGraph: `Client::check_binary()` checks for `codegraph` on PATH
- Arbor: `Client::check_binary()` checks for `arbor` on PATH
- Semble: Add check for `semble` on PATH
- RTK: Already checks for `rtk` on PATH with timeout

### Fallback Strategy

- CodeGraph: Native SQLite queries when CLI unavailable
- Arbor: Native in-memory graph.json for callers/callees/trace_path when CLI unavailable
- Semble: Native BM25 when CLI unavailable
- RTK: Run commands normally when unavailable

### Rationale

Graceful fallback ensures tools work even when external CLIs are not installed. Native implementations provide offline capability and reduce dependency on external tools. Detection checks are fast and cached where possible.
