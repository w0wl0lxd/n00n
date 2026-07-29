# Research: Memory System V2

## Internal baseline (pre-v2)

- `plugins/memory`: view/write/delete only; flat markdown files under `state/projects/{id}/memories/`.
- `memory_helpers`: path safety, line/byte limits, project_id, list formatting.
- Prompt hint: file count only (`"Memory files: N entries"`).
- No agent-loop recall, no ranking, no telemetry, no metadata schema.
- Blackboard plugin is separate (coordination, not long-term knowledge).

## Competitor patterns (verified via Context7, Exa, 2026-07-27)

1. **Claude Code**: CLAUDE.md instructions + auto memory notes; `/memory` command; session-start load.
2. **Letta**: archival memory with semantic search, tags, temporal filters; separate from core blocks.
3. **Community plugins**: lite/deep layers, hybrid search, hook-based auto-recall, local-first SQLite.

## Design decisions

1. **Offline-first ranking**: Token-overlap heuristic (like skill v2 ranking), no embedding dependency. Keeps n00n token/cost efficient and works without API keys.
2. **YAML frontmatter**: Same convention as skills; optional on legacy files.
3. **Lite layer injection**: Prompt hint shows lite entries (synopsis or first line) capped at 5; deep entries require explicit search/view.
4. **Index cache**: JSON metadata index in state dir; invalidate on mtime/size fingerprint change.
5. **Telemetry**: Mirror skill v2 JSONL pattern at `memories/events.jsonl`.
6. **No Rust agent changes in v2**: Recall stays in Lua prompt hints + tool commands; avoids coupling to agent loop this pass.

## Risks

| Risk | Mitigation |
|------|------------|
| Heuristic search misses semantic matches | Document limitation; future v3 can add optional embeddings |
| Frontmatter breaks legacy files | Parser treats missing frontmatter as body-only |
| Prompt hint bloat | Cap lite injections; importance ordering |

## Out of scope (v3)

- Embedding / vector search
- Auto-recall hooks on every user message
- Cross-project memory sync
- Rust agent-loop memory policy module
