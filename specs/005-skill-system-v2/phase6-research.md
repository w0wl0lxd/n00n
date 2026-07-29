# Research: Skill System V2 Phase 6

## Internal baseline (post Phase 5)

- Skill plugin parses tool policy frontmatter and returns `state.active_skill` on load.
- `skill_policy.lua` provides Lua-side `evaluate` and `call_tool` wrapper only.
- `tool_dispatch::run` enforces audience, plan-mode writes, permissions — no skill policy hook.
- `ToolContext` has no `active_skill_policy` field; policy does not persist across agent turns.
- Ranking is heuristic (name/description/tags tokens) without graph signals.
- `plan=true` extracts markdown sections only; no `steps` frontmatter.

## Design decisions

1. **Rust enforcement in `n00n-agent`**: Add `skill_policy` module mirroring Lua normalization; gate in `tool_dispatch::run` after audience check, before permission enforcement.
2. **Policy lifecycle**: Updated from `skill` tool `ToolDoneEvent.output.state().active_skill`; cleared when skill loads without policy or on explicit null.
3. **Parallel tool calls**: Same-batch skill calls establish policy before non-skill tools execute; skill calls run first within the batch and results retain original ordering.
4. **Graph rank**: Lightweight index-presence bonuses (+arbor indexed, +codegraph dir) rather than per-list arbor/codegraph queries (latency/token cost).
5. **Telemetry**: Append-only JSONL at `state_dir/projects/{pid}/skills/events.jsonl` via `skill_telemetry.lua`.
6. **Execution plans**: Normalize YAML `steps` array; `plan=true` prefers frontmatter steps over body extraction.

## Risks

| Risk | Mitigation |
|------|------------|
| Policy blocks legitimate tools after skill load | Clear policy when skill without envelope loads; explicit error messages |
| Graph bonus without real relevance | Bonus only when skill already path-scoped |
| Telemetry file growth | JSONL append; no rotation in v1 |
