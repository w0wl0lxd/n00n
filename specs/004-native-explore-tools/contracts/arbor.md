# Contract: `arbor` Tool

**Tool name**: `arbor`  
**Kind**: read  
**Audience**: main  

## Input Schema

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "enum": ["callers", "callees", "map", "diff", "query", "status"]
    },
    "symbol": { "type": "string" },
    "project": { "type": "string" },
    "token_budget": { "type": "integer", "default": 1024 }
  },
  "required": ["command"]
}
```

## Behavior

- `callers <symbol>`: Return functions/methods that call the given symbol.
- `callees <symbol>`: Return functions/methods called by the given symbol.
- `map`: Return a token-bounded, centrality-ranked project skeleton.
- `diff`: Return blast-radius impact of uncommitted git changes.
- `query <text>`: Free-text search of the code graph.
- `status`: Return index status (node count, edge count, file count).

## Output Contract

- Success: a plain-text list formatted as `name (kind) path:line` for `callers`/`callees`, a file/symbol tree for `map`, or raw status text.
- Error: `{ llm_output: "error: ...", is_error: true }`.
- The tool MUST use `ExploreResult` live cards for interactive display and respect `output_limits`.

## Dependencies

- Rust crate `n00n-arbor` wraps `arbor-core` + `arbor-graph`.
- Index directory: `.arbor/` at the project root.
