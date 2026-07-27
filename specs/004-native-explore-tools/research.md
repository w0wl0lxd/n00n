# Research: Native Explore Tools

**Feature**: Native Explore Tools  
**Date**: 2026-07-27

## Summary

The goal is to replace external CLI/MCP dependencies for `arbor`, `codegraph`, and `semblem` with in-process Rust code that is shipped as built-in n00n tools. This research evaluated upstream Rust libraries, the current n00n plugin and API patterns, and the existing index formats produced by each tool on the n00n repository. The final design introduces a reusable `n00n-search` core for indexing, BM25, and embedder orchestration; `rusqlite` for CodeGraph; and `arbor-core`/`arbor-graph` for Arbor.

## Findings by Tool

### Arbor

- **Current state**: `plugins/arbor/init.lua` + `n00n-lua/src/api/arbor.rs` + `n00n-arbor` crate. The Rust `Client` shells out to the `arbor` binary for every command and parses JSON.
- **Index format**: `.arbor/` contains `cache/` (Sled/graph store) and `blobs/`. The actual graph data is in `.arbor/cache/`.
- **Native option**: `arbor-core` 2.5.0 and `arbor-graph` 2.5.0 are published under MIT on crates.io. `arbor-graph` exposes `GraphStore::open(path)` and `GraphStore::load_graph()` to read the existing `.arbor/cache/` store, plus `ArborGraph` query methods and `GraphBuilder` for incremental builds.
- **Decision**: Adopt `arbor-core` 2.5.0 + `arbor-graph` 2.5.0 and rewrite `n00n-arbor` to load/build the graph in-process.
- **Rationale**: Official, stable API that mirrors the CLI's own data model; `GraphStore` supports incremental updates and can read the CLI-produced cache.
- **Alternatives rejected**:
  - Parse `graph.json` only: too large and stale.
  - Keep shelling out: defeats the native goal.

**Compatibility notes**:
- `arbor-graph` 2.5.0 depends on `tiktoken-rs ^0.5` while n00n already depends on `tiktoken-rs 0.9`. Cargo will build both versions; `deny.toml` sets `multiple-versions = "warn"`, so this produces a warning rather than a hard failure. Phase 0 spike confirmed `cargo deny check` passes with warnings.
- `arbor-core` 2.5.0 depends on `tree-sitter ^0.22`; n00n uses `tree-sitter 0.26`. These cannot coexist in the same workspace due to `links = "tree-sitter"` conflicts. The spike was created as an independent workspace to avoid this conflict. For n00n integration, `n00n-arbor` must be isolated as a separate workspace or use a workspace inheritance strategy that avoids the conflict.
- **GraphStore path semantics**: `GraphStore::open()` expects the path to the `.arbor/cache/` directory, not `.arbor/`. Opening `.arbor/` succeeds but returns an empty graph (0 nodes, 0 edges). Opening `.arbor/cache/` correctly loads the graph (10482 nodes, 6089 edges in the spike test).
- **Node count discrepancy**: The spike loaded 10482 nodes from `.arbor/cache/`, while `arbor status` reported 13659 nodes. This is because the CLI re-indexed during the spike run (see stderr: "Indexed 370 files, 0 cache hits (13659 nodes)"). The cached graph was from an earlier index run. This is expected behavior and not a compatibility issue.
- **cargo deny results**: Phase 0 spike ran `cargo deny check` on the independent workspace. Results:
  - **Advisories**: 3 unmaintained crates (bincode 1.3.3, fxhash 0.2.1, instant 0.1.13) - all transitive dependencies of arbor-graph's sled dependency. No safe upgrade available per advisories.
  - **Licenses**: Spike crate lacked license field (expected for throwaway). Arbor dependencies are MIT/Apache-2.0.
  - **Duplicates**: syn 2.0.119 and 3.0.3 coexist (expected for serde derive vs tracing).
  - **Bans/Sources**: Passed.
  - **Verdict**: The unmaintained advisories are in arbor-graph's dependency tree (sled) and have no safe upgrades. This is a supply-chain risk inherited from arbor-graph 2.5.0.

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

| Preset | Model | `--max-model-len` | `--max-num-seqs` | `--gpu-memory-utilization` | Notes |
|--------|-------|-------------------|------------------|----------------------------|-------|
| Light | `Snowflake/snowflake-arctic-embed-xs` | 512 | 32 | 0.4 | ~0.5-1 GB VRAM, fastest startup. |
| Medium | `Snowflake/snowflake-arctic-embed-m-v1.5` | 512 | 64 | 0.6 | ~2-3 GB VRAM, balanced throughput. |
| Heavy | `Snowflake/snowflake-arctic-embed-l-v1.5` | 512 | 128 | 0.8 | ~4-6 GB VRAM, best retrieval quality. |

