# Contract: Index Tool

**Tool**: `index`  
**Plugin**: `plugins/index/init.lua`

## Input Schema

```json
{
  "type": "object",
  "required": ["path"],
  "properties": {
    "path": {
      "type": "string",
      "description": "File path to index and skeletonize."
    }
  }
}
```

## Output Contract

Returns a compact single-file skeleton with top-level definitions, function signatures, and line ranges.

Format:

```text
file_path
  definition1 (line N-M)
  definition2 (line N-M)
  ...
```

## Error Handling

- If path is not provided, return error: "path is required".
- If file does not exist, return error: "file not found: {path}".
- If file is not a supported language, return error: "unsupported file type: {extension}".
- If tree-sitter parsing fails, return error: "failed to parse file: {path}".

## Notes

This tool is unchanged from spec 004. It remains a first-tier single-file skeleton tool.
