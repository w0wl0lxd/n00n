# Contract: `codegraph` Tool

**Tool name**: `codegraph`  
**Kind**: read  
**Audience**: main  

## Input Schema

```json
{
  "type": "object",
  "properties": {
    "query": { "type": "string" },
    "projectPath": { "type": "string" }
  },
  "required": ["query"]
}
```

## Behavior

- `query`: A natural-language question or symbol/file names. The tool returns verbatim source grouped by file plus a dependency impact summary.
- `projectPath`: Absolute path to the project (defaults to current workspace).

## Output Contract

- Success: plain text with grouped source snippets, call paths, and a blast-radius summary.
- Error: `{ llm_output: "error: ...", is_error: true }` when the index is missing or invalid.
- The tool MUST use `ExploreResult` live cards and respect `output_limits`.

## Dependencies

- Rust crate `n00n-codegraph` wraps `cgz` or a `rusqlite` query layer over `.codegraph/codegraph.db`.
- Index directory: `.codegraph/` at the project root.
