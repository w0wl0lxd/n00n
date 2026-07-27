# Data Model: Native Explore Tools

**Feature**: Native Explore Tools  
**Date**: 2026-07-27

## Entities

### ArborGraph

A directed graph of code entities and their relationships.

| Field | Type | Description |
|-------|------|-------------|
| `nodes` | `Vec<CodeNode>` | Code entities extracted from source files (functions, structs, modules, etc.). |
| `edges` | `Vec<GraphEdge>` | Relationships such as `calls`, `imports`, `contains`, `implements`. |
| `symbol_table` | `SymbolTable` | Map from qualified/symbol names to node identifiers for fast lookup. |
| `centrality` | `HashMap<NodeId, f64>` | Pre-computed centrality scores used to rank `map` output. |

**CodeNode** fields of interest:

| Field | Type | Description |
|-------|------|-------------|
| `id` | `String` | Unique node identifier. |
| `name` | `String` | Short symbol name. |
| `qualified_name` | `String` | Fully-qualified name. |
| `kind` | `NodeKind` | Function, Method, Struct, Module, etc. |
| `file` | `String` | Absolute source file path. |
| `line_start` | `usize` | Start line (1-based). |
| `line_end` | `usize` | End line (1-based). |
| `visibility` | `Visibility` | public / private / etc. |
| `signature` | `String` | Compact signature for display. |

### CodeGraphIndex

A SQLite database stored at `<project>/.codegraph/codegraph.db`.

| Table | Purpose |
|-------|---------|
| `nodes` | Code entities with `kind`, `name`, `qualified_name`, `file_path`, `start_line`, `end_line`, `signature`, `docstring`. |
| `edges` | Relationships between nodes (`source`, `target`, `kind`, `line`, `col`, `metadata`). |
| `files` | Indexed files with `content_hash`, `language`, `size`, `modified_at`. |
| `unresolved_refs` | References that could not be resolved to a node. |
| `nodes_fts` | FTS5 virtual table over `nodes` for natural-language search. |

### SearchIndex

A unified BM25 + optional semantic code-search index managed by `n00n-search`, stored under `.n00n/search/`.

| Component | Purpose |
|-----------|---------|
| `tantivy_index/` | BM25 inverted index over chunk `content`, `path`, `language`, `start_line`, `end_line`. |
| `vectors.bin` | Optional dense vector cache keyed by chunk id. |
| `metadata.json` | Index version, embedder fingerprint, last indexed timestamp. |

**Chunk** fields:

| Field | Type | Description |
|-------|------|-------------|
| `id` | `u64` | Stable chunk identifier (e.g., hash of path + start line). |
| `file_path` | `String` | Absolute source file path. |
| `start_line` | `usize` | Start line (1-based). |
| `end_line` | `usize` | End line (1-based). |
| `content` | `String` | Snippet text. |
| `language` | `String` | Detected language (file-extension based). |

### Embedder Config

Defines how semantic vectors are produced.

| Variant | Fields | Description |
|---------|--------|-------------|
| `None` | — | BM25-only; no vectors. |
| `Static` | `model_id: String` | Local static `model2vec` model loaded from HuggingFace cache or local path (feature-gated). |
| `Vllm` | `url: String`, `model: String` | OpenAI-compatible `/v1/embeddings` endpoint served by a local vLLM container. |
| `Remote` | `url: String`, `api_key: String`, `model: String` | User-supplied OpenAI-compatible endpoint. |

### VllmPreset

A preconfigured local embedding server option.

| Field | Type | Description |
|-------|------|-------------|
| `name` | `String` | Light / Medium / Heavy. |
| `model` | `String` | HuggingFace model id (e.g., `Snowflake/snowflake-arctic-embed-xs`). |
| `max_model_len` | `usize` | Max tokens per sequence. |
| `max_num_seqs` | `usize` | Max concurrent sequences. |
| `gpu_memory_utilization` | `f32` | GPU memory fraction to reserve. |
| `podman_command` | `String` | Generated `podman run ...` command. |

### ToolResult

Common envelope returned to Lua plugins for rendering.

| Field | Type | Description |
|-------|------|-------------|
| `llm_output` | `String` | Truncated, formatted text for the model. |
| `body` | `Buf` | Optional live UI card buffer (used for progress and results). |
| `is_error` | `bool` | True if the tool failed. |

## Relationships

- An **ArborGraph** is built from parsed **CodeNode**s and stored in `.arbor/`.
- A **CodeGraphIndex** is built by the `codegraph` indexer and stored in `.codegraph/`.
- A **SearchIndex** is built by `n00n-search` and stored in `.n00n/search/`.
- An **Embedder Config** is selected by the user; `None` is the default for `semblem`.
- A **VllmPreset** is offered to the user when they request semantic search without a configured embedder.
- Each **ToolResult** is produced by a Lua plugin that queries one of the above indexes through a `n00n.<tool>` API.
