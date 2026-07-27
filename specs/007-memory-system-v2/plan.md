# Implementation Plan: Memory System V2

## Phase 1 — Helpers and frontmatter (P1)

- Extend `memory_helpers.lua`: `parse_frontmatter`, `parse_memory_file`, `tokenize`, `score_memory`, `rank_memories`, `format_entry`, `build_frontmatter`, `append_body`.
- Unit tests in `plugins/memory/tests/spec.lua`.

## Phase 2 — Search, append, enriched write (P1)

- `search` command: query + optional tags/path/limit.
- `append` command: append to body preserving frontmatter.
- `write` accepts tags, topic, importance, layer, synopsis, paths.
- `view` list mode supports optional `query` ranking.

## Phase 3 — Index cache and prompt injection (P2)

- `memory_index.lua`: fingerprint, load/save, build from dir.
- Upgrade prompt hint to inject lite summaries.

## Phase 4 — Telemetry and integration tests (P2)

- `memory_telemetry.lua`: append events.
- `plugin_host.rs`: `memory_tool_search_ranks_results`, `memory_tool_append_preserves_frontmatter`.
- `just gen-docs`.

## File map

| File | Action |
|------|--------|
| `plugins/memory/memory_helpers.lua` | extend |
| `plugins/memory/memory_index.lua` | new |
| `plugins/memory/memory_telemetry.lua` | new |
| `plugins/memory/init.lua` | extend |
| `plugins/memory/tests/spec.lua` | extend |
| `n00n-lua/tests/plugin_host.rs` | new tests |
| `specs/007-memory-system-v2/*` | new |
