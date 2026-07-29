# Research: Skill System V2

## Internal baseline

- Current `plugins/skill/init.lua` discovery is one-level deep per root.
- Current metadata parsing does not normalize skill policy fields beyond `name` and `description`.
- Existing tests validate basic skill list/load behavior and frontmatter parsing, but not nested scopes or manual-only policies.

## External capability baseline (verified during session)

- Cursor skills support nested directories, path scoping (`paths`), and invocation control (`disable-model-invocation`).
- Claude Code skills support nested discovery, scoped paths, manual-only controls, and progressive loading.
- Codex skills documentation indicates similar progressive skill metadata and explicit/implicit invocation controls.

## Design decisions

1. Add recursive discovery in `scan_skill_dir` so monorepo-style nested skills are first-class.
2. Normalize frontmatter fields in `skill_helpers.lua`:
   - `paths` accepts comma-separated strings and YAML arrays.
   - `disable-model-invocation` normalizes into strict bool.
3. Keep behavior backwards-compatible:
   - Existing skills without metadata continue to work.
   - Existing error shape retained for unknown/out-of-scope names.
4. Add optional tool inputs:
   - `path` for focus-based scope filtering.
   - `include_manual` for manual-only listing/debugging.

## Risk and mitigation

- **Risk**: recursive scanning could include unintended folders.
  - **Mitigation**: load only directories containing `SKILL.md`.
- **Risk**: `paths` matching could be expensive if many patterns.
  - **Mitigation**: bounded glob limit and short-circuit on first match.
- **Risk**: hidden manual-only skills make debugging harder.
  - **Mitigation**: `include_manual=true` explicitly reveals them.
