# Contract: Arbor Tool

**Tool**: `arbor`  
**Crate**: `n00n-arbor`  
**Plugin**: `plugins/arbor/init.lua`

## Input Schema

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "enum": ["callers", "callees", "map", "diff", "query", "status", "entry-points", "file-graph", "inspect", "path", "refactor", "check", "summary"],
      "description": "Arbor command to execute."
    },
    "symbol": {
      "type": "string",
      "description": "Symbol name for callers, callees, inspect commands."
    },
    "from_symbol": {
      "type": "string",
      "description": "From symbol for trace_path command."
    },
    "to_symbol": {
      "type": "string",
      "description": "To symbol for trace_path command."
    },
    "path": {
      "type": "string",
      "description": "File path for file-graph command."
    },
    "project": {
      "type": "string",
      "description": "Project root path (defaults to cwd)."
    },
    "token_budget": {
      "type": "integer",
      "description": "Token budget for map command (default 1024)."
    }
  }
}
```

## Commands

|| Command | Description | Implementation |
||---------|-------------|----------------|
|| `callers` | List callers of a symbol | Native in-memory graph.json or CLI fallback |
|| `callees` | List callees of a symbol | Native in-memory graph.json or CLI fallback |
|| `map` | Project map with ranked symbols | CLI only |
|| `diff` | Diff impact analysis | CLI only |
|| `query` | Symbol query | CLI only |
|| `status` | Index status | CLI only |
|| `entry-points` | List entry points | CLI only |
|| `file-graph` | File-level graph | CLI only |
|| `inspect` | Inspect a symbol | CLI only |
|| `path` | Path between symbols | CLI only |
|| `refactor` | Refactoring suggestions | CLI only |
|| `check` | Check graph health | CLI only |
|| `summary` | Graph summary | CLI only |

## Output Contract

### callers, callees

Returns a list of relations with name, path, kind, and line.

### map

Returns a list of map entries with file and symbols (name, kind, line, centrality, callers, is_entry_point).

### diff

Returns impact metrics: direct_callers, indirect_callers, blast_radius_nodes, api_entrypoints_affected, files_likely_require_updates.

### query

Returns query results as formatted text.

### status

Returns index status as formatted text.

### entry-points

Returns list of entry points with symbol details.

### file-graph

Returns file-level graph with nodes and edges.

### inspect

Returns detailed symbol information.

### path

Returns path between symbols with intermediate nodes.

### refactor

Returns refactoring suggestions.

### check

Returns graph health check results.

### summary

Returns graph summary statistics.

## Error Handling

- If CLI command fails, return error with stderr.
- If CLI is unavailable and command is not supported natively, return error: "arbor CLI not found and command not supported natively".
- If graph.json is missing for native queries, return error: "missing Arbor graph file: {path}".
- If index is stale, auto-index before query (for CLI commands).

## Fallback Strategy

1. Prefer native in-memory graph.json for callers, callees, trace_path when CLI unavailable.
2. Use CLI 2.5.0 for all other commands.
3. If CLI is unavailable and command is not supported natively, return clear error.
