# Contract: `semblem` Tool

**Tool name**: `semblem`  
**Kind**: read  
**Audience**: main  

## Input Schema

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "enum": ["search", "find_related"]
    },
    "repo": { "type": "string" },
    "query": { "type": "string" },
    "file_path": { "type": "string" },
    "line": { "type": "integer" },
    "mode": {
      "type": "string",
      "enum": ["bm25", "hybrid", "semantic"],
      "default": "bm25"
    },
    "top_k": { "type": "integer", "default": 5 },
    "max_snippet_lines": { "type": ["integer", "null"], "default": 10 }
  },
  "required": ["command"]
}
```

## Behavior

- `search`: Return ranked code snippets matching the query.
  - `mode = "bm25"` (default): keyword/BM25 search, no embedder required.
  - `mode = "hybrid"`: BM25 + semantic search fused via RRF; requires a configured embedder.
  - `mode = "semantic"`: vector-only search; requires a configured embedder.
- `find_related`: Return code related to the given `file_path` and `line`; uses the configured embedder or falls back to BM25.
- `repo`: Local project path or `https://` git URL.
- `top_k`: Maximum number of results.
- `max_snippet_lines`: Maximum snippet context lines per result.

## Output Contract

- Success: a plain-text list of ranked snippets formatted as `file_path:start_line-end_line score` followed by the snippet.
- Embedder nag: when `mode` is `hybrid` or `semantic` and no embedder is configured, the tool returns a message listing local vLLM Podman presets (light/medium/heavy) and a remote OpenAI-compatible option, then falls back to BM25 results.
- Error: `{ llm_output: "error: ...", is_error: true }`.
- The tool MUST use `ExploreResult` live cards and respect `output_limits`.

## Dependencies

- Rust crate `n00n-semble` wraps `n00n-search`.
- `n00n-search` uses `tantivy` for BM25 and an optional embedder.
- Index: `.n00n/search/` under the project or a temp directory for `https://` URLs.
