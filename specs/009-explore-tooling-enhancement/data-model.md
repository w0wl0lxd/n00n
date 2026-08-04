# Data Model: Explore Tooling Enhancement

**Feature**: Explore Tooling Enhancement  
**Date**: 2026-08-01

## Entities

### ExploreIntent

The classification of a user query for routing to the appropriate backend.

|| Field | Type | Description |
||-------|------|-------------|
|| `auto` | enum | Default intent; router infers from query patterns |
|| `file` | enum | Single-file skeleton query |
|| `relations` | enum | Caller/callee/impact/trace queries |
|| `cross_file` | enum | Cross-file structural analysis |
|| `search` | enum | Keyword or natural-language search |
|| `skeleton` | enum | File skeleton (alias for file) |
|| `symbol` | enum | Symbol-specific drill-down |
|| `impact` | enum | Blast-radius or impact analysis |
|| `trace` | enum | Call path tracing |

### ExploreRoute

The mapping from intent to backend tool and input parameters.

|| Field | Type | Description |
||-------|------|-------------|
|| `intent` | ExploreIntent | The classified intent |
|| `backend` | string | The target tool name (index, arbor, codegraph, semblem) |
|| `backend_input` | object | The input parameters for the backend tool |
|| `cache_key` | string | Unique key for session caching |

### CodeGraphIndex

A SQLite database stored at `<project>/.codegraph/codegraph.db`.

|| Table | Purpose |
||-------|---------|
|| `nodes` | Code entities with kind, name, qualified_name, file_path, start_line, end_line, signature, docstring |
|| `edges` | Relationships between nodes (source, target, kind, line, col, metadata) |
|| `files` | Indexed files with content_hash, language, size, modified_at |
|| `unresolved_refs` | References that could not be resolved to a node |
|| `nodes_fts` | FTS5 virtual table over nodes for natural-language search |

### ArborGraph

A directed graph of code entities and their relationships, stored in `.arbor/graph.json`.

|| Field | Type | Description |
||-------|------|-------------|
|| `nodes` | Vec<CodeNode> | Code entities extracted from source files |
|| `edges` | Vec<GraphEdge> | Relationships such as calls, imports, contains, implements |
|| `symbol_table` | SymbolTable | Map from qualified/symbol names to node identifiers |
|| `centrality` | HashMap<NodeId, f64> | Pre-computed centrality scores for ranking |

**CodeNode** fields:

|| Field | Type | Description |
||-------|------|-------------|
|| `id` | String | Unique node identifier |
|| `name` | String | Short symbol name |
|| `qualified_name` | String | Fully-qualified name |
|| `kind` | NodeKind | Function, Method, Struct, Module, etc. |
|| `file` | String | Absolute source file path |
|| `line_start` | usize | Start line (1-based) |
|| `line_end` | usize | End line (1-based) |
|| `visibility` | Visibility | public / private / etc. |
|| `signature` | String | Compact signature for display |

### SearchIndex

A unified BM25 + optional semantic code-search index managed by `n00n-search`, stored under `.n00n/search/`.

|| Component | Purpose |
||-----------|---------|
|| `tantivy_index/` | BM25 inverted index over chunk content, path, language, start_line, end_line |
|| `vectors.bin` | Optional dense vector cache keyed by chunk id |
|| `metadata.json` | Index version, embedder fingerprint, last indexed timestamp |
|| `.lock` | File lock to serialize concurrent index builds |

**Chunk** fields:

|| Field | Type | Description |
||-------|------|-------------|
|| `id` | u64 | Stable chunk identifier (hash of path + start line) |
|| `file_path` | String | Absolute source file path |
|| `start_line` | usize | Start line (1-based) |
|| `end_line` | usize | End line (1-based) |
|| `content` | String | Snippet text |
|| `language` | String | Detected language (file-extension based) |

### EmbedderConfig

Defines how semantic vectors are produced for Semble.

|| Variant | Fields | Description |
||---------|--------|-------------|
|| `None` | — | BM25-only; no vectors |
|| `Static` | `model_id: String` | Local static model (feature-gated, not used in this spec) |
|| `Vllm` | `url: String`, `model: String` | OpenAI-compatible endpoint served by local vLLM container |
|| `Remote` | `url: String`, `api_key: String`, `model: String` | User-supplied OpenAI-compatible endpoint |

### ToolResult

Common envelope returned to Lua plugins for rendering.

|| Field | Type | Description |
||-------|------|-------------|
|| `llm_output` | String | Truncated, formatted text for the model |
|| `body` | Buf | Optional live UI card buffer (used for progress and results) |
|| `is_error` | bool | True if the tool failed |

### RtkRewrite

The transformation of a shell command through rtk for token-efficient output.

|| Field | Type | Description |
||-------|------|-------------|
|| `original_command` | String | The original shell command |
|| `rewritten_command` | String | The rtk-rewritten command (or nil if not rewritten) |
|| `availability_cached` | bool | Whether rtk availability is cached for the session |
|| `unsupported` | bool | Whether the command is unsupported by rtk |

## Relationships

- An **ExploreIntent** is classified by the router from the user query.
- An **ExploreRoute** maps an intent to a backend tool and input parameters.
- A **CodeGraphIndex** is queried by the CodeGraph tool for structural information.
- An **ArborGraph** is loaded from `.arbor/graph.json` for in-memory queries.
- A **SearchIndex** is queried by Semble for keyword and semantic search.
- An **EmbedderConfig** is selected by the user for semantic search; `None` is the default.
- Each **ToolResult** is produced by a Lua plugin that queries one of the above indexes.
- An **RtkRewrite** is applied by the bash plugin to compress shell output.

## Changes from 004

This spec extends the data model from 004-native-explore-tools with:

- **ExploreIntent**: Added `search`, `skeleton`, `symbol`, `impact`, `trace` intents.
- **ExploreRoute**: Enhanced routing logic to support new intents and backends.
- **CodeGraphIndex**: No schema changes, but additional commands (callers, callees, impact, affected, node, query, sync) will query the existing tables.
- **ArborGraph**: No schema changes, but additional commands (entry-points, file-graph, inspect, path, refactor, check, summary) will be exposed via CLI wrapper.
- **SearchIndex**: No schema changes, but upstream Semble CLI will be wrapped for additional features (remote URLs, content filters).
- **RtkRewrite**: New entity to represent RTK command rewriting with session caching.
