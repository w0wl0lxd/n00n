# Research: Native Explore Tools

**Feature**: Native Explore Tools  
**Date**: 2026-07-27

## Summary

The goal is to replace external CLI/MCP dependencies for `arbor`, `codegraph`, and `semblem` with in-process Rust code that is shipped as built-in n00n tools. This research evaluated upstream Rust libraries, the current n00n plugin and API patterns, and the existing index formats produced by each tool on the n00n repository. The final design introduces a reusable `n00n-search` core for indexing, BM25, and embedder orchestration; `rusqlite` for CodeGraph; and `arbor-core`/`arbor-graph` for Arbor.

## Findings by Tool

### Arbor

- **Current state**: `plugins/arbor/init.lua` + `n00n-lua/src/api/arbor.rs` + `n00n-arbor` crate. The Rust `Client` shells out to the `arbor` binary for every command and parses JSON.
- **Index format**: `.arbor/` contains `graph.bin` (Sled/graph store) and `graph.json` (serialized `ArborGraph`).
- **Native option**: `arbor-core` and `arbor-graph` are official MIT crates. They expose `CodeNode`, `GraphBuilder`, `GraphStore`, `ArborGraph`, and methods for `get_callers`, `get_callees`, `analyze_impact`, `slice_context`, and `compute_centrality`.
- **Decision**: Adopt `arbor-core` + `arbor-graph` and rewrite `n00n-arbor` to load/build the graph in-process.
- **Rationale**: Official, stable API that mirrors the CLI's own data model; `GraphStore` supports incremental updates.
- **Alternatives rejected**:
  - Parse `graph.json` only: too large and stale.
  - Keep shelling out: defeats the native goal.

### CodeGraph

- **Current state**: `plugins/codegraph/init.lua` shells out to `codegraph explore`.
- **Index format**: `.codegraph/` contains a SQLite database with `nodes`, `edges`, `files`, `unresolved_refs`, and an FTS5 virtual table `nodes_fts`.
- **Native options evaluated**:
  1. `cgz` / `codegraph` crate — a Rust port. Last update is older than `rusqlite`, and the schema produced by the installed `codegraph` CLI 1.4.1 may drift.
  2. Custom `rusqlite` query layer over the SQLite schema — direct, synchronous, and keeps us in control of the query logic and schema evolution.
  3. `sqlx` — modern but requires a DB at build time for compile-time checks; rejected because the `.codegraph` index is project-local and not available at crate build time.
- **Decision**: Implement `n00n-codegraph` with `rusqlite` (bundled, `modern_sqlite` for WAL/FTS5).
- **Rationale**: `rusqlite` is well-maintained, lets us query the existing CLI-produced index, and avoids a build-time DB dependency. WAL + prepared statements + PRAGMA tuning give acceptable performance for interactive use.
- **Alternatives rejected**:
  - `cgz`: schema-drift risk and stale release cadence.
  - `sqlx`: build-time DB requirement is impractical for a tool that opens project-local SQLite files.

### Semble / Search Core

- **Current state**: `semblem` is not a built-in n00n tool. It is provided as an MCP server (`mcp__semble__search`) or CLI (`semblem`).
- **Index format**: `semblem` builds a cached hybrid index of chunks, BM25 scores, and optional vector embeddings.
- **Native option**: `sonar-core` is a pure Rust translation of `semblem` with `SonarIndex::from_path_cached` and `search_with_options`. It bundles `model2vec-rs` and auto-downloads `minishlab/potion-code-16M` from HuggingFace on first use.
- **Decision**: Build a new `n00n-search` crate from first principles using `tantivy` for BM25 and a pluggable `Embedder` trait. `n00n-semble` will be a thin wrapper around `n00n-search`.
- **Rationale**:
  - `n00n-search` can be reused by future search features, not just `semblem`.
  - `tantivy` is a mature, zero-model BM25 engine, satisfying the BM25-only default without downloads.
  - A pluggable embedder trait lets us support local vLLM, remote OpenAI-compatible endpoints, and an optional static model behind a feature — without bundling any one model or provider.
  - Avoiding `sonar-core` avoids a hard dependency on a single static model and its HuggingFace download at first use.
- **Alternatives rejected**:
  - `sonar-core` as-is: ties us to a single default model and does not support vLLM/remote embedders.
  - Wrapping `sonar-core` and adding embedders: still coupled to `sonar-core`'s index format and chunking, duplicating work `n00n-search` will need for other tools.

### vLLM Embedding Serving

- vLLM supports pooling/embedding models via `vllm serve <model> --task embed`.
- vLLM `BertModel` and `XLMRobertaModel` architectures support the Snowflake Arctic Embed family.
- Snowflake Arctic Embed sizes:
  - `snowflake-arctic-embed-xs`: 22M params, 384 dim, fast, low VRAM.
  - `snowflake-arctic-embed-m-v1.5`: 110M params, 768 dim, balanced.
  - `snowflake-arctic-embed-l-v1.5`: 335M params, 1024 dim, highest quality.
- These map to the three memory/performance presets requested.

### n00n Integration Pattern

- The `Tool` trait in `n00n-agent/src/tools/registry.rs` supports native Rust tools, but all built-in tools today are registered through Lua plugins.
- `n00n-lua/src/api/mod.rs` exposes global tables (`n00n.arbor`, etc.) that Lua plugins consume.
- New tools should follow the same pattern: Rust core crate → `n00n-lua` API module → `plugins/<tool>/init.lua` → `BUNDLED_PLUGINS` / `DEFAULT_BUILTINS`.
- `n00n.ui` can render live status cards for moving progress indicators.

## Open Questions / Risks

1. `arbor-graph` depends on `tiktoken-rs ^0.5` while n00n uses `0.9`; Cargo can resolve both, but `cargo deny` must pass.
2. `tantivy` is a large dependency; binary bloat must be measured and feature flags added if needed.
3. Tree-sitter chunking needs to reuse workspace grammars and avoid duplicate parser deps.
4. vLLM Podman presets assume a GPU; CPU fallback is limited and should be documented.
5. Auto-indexing on first call must not block the `smol` runtime; indexing should run on a dedicated thread pool or as a spawned process with progress streamed back.

## Research Artifacts

- `arbor` indexed n00n to `.arbor/` (graph.json ~10 MB).
- `codegraph` indexed n00n to `.codegraph/` (SQLite DB, 16,870 nodes, 56,586 edges).
- `semblem` indexed n00n and returned ranked chunk results.
- `codegraph` DB schema captured via `sqlite3 .codegraph/codegraph.db .schema`.
- vLLM embedding docs and Snowflake Arctic Embed specs captured via `exa`.
