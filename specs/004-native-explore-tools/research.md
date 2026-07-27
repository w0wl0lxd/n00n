# Research: Native Explore Tools

**Feature**: Native Explore Tools  
**Date**: 2026-07-27

## Summary

The goal is to replace external CLI/MCP dependencies for `arbor`, `codegraph`, and `semblem` with in-process Rust code that is shipped as built-in n00n tools. The research evaluated upstream Rust libraries, the current n00n plugin and API patterns, and the existing index formats produced by each tool on the n00n repository.

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
- **Native options**:
  1. `cgz` / `codegraph` crate (f4ah6o/codegraph-rs) — a Rust port that exposes `CodeGraph::open`, `index_all`, `sync`, `search_nodes`, `get_callers`, `get_callees`, `get_impact_radius`, `build_context`.
  2. Custom `rusqlite` query layer over the SQLite schema.
- **Decision**: Start with `cgz`; fall back to a `rusqlite` query layer if `cgz` cannot read indexes produced by the installed `codegraph` CLI 1.4.1.
- **Rationale**: `cgz` implements the full query logic we need; a custom query layer would reimplement callers, callees, impact, and context building.
- **Alternatives rejected**:
  - Keep shelling out: not native.
  - Custom `rusqlite` first: higher effort, more error-prone; kept as fallback.

### Semble

- **Current state**: `semblem` is not a built-in n00n tool. It is provided as an MCP server (`mcp__semble__search`) or CLI (`semblem`).
- **Index format**: `semblem` builds a cached hybrid index of chunks, BM25 scores, and optional vector embeddings.
- **Native option**: `sonar-core` is a pure Rust translation of `semblem` with `SonarIndex::from_path_cached`, `search_with_options`, and BM25-only fallback.
- **Decision**: Adopt `sonar-core` with BM25-only by default and optional semantic mode when an embedding model is vendored locally.
- **Rationale**: Removes the Python runtime; `sonar-core` exposes a drop-in API. BM25-only avoids runtime model downloads.
- **Alternatives rejected**:
  - Shell out to `semblem` Python CLI: not native, requires Python runtime.
  - Port subset by hand: too much effort.

### n00n Integration Pattern

- The `Tool` trait in `n00n-agent/src/tools/registry.rs` supports native Rust tools, but all built-in tools today are registered through Lua plugins.
- `n00n-lua/src/api/mod.rs` exposes global tables (`n00n.arbor`, etc.) that Lua plugins consume.
- New tools should follow the same pattern: Rust core crate → `n00n-lua` API module → `plugins/<tool>/init.lua` → `BUNDLED_PLUGINS` / `DEFAULT_BUILTINS`.

## Open Questions / Risks

1. `cgz` maturity and compatibility with `codegraph` CLI 1.4.1 indexes needs a spike.
2. `arbor-graph` depends on `tiktoken-rs ^0.5` while n00n uses `0.9`; Cargo can resolve both, but `cargo deny` must pass.
3. `sonar-core` may attempt to download an embedding model at runtime; defaulting to BM25-only mitigates the supply-chain risk.
4. Indexing large repositories must run off the main async runtime to avoid blocking the agent loop.

## Research Artifacts

- `arbor` indexed n00n to `.arbor/` (graph.json ~10 MB).
- `codegraph` indexed n00n to `.codegraph/` (SQLite DB, 16,870 nodes, 56,586 edges).
- `semblem` indexed n00n and returned ranked chunk results.
- `codegraph` DB schema captured via `sqlite3 .codegraph/codegraph.db .schema`.
- `thoughtbox` reasoning session: `cd34a567-3929-4c32-99e7-4fbc29a5d25d`.
