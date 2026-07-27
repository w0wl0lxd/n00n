# Implementation Plan: Native Explore Tools

**Branch**: `004-native-explore-tools` | **Date**: 2026-07-27 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/004-native-explore-tools/spec.md`

---

## Summary

This feature ports the `arbor`, `codegraph`, and `semblem` code-intelligence tools from external CLI/MCP dependencies to in-process Rust libraries exposed through n00n's existing Lua plugin layer. It introduces a new reusable `n00n-search` crate for indexing, BM25, and embedder orchestration; uses `rusqlite` for CodeGraph's SQLite index; keeps Arbor on `arbor-core`/`arbor-graph`; and defaults `semblem` to BM25-only, nagging the user to configure a local or remote embedder when semantic search is requested.

---

## Technical Context

**Language/Version**: Rust 2024 edition (workspace `rust-version = 1.97`).

**Primary Dependencies**:
- `arbor-core` + `arbor-graph` (Arbor parsing and graph queries).
- `rusqlite` (bundled, `modern_sqlite` for WAL/FTS5) for CodeGraph index access.
- `tantivy` (BM25 full-text search) inside `n00n-search`.
- `ignore` + workspace `tree-sitter-*` grammars for file walking and chunking in `n00n-search`.
- `isahc` or workspace HTTP client for remote embedders.
- `model2vec-rs` (optional, behind a feature) for a small static local embedder.

**Storage**: Existing project-side indexes remain under `.arbor/`, `.codegraph/`, and a new `.n00n/search/` cache. No new persistent storage in n00n itself.

**Testing**: `cargo test -p n00n-lua`, `cargo test -p n00n-agent`, `cargo clippy --all --tests -- -D warnings`, `cargo deny check`.

**Target Platform**: Linux primary; macOS secondary. All chosen crates are pure Rust or use widely supported native parsers.

**Project Type**: CLI/TUI agent with built-in Lua plugins.

**Performance Goals**:
- `arbor` and `codegraph` query latency on the n00n repo must not exceed the external-CLI baseline.
- `semblem` BM25 query latency must be under 1 second for the n00n repo on a warm index.

**Constraints**:
- `unsafe_code = "deny"` workspace-wide; no new `unsafe` blocks without review.
- `unwrap_used` and `expect_used` denied in production code.
- New dependencies must pass `cargo deny check` and be added to workspace `Cargo.toml` first.
- Tool definitions must shrink or stay flat in token size after removing CLI-installation notes.
- No bundled API keys, cloud providers, or embedding models. Embedder config is explicit and user-supplied.

**Scale/Scope**: Four new/rewritten crates (`n00n-arbor` rewrite, `n00n-codegraph`, `n00n-search`, `n00n-semble`), four Lua API modules, three Lua plugins, and feature flags to gate heavy deps.

---

## Constitution Check

*The project constitution is defined in `AGENTS.md`. The following gates apply before implementation:*

| Gate | Status | Notes |
|------|--------|-------|
| No new `unsafe` without review | Pass | The chosen crates do not require new `unsafe` blocks in n00n wrapper code. |
| `cargo clippy --all --tests -- -D warnings` | TBD | Must pass before PR. |
| `cargo deny check` | TBD | Must pass; dependency licenses are MIT/Apache-2.0. |
| No silent `.ok()` / default fallbacks | Pass | Errors from upstream crates will be mapped to typed `thiserror` variants. |
| TDD / failing test first | Pass | Each phase starts with a failing test or fixture assertion. |
| DRY/SRP | Pass | Each crate has one responsibility: parsing/indexing, query API, or Lua binding. |
| No bundled credentials or cloud providers | Pass | Embedder config is explicit; defaults to BM25. |

---

## Project Structure

### Documentation (this feature)

```text
specs/004-native-explore-tools/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── spec.md              # User-facing specification
├── data-model.md        # Entity and schema contracts
├── quickstart.md        # Validation guide
├── contracts/           # Tool schemas and API contracts
│   ├── arbor.md
│   ├── codegraph.md
│   ├── semble.md
│   └── search.md
└── checklists/
    └── requirements.md
