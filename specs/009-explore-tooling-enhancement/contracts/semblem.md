# Contract: Semblem Tool

**Tool**: `semblem`  
**Crate**: `n00n-semble`  
**Plugin**: `plugins/semblem/init.lua`

## Input Schema

```json
{
  "type": "object",
  "required": ["command"],
  "properties": {
    "command": {
      "type": "string",
      "enum": ["search", "find_related", "savings"],
      "description": "Semble command to execute."
    },
    "repo": {
      "type": "string",
      "description": "Repository path or remote git URL (defaults to cwd). Remote URLs require the N00N_SEMBLE_ALLOWED_REMOTE_REPOS allowlist."
    },
    "query": {
      "type": "string",
      "description": "Search query for search command."
    },
    "file_path": {
      "type": "string",
      "description": "File path for find_related command."
    },
    "line": {
      "type": "integer",
      "description": "Line number for find_related command."
    },
    "mode": {
      "type": "string",
      "enum": ["bm25", "hybrid", "semantic"],
      "default": "bm25",
      "description": "Search mode."
    },
    "content": {
      "type": "string",
      "enum": ["code", "docs", "config", "all"],
      "description": "Optional content filter for search command (upstream CLI only; no default)."
    },
    "top_k": {
      "type": "integer",
      "description": "Number of results to return (default 5)."
    }
  }
}
```

## Commands

|| Command | Description | Implementation |
||---------|-------------|----------------|
|| `search` | Search code by keyword or natural-language | Upstream CLI with remote URL and content filter support, or native BM25 fallback |
|| `find_related` | Find code similar to a specific location | Upstream CLI or native BM25 fallback |
|| `savings` | Analyze token savings | Upstream CLI only |

## Output Contract

### search

Returns ranked code snippets with file path, line range, score, and snippet content.

Format:

```
file_path:start_line-end_line score=0.xxx
snippet content
```

### find_related

Returns related code snippets with the same format as search.

### savings

Returns token savings analysis as formatted text.

## Error Handling

- If repo is omitted, the current working directory is used.
- Remote git URLs are only permitted when listed in the `N00N_SEMBLE_ALLOWED_REMOTE_REPOS` allowlist.
- If query is not provided for search command, return error: "query is required".
- If file_path or line is not provided for find_related, return error: "file_path and line are required".
- If upstream CLI is unavailable, fall back to native BM25 for search and find_related.
- If mode is hybrid or semantic and no embedder is configured, nag with vLLM presets and fall back to BM25.
- If native BM25 index fails, return error with search error message.

## Fallback Strategy

1. Prefer upstream Semble CLI 0.5.1 for search, find_related, savings.
2. Fall back to native n00n-search BM25 for search and find_related when CLI unavailable.
3. If mode is hybrid/semantic without embedder, nag and fall back to BM25.
4. Savings command requires upstream CLI; return error if unavailable.