Generated Podman command shape (Light example):

```bash
podman run --rm -it --gpus all \
  -p 8000:8000 \
  -v "$HOME/.cache/huggingface:/root/.cache/huggingface" \
  vllm/vllm-openai:latest \
  --model Snowflake/snowflake-arctic-embed-xs \
  --task embed \
  --max-model-len 512 \
  --max-num-seqs 32 \
  --gpu-memory-utilization 0.4
```

CPU-only fallback is limited; the preset generator will include `--device cpu` documentation but warn that GPU is strongly recommended for vLLM embedding throughput.

### Embedder License / Supply Chain

- `model2vec-rs` 0.2.1 is MIT licensed (source `LICENSE` on docs.rs and GitHub badge confirm MIT). No supply-chain concern.
- Snowflake Arctic Embed models are Apache-2.0 licensed on HuggingFace.
- `rusqlite`, `tantivy`, `arbor-core`, `arbor-graph` are MIT licensed.

### n00n Integration Pattern

- The `Tool` trait in `n00n-agent/src/tools/registry.rs` supports native Rust tools, but all built-in tools today are registered through Lua plugins.
- `n00n-lua/src/api/mod.rs` exposes global tables (`n00n.arbor`, etc.) that Lua plugins consume.
- New tools should follow the same pattern: Rust core crate → `n00n-lua` API module → `plugins/<tool>/init.lua` → `BUNDLED_PLUGINS` / `DEFAULT_BUILTINS`.
- `n00n.ui` can render live status cards for moving progress indicators.

### Auto-indexing and Progress Indicators

- `ExploreResult.live(ctx)` creates a live UI buffer and registers it via `ctx:live_buf(card.buf)`.
- `Card:update(output)` replaces text in the buffer.
- Long-running indexing must run off the `smol` async runtime to avoid blocking the agent loop. Options:
  1. Spawn a dedicated `std::thread` and stream progress via a bounded channel (`flume`/`crossbeam`) to the Lua/Rust boundary.
  2. Use `smol::unblock` for synchronous indexing calls and poll a progress receiver in the Lua plugin coroutine.
- The chosen approach: `n00n-search`/`n00n-codegraph`/`n00n-arbor` expose an indexing function that accepts a `Fn(IndexProgress)` callback. The Rust Lua binding calls this on a thread and uses a channel to forward `card:update()` calls into the Lua thread. This matches the existing `ExploreResult`/`Card` pattern.

### Concurrent Indexing

- To prevent two simultaneous index builds from corrupting the same project index, use a project-scoped `fs2::FileLock` (or `file-lock` crate) on a sentinel file (e.g., `.n00n/search/.lock`, `.codegraph/.lock`, `.arbor/.lock`).
- If the lock is already held, return a progress message telling the user an index is in progress; do not block the agent loop waiting for the lock.

## Open Questions / Risks

1. `arbor-graph` 2.1.0 `GraphStore` can read `.arbor/` stores in principle, but a Phase 0 spike must verify compatibility with the index produced by the installed `arbor` CLI.
2. `tiktoken-rs` duplicate-version warning is expected; `cargo deny check` must still be confirmed to pass.
3. `tree-sitter` 0.22 vs 0.26 duplicate versions are expected; must verify no build/link issues.
4. vLLM Podman presets assume a GPU; CPU fallback is limited and must be documented.
5. `tantivy` is a large dependency; binary bloat must be measured and feature flags added if needed.
6. Tree-sitter chunking in `n00n-search` needs to reuse workspace grammars and avoid duplicate parser deps.
7. Fixture repositories for integration tests must be selected (n00n self-index and a small multi-language repo).

## Research Artifacts

- `arbor` indexed n00n to `.arbor/` (graph.json ~10 MB).
- `codegraph` indexed n00n to `.codegraph/` (SQLite DB, 16,870 nodes, 56,586 edges).
- `semblem` indexed n00n and returned ranked chunk results.
- `codegraph` DB schema captured via `sqlite3 .codegraph/codegraph.db .schema`.
- vLLM embedding docs and Snowflake Arctic Embed specs captured via `exa`.
- `arbor-graph` 2.1.0 API verified on docs.rs (`GraphStore::open`, `GraphStore::load_graph`).
