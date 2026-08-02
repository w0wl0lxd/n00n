# Contract: CodeGraph Tool

**Tool**: `codegraph`  
**Crate**: `n00n-codegraph`  
**Plugin**: `plugins/codegraph/init.lua`

## Input Schema

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "enum": ["explore", "callers", "callees", "impact", "affected", "node", "query", "sync", "files"],
      "description": "CodeGraph command to execute."
    },
    "query": {
      "type": "string",
      "description": "Natural-language query for explore command."
    },
    "symbol": {
      "type": "string",
      "description": "Symbol name for callers, callees, impact commands."
    },
    "node_id": {
      "type": "string",
      "description": "Node ID for node command."
    },
    "files": {
      "type": "array",
      "items": { "type": "string" },
      "description": "Array of file paths for affected command."
    },
    "projectPath": {
      "type": "string",
      "description": "Project root path (defaults to cwd)."
    },
    "timeout_secs": {
      "type": "integer",
      "description": "Timeout in seconds for CLI commands (default 30)."
    }
  }
}
```

## Commands

|| Command | Description | Implementation |
||---------|-------------|----------------|
|| `explore` | Natural-language cross-file structural analysis | Native SQLite FTS5 or CLI fallback |
|| `callers` | List callers of a symbol | Native SQLite edges query or CLI fallback |
|| `callees` | List callees of a symbol | Native SQLite edges query or CLI fallback |
|| `impact` | Blast-radius impact analysis | Native SQLite edges query or CLI fallback |
|| `affected` | List nodes affected by a change | Native SQLite edges query or CLI fallback |
|| `node` | Get node details by ID | Native SQLite nodes query or CLI fallback |
|| `query` | Symbol-specific query | Native SQLite nodes query or CLI fallback |
|| `sync` | Re-index the project | CLI only (triggers CodeGraph 1.5.0 sync) |
|| `files` | List indexed files | Native SQLite files query or CLI fallback |

## Output Contract

### explore

Returns grouped source snippets by file with a blast-radius summary.

### callers, callees

Returns a list of relations with name, path, kind, and line.

### impact, affected

Returns impact metrics: direct_callers, indirect_callers, blast_radius_nodes, api_entrypoints_affected, files_likely_require_updates.

### node

Returns node details: id, name, qualified_name, kind, file_path, start_line, end_line, signature, docstring.

### query

Returns query results as formatted text.

### sync

Returns sync status and progress.

### files

Returns list of indexed files with content_hash, language, size, modified_at.

## Error Handling

- If query is empty for explore command, return error: "query is required".
- If no `.codegraph/` index exists, return error: "no .codegraph/ index found in {project}".
- If CLI command fails, return error with stderr.
- If SQLite query fails, return error with SQLite error message.
- If CLI is unavailable and command is not supported natively, return error: "codegraph CLI not found and command not supported natively".

## Fallback Strategy

1. Prefer native SQLite queries for explore, callers, callees, impact, affected, node, query, files.
2. Fall back to CLI 1.5.0 for sync and commands not yet supported natively.
3. If CLI is unavailable, return clear error for unsupported commands.
