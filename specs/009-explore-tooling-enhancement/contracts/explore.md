# Contract: Explore Tool

**Tool**: `explore`  
**Plugin**: `plugins/explore/init.lua`  
**Router**: `plugins/explore/router.lua`

## Input Schema

```json
{
  "type": "object",
  "required": ["query"],
  "properties": {
    "query": {
      "type": "string",
      "description": "Question, symbol, or file path to explore."
    },
    "path": {
      "type": "string",
      "description": "File path for skeleton queries. A file extension selects the index backend in auto mode."
    },
    "project": {
      "type": "string",
      "description": "Project root for arbor/codegraph queries (defaults to cwd)."
    },
    "intent": {
      "type": "string",
      "enum": ["auto", "file", "relations", "cross_file", "search", "skeleton", "symbol", "impact", "trace"],
      "default": "auto"
    },
    "command": {
      "type": "string",
      "enum": ["callers", "callees", "trace_path", "map", "diff", "query", "status"]
    },
    "symbol": { "type": "string" },
    "from_symbol": { "type": "string" },
    "to_symbol": { "type": "string" },
    "token_budget": { "type": "integer", "default": 1024 },
    "use_cache": { "type": "boolean", "default": false }
  }
}
```

## Intent Enum

|| Intent | Description | Backend |
||--------|-------------|---------|
|| `auto` | Router infers from query patterns | index, arbor, codegraph, or semblem |
|| `file` | Single-file skeleton query | index |
|| `relations` | Caller/callee/impact/trace queries | arbor |
|| `cross_file` | Cross-file structural analysis | codegraph |
|| `search` | Keyword or natural-language search | semblem |
|| `skeleton` | File skeleton (alias for file) | index |
|| `symbol` | Symbol-specific drill-down | arbor or codegraph |
|| `impact` | Blast-radius or impact analysis | arbor or codegraph |
|| `trace` | Call path tracing | arbor |

## Routing Logic

1. If `intent` is not `auto`, use the specified intent.
2. If `command` is provided, route to `arbor` with the command.
3. If `path` is provided and looks like a file path, route to `index`.
4. If `query` looks like a file path, route to `index`.
5. If `query` contains relation keywords (caller, callee, trace, map, status, diff), route to `arbor`.
6. If `intent` is `search`, route to `semblem`.
7. Default to `cross_file` and route to `codegraph`.

## Output Contract

```json
{
  "llm_output": "string (formatted results with route prefix)",
  "body": "Buf (optional live UI card buffer)",
  "is_error": "boolean"
}
```

The `llm_output` includes a route prefix indicating the backend used:
- `[file via index]`
- `[relations via arbor]`
- `[cross_file via codegraph]`
- `[search via semblem]`

Cached results include `, cached` in the route prefix.

## Error Handling

- If `query` is empty or whitespace, return error: "query is required".
- If backend dispatch fails, return error: "explore dispatch to {backend} failed: {error}".
- If live card creation fails, return error: "failed to publish explore results: {error}".