```

### Source Code (repository root)

```text
n00n-arbor/              # Rewritten: in-process Arbor graph client
n00n-codegraph/          # New: in-process CodeGraph SQLite client (rusqlite)
n00n-search/             # New: reusable indexing, BM25, embedder orchestration
n00n-semble/             # New: semblem tool logic + Lua binding
n00n-lua/src/api/        # Add/Update arbor.rs, codegraph.rs, semble.rs
plugins/                 # Update arbor, codegraph; add semble
n00n-agent/src/prompt.rs # Update NATIVE_EFFICIENT_TOOLS list
n00n-config/src/lib.rs  # Update DEFAULT_BUILTINS list
n00n-lua/src/loader.rs  # Update BUNDLED_PLUGINS list
Cargo.toml              # Workspace dependencies and members
```

**Structure Decision**: One Rust crate per responsibility. `n00n-search` is shared by `semblem` and future search features. Tool-specific crates (`n00n-arbor`, `n00n-codegraph`, `n00n-semble`) handle schema formatting and Lua bindings. Lua plugins remain the tool surface.

---

## Complexity Tracking

No constitution violations expected. Binary size is managed via Cargo feature flags:
- `arbor` — enables `n00n-arbor` and Arbor plugin.
- `codegraph` — enables `n00n-codegraph` and CodeGraph plugin.
- `semblem` — enables `n00n-semble`/`n00n-search` and Semble plugin.
- `vllm` (optional) — pulls HTTP client and vLLM preset helpers into `n00n-search`.
- `static-embed` (optional) — pulls `model2vec-rs` for a local static embedder.

If binary bloat is measurable, features let users disable individual tools.

---

## Crate Responsibilities

### `n00n-search`

Reusable code-search core. It is **not** a tool; it is a library used by `n00n-semble` and future search features.

- `walk`: `.gitignore`-aware directory walker.
- `chunk`: source chunking by top-level definitions (tree-sitter) with line/paragraph fallback.
- `index`: `SearchIndex` combining a `tantivy` BM25 index and an optional dense-vector index.
- `embed`: `Embedder` trait + implementations:
  - `None` (BM25-only).
  - `Static` (`model2vec-rs`, feature-gated).
  - `Vllm` (OpenAI-compatible `/v1/embeddings` against a local vLLM container).
  - `Remote` (OpenAI-compatible endpoint with user-supplied URL and key).
- `vllm`: `VllmPreset` definitions and Podman command generators for light/medium/heavy models.
- `rank`: RRF fusion of BM25 and vector rankings.

### `n00n-semble`

Tool-specific crate for `semblem`.

- Loads/creates `.n00n/search/` index for a repo via `n00n-search`.
- Parses `semblem` arguments, calls `n00n-search` search/hybrid, formats `ToolResult`.
- Implements `find_related` by embedding the target chunk and performing vector similarity.
- Handles the "nag" UX: when semantic mode is requested with no embedder, return a message that lists the vLLM Podman presets and a remote option, then fall back to BM25.
- Exposes `n00n.semblem` Lua table.

### `n00n-codegraph`

Tool-specific crate for `codegraph`.

- Opens `.codegraph/codegraph.db` with `rusqlite` (bundled).
- Implements `explore`, `callers`, `callees`, `impact`, `status`, `ensure_indexed`.
- Uses FTS5 for natural-language node search, edge tables for call graphs, and custom SQL for blast radius.
- Triggers index build (or re-index if stale) when called and shows progress through `n00n.ui` status card.
- Exposes `n00n.codegraph` Lua table.

### `n00n-arbor`

Rewritten in-process Arbor client.

- Loads `.arbor/graph.bin` or builds from source via `arbor-core`/`arbor-graph`.
- Implements `callers`, `callees`, `map`, `diff`, `query`, `status`.
- Shows progress during indexing.
- Exposes `n00n.arbor` Lua table.

---

## Phased Roadmap

### Phase 0: Validation Spikes

1. Add `arbor-core`/`arbor-graph` to a throwaway branch and prove `GraphStore::load_graph` reads the `.arbor/` index built by the CLI.
2. Add `rusqlite` with `bundled` + `modern_sqlite` and prove read access to `.codegraph/codegraph.db` schema and FTS5 queries.
3. Add `tantivy` to a throwaway crate, index the n00n repo, and prove BM25 sub-second queries.

### Phase 1: Crate Scaffolding

1. Create `n00n-search` with `Cargo.toml`, feature flags, and empty module skeleton.
2. Create `n00n-codegraph` and `n00n-semble` with `Cargo.toml` and module skeleton.
3. Update root `Cargo.toml` workspace `members` and `workspace.dependencies`.
4. Wire `n00n-lua` API modules and `BUNDLED_PLUGINS` (initially as no-ops).
5. Run `cargo check --workspace` and `cargo deny check`.

### Phase 2: `n00n-search` Core

1. Implement `.gitignore`-aware file walker.
2. Implement tree-sitter chunker (reuse workspace grammars) with line fallback.
3. Implement `tantivy` BM25 index builder and searcher.
4. Implement `Embedder` trait and `None`/`Remote`/`Vllm` backends; `Static` behind `static-embed` feature.
5. Implement `VllmPreset` generator for light/medium/heavy Snowflake Arctic Embed models.
6. Add tests with fixture repo.

### Phase 3: Arbor Native

1. Rewrite `n00n-arbor` to use `arbor-graph` + `arbor-core`.
2. Update `n00n-lua/src/api/arbor.rs` and `plugins/arbor/init.lua`.
3. Add progress reporting during indexing.
4. Add fixture tests for `callers`, `callees`, `map`, `diff`, `query`, `status`.

### Phase 4: Semble Native

1. Implement `n00n-semble` using `n00n-search`.
2. Add `n00n-lua/src/api/semblem.rs` and `plugins/semblem/init.lua`.
3. Add `semblem` to `BUNDLED_PLUGINS`, `DEFAULT_BUILTINS`, and `NATIVE_EFFICIENT_TOOLS`.
4. Implement default BM25 mode and embedder-nag UX.
5. Add tests for `search` and `find_related`.

### Phase 5: CodeGraph Native

1. Implement `n00n-codegraph` using `rusqlite`.
2. Add `n00n-lua/src/api/codegraph.rs` and rewrite `plugins/codegraph/init.lua`.
3. Implement `explore`, `callers`, `callees`, `impact`, `status`, `ensure_indexed`.
4. Add progress reporting during indexing.
5. Validate output matches the existing `codegraph explore` format.

### Phase 6: Integration & Rollout

1. Update tool descriptions to remove external CLI installation notes.
2. Add feature flags for `arbor`/`codegraph`/`semblem`/`vllm`/`static-embed`.
3. Run `cargo nextest run --workspace`, `cargo clippy --all --tests -- -D warnings`, `cargo deny check`.
4. Measure tool-definition token size and tool call latency against baseline.
5. Draft PR with performance comparison.

---

## Quick Validation Guide

See [quickstart.md](quickstart.md) for end-to-end validation steps.
