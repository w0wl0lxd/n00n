# Implementation Plan: Native Explore Tools

**Branch**: `004-native-explore-tools` | **Date**: 2026-07-27 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/004-native-explore-tools/spec.md`

---

## Summary

This feature ports the `arbor`, `codegraph`, and `semblem` code-intelligence tools from external CLI/MCP dependencies to in-process Rust libraries exposed through n00n's existing Lua plugin layer. It introduces a new reusable `n00n-search` crate for indexing, BM25, and embedder orchestration; uses `rusqlite` for CodeGraph's SQLite index; keeps Arbor on `arbor-core` 2.1.0 / `arbor-graph` 2.1.0; and defaults `semblem` to BM25-only, nagging the user to configure a local or remote embedder when semantic search is requested.

---

## Technical Context

**Language/Version**: Rust 2024 edition (workspace `rust-version = 1.97`).

**Primary Dependencies**:
- `arbor-core` 2.1.0 + `arbor-graph` 2.1.0 (Arbor parsing and graph queries).
- `rusqlite` 0.40+ (bundled, `modern_sqlite` for WAL/FTS5) for CodeGraph index access.
- `tantivy` 0.26+ (BM25 full-text search) inside `n00n-search`.
- `ignore` + workspace `tree-sitter-*` grammars for file walking and chunking in `n00n-search`.
- `isahc` or workspace HTTP client for remote embedders.
- `model2vec-rs` 0.2+ (optional, behind `static-embed` feature) for a small static local embedder.
- `fs2` (or `file-lock`) for project-scoped index locks.

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
| `cargo deny check` | TBD | Must pass; dependency licenses are MIT/Apache-2.0. `arbor-graph`/`arbor-core` pull `tiktoken-rs ^0.5` and `tree-sitter ^0.22`, which duplicate n00n's `0.9`/`0.26` versions; `deny.toml` warns on duplicates, so this is acceptable but must be verified. |
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

| Feature | Default | Description |
|---------|---------|-------------|
| `arbor` | yes | Enables `n00n-arbor` and the Arbor plugin. |
| `codegraph` | yes | Enables `n00n-codegraph` and the CodeGraph plugin. |
| `semblem` | yes | Enables `n00n-semble`/`n00n-search` and the Semble plugin. |
| `vllm` | no | Pulls HTTP client helpers and vLLM preset generation into `n00n-search`. |
| `static-embed` | no | Pulls `model2vec-rs` for an optional local static embedder. |

Default `n00n` builds include `arbor`, `codegraph`, and `semblem` (BM25-only). Users opt into `vllm` or `static-embed` when they want semantic search. If binary bloat is measurable, the default set can be trimmed.

---

## Crate Responsibilities

### `n00n-search`

Reusable code-search core. It is **not** a tool; it is a library used by `n00n-semble` and future search features.

- `walk`: `.gitignore`-aware directory walker.
- `chunk`: source chunking by top-level definitions (tree-sitter) with line/paragraph fallback.
- `index`: `SearchIndex` combining a `tantivy` BM25 index and an optional dense-vector cache.
- `embed`: `Embedder` trait + implementations:
  - `None` (BM25-only).
  - `Static` (`model2vec-rs`, feature-gated).
  - `Vllm` (OpenAI-compatible `/v1/embeddings` against a local vLLM container).
  - `Remote` (OpenAI-compatible endpoint with user-supplied URL/key).
- `vllm`: `VllmPreset` definitions and Podman command generators.
- `rank`: RRF fusion of BM25 and vector rankings.
- `progress`: `IndexProgress` callback type.

### `n00n-semble`

Tool-specific crate for `semblem`.

- Loads/creates `.n00n/search/` index for a repo via `n00n-search`.
- Parses `semblem` arguments, calls `n00n-search` search/hybrid, formats `ToolResult`.
- Implements `find_related` by embedding the target chunk and performing vector similarity.
- Handles the "nag" UX: when `mode` is `hybrid`/`semantic` and no embedder is configured, return a message that lists local vLLM Podman presets and a remote option, then fall back to BM25.
- Exposes `n00n.semblem` Lua table.

### `n00n-codegraph`

Tool-specific crate for `codegraph`.

- Opens `.codegraph/codegraph.db` with `rusqlite` (bundled).
- Implements `explore`, `callers`, `callees`, `impact`, `status`, `ensure_indexed`.
- Uses FTS5 for natural-language node search, edge tables for call graphs, and custom SQL for blast radius.
- Triggers index build (or re-index if stale) when called.
- Exposes `n00n.codegraph` Lua table.

### `n00n-arbor`

Rewritten in-process Arbor client.

- Loads `.arbor/` via `arbor-graph::GraphStore` or builds from source via `arbor-core`/`arbor-graph`.
- Implements `callers`, `callees`, `map`, `diff`, `query`, `status`.
- Exposes `n00n.arbor` Lua table.

---

## vLLM Podman Presets

`n00n-search` generates preconfigured Podman commands for local embedding serving. All presets use `--task embed` and an OpenAI-compatible `/v1/embeddings` endpoint on port `8000`.

| Preset | Model | `--max-model-len` | `--max-num-seqs` | `--gpu-memory-utilization` | Notes |
|--------|-------|-------------------|------------------|----------------------------|-------|
| Light | `Snowflake/snowflake-arctic-embed-xs` | 512 | 32 | 0.4 | ~0.5-1 GB VRAM; fastest startup. |
| Medium | `Snowflake/snowflake-arctic-embed-m-v1.5` | 512 | 64 | 0.6 | ~2-3 GB VRAM; balanced throughput. |
| Heavy | `Snowflake/snowflake-arctic-embed-l-v1.5` | 512 | 128 | 0.8 | ~4-6 GB VRAM; best retrieval quality. |

Example generated command (Light):

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

CPU-only fallback is documented but not recommended for throughput.

---

## Indexing, Progress, and Concurrency

All three tools auto-index on first call and report progress through a moving status indicator.

- **Off-runtime indexing**: Indexing runs on a dedicated `std::thread` (or `smol::unblock`) so the `smol` executor is not blocked.
- **Progress callback**: Rust crates accept `Fn(IndexProgress)` and emit `IndexProgress` structs (`phase`, `processed`, `total`, `message`).
- **Lua card updates**: The Lua binding receives progress via a bounded channel and calls `card:update()` on an `ExploreResult` live card, then replaces it with the final tool output.
- **Concurrency control**: Each project index is protected by a `fs2::FileLock` on a sentinel lock file (`.arbor/.lock`, `.codegraph/.lock`, `.n00n/search/.lock`). If a build is already in progress, the tool returns a progress message without blocking.
- **Stale detection**: Each index records a `metadata.json` (SearchIndex) or uses the existing `files` table (CodeGraphIndex) / mtime checks (ArborGraph). Stale indexes trigger incremental updates.

---

## Phased Roadmap

### Phase 0: Validation Spikes

1. **Arbor index compatibility**: Create a throwaway crate, add `arbor-core` 2.1.0 + `arbor-graph` 2.1.0, and call `GraphStore::open(".arbor/")` + `load_graph()` on the n00n repo. Verify it loads without `cache version mismatch`.
2. **CodeGraph SQLite access**: Add `rusqlite` 0.40 with `bundled` + `modern_sqlite`, open `.codegraph/codegraph.db`, and run representative FTS5 and edge queries.
3. **Search BM25**: Add `tantivy` to a throwaway crate, index the n00n repo, and confirm sub-second BM25 queries.
4. **Dependency audit**: Add `arbor-core`/`arbor-graph`/`tantivy`/`rusqlite` to a throwaway `Cargo.toml` and run `cargo deny check`. Verify duplicate `tiktoken-rs`/`tree-sitter` warnings are only warnings.

### Phase 1: Crate Scaffolding

1. Create `n00n-search` with `Cargo.toml`, feature flags (`vllm`, `static-embed`), and empty module skeleton.
2. Create `n00n-codegraph` and `n00n-semble` with `Cargo.toml` and module skeleton.
3. Update root `Cargo.toml` workspace `members` and `workspace.dependencies`.
4. Wire `n00n-lua` API modules and `BUNDLED_PLUGINS` (initially as no-ops).
5. Run `cargo check --workspace` and `cargo deny check`.

### Phase 2: `n00n-search` Core

1. Implement `.gitignore`-aware file walker.
2. Implement tree-sitter chunker (reuse workspace grammars) with line fallback.
3. Implement `tantivy` BM25 index builder and searcher.
4. Implement `Embedder` trait and `None`/`Remote`/`Vllm` backends; `Static` behind `static-embed` feature.
5. Implement `VllmPreset` generator and Podman command formatting.
6. Implement `fs2` file lock for concurrent indexing.
7. Add tests with a small fixture repo (e.g., `n00n` self-index in tests).

### Phase 3: Arbor Native

1. Rewrite `n00n-arbor` to use `arbor-graph` 2.1.0 + `arbor-core` 2.1.0.
2. Update `n00n-lua/src/api/arbor.rs` and `plugins/arbor/init.lua`.
3. Add progress reporting and file-locking during indexing.
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
4. Add progress reporting and file-locking during indexing.
5. Validate output matches the existing `codegraph explore` format.

### Phase 6: Integration, Validation, and Docs

1. Update tool descriptions to remove external CLI installation notes.
2. Add feature flags to `n00n` root crate.
3. Run `cargo nextest run --workspace`, `cargo clippy --all --tests -- -D warnings`, `cargo deny check`.
4. Create integration test fixtures:
   - A small multi-language repo under `tests/fixtures/search-repo`.
   - Use the n00n repo itself for Arbor/CodeGraph/Search smoke tests.
5. Define performance regression tests:
   - Time `arbor map`, `codegraph explore`, `semblem search` on n00n.
   - Compare against baseline external-CLI timings captured before the migration.
6. Add user-facing migration/troubleshooting docs:
   - How to migrate existing `.arbor/`/`.codegraph/` indexes (no action needed; native tools read them).
   - vLLM Podman setup prerequisites (GPU, nvidia-container-toolkit, HuggingFace cache).
   - How to configure a remote embedder.
7. Measure tool-definition token size and tool call latency against baseline.
8. Draft PR with performance comparison.

---

## Quick Validation Guide

See [quickstart.md](quickstart.md) for end-to-end validation steps.
