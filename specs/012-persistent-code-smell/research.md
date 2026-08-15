# Research: Persistent code-smell and comment index

## Existing systems

### `n00n-semble` / `n00n-search`

- `n00n-search/src/index.rs` implements a Tantivy `SearchIndex` under `.n00n/search`.
- Schema: `content` (indexed/stored text), `path` (STRING | STORED), `start_line`/`end_line` (u64 STORED), `language` (STRING STORED).
- `update` rebuilds the index by walking the repo and chunking files (`n00n-search/src/walk.rs`, `n00n-search/src/chunk.rs`).
- `search` uses `QueryParser` over `content` and returns `TopDocs`.
- `n00n-semble/src/lib.rs` wraps this for the Lua API and the `semblem` tool.

### `n00n-git conflicts`

- `n00n-git/src/conflicts.rs` exposes `find(path, &ConflictsOptions) -> Result<GitConflicts, GitError>`.
- `GitConflicts` contains `Vec<ConflictFile>`; each file has `path`, `findings`, `truncated`.
- `Finding` has `kind`, `line`, `message`, `content` (for compact/full/both output), and optional `hunk`.
- `FindingKind` covers `Conflict`, `Todo`, `Fixme`, `Hack`, `Placeholder`.
- This is the source of truth for smell detection; the new index should consume it rather than duplicate parsing.

### Lua tooling

- `n00n-lua/src/api/mod.rs` registers API namespaces (`n00n.semblem`, `n00n.arbor`, etc.).
- `n00n-lua/src/api/semblem.rs` shows the pattern: create a Lua table with functions, map errors with `mlua::Error::external`.
- Plugins are in `plugins/<name>/init.lua`; each calls `n00n.api.register_tool` and uses `ToolView`/`ExploreResult` helpers.

## Design decisions

- Build a **separate** `.n00n/smells` Tantivy index instead of adding fields to `.n00n/search`. Smell documents have a different shape (single-line or hunk) and a `kind` facet, so a dedicated index avoids schema migration and keeps `n00n-search` focused on code chunks.
- Reuse `n00n-git::conflicts::find` as the scanner. This avoids duplicating conflict-marker and comment-smell parsing logic and ensures consistency.
- Provide a Rust CLI `n00n-smell` with `index` and `search` subcommands, and a Lua tool `smell` that calls `n00n.smell` bindings.

## References

- `n00n-search/src/index.rs` — Tantivy index pattern
- `n00n-semble/src/lib.rs` — client and CLI fallback patterns
- `n00n-git/src/conflicts.rs` — source smell data
- `n00n-lua/src/api/semblem.rs` — Lua API binding pattern
- `plugins/semblem/init.lua` — built-in tool pattern
