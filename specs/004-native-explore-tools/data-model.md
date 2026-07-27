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

### SembleIndex

A hybrid keyword/semantic index of source chunks.

| Field | Type | Description |
|-------|------|-------------|
| `chunks` | `Vec<Chunk>` | Indexed source snippets. |
| `bm25` | `BM25Index` | Keyword scoring index. |
| `embedder` | `Option<Embedder>` | Optional vector embedder for semantic search. |
| `flat` | `Option<Flat>` | Optional flat vector index for semantic search. |

**Chunk** fields:

| Field | Type | Description |
|-------|------|-------------|
| `file_path` | `String` | Source file path. |
| `start_line` | `usize` | Start line. |
| `end_line` | `usize` | End line. |
| `content` | `String` | Snippet text. |
| `language` | `Option<String>` | Detected language. |

### ToolResult

Common envelope returned to Lua plugins for rendering.

| Field | Type | Description |
|-------|------|-------------|
| `llm_output` | `String` | Truncated, formatted text for the model. |
| `body` | `Buf` | Optional live UI card buffer. |
| `is_error` | `bool` | True if the tool failed. |

## Relationships

- An **ArborGraph** is built from parsed **CodeNode**s and stored in `.arbor/`.
- A **CodeGraphIndex** is built by the `codegraph` indexer and stored in `.codegraph/`.
- A **SembleIndex** is built by walking source files and stored in a cache directory.
- Each **ToolResult** is produced by a Lua plugin that queries one of the above indexes through a `n00n.<tool>` API.
