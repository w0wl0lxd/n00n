Return a compact skeleton of a source file: imports, type definitions, function signatures, and structure with their line numbers sorrounded by []. ~70-90% fewer tokens than reading the full file.

- Use this FIRST to understand file structure before using read with offset/limit.
- Supports: Rust, Python, TypeScript, JavaScript.
- Returns an error for unsupported file types. Use read instead.
