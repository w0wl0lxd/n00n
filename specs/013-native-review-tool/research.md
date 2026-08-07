# Native review tool — Research

## Summary

The native `review` tool should integrate existing n00n primitives (arbor/codegraph for blast radius, skill system for checklists, subagent.launch for adversarial passes) into a unified interface that fetches diffs, analyzes impact, and returns structured findings. It depends on the upcoming `github`/`git` tools (#236) for PR/diff fetching but can work with local `git diff` immediately.

## Evidence

- Issue #238 defines the proposed schema: `target` (pr/branch/commit/file/diff), `focus` (security/correctness/performance/style/all), `depth` (quick/thorough), `output` (findings/comment_draft/both)
- Skill system loads from `~/.claude/skills` and `~/.agents/skills` with frontmatter metadata — `plugins/skill/init.lua:27-37`
- Arbor `diff` command returns blast radius impact (direct/indirect callers, entry points affected) — `plugins/arbor/init.lua:189-210`
- CodeGraph `affected` command takes file array and returns impact analysis — `plugins/codegraph/init.lua:135-139`
- Subagent launch helper (`n00n.subagent.launch`) provides unified interface for model resolution, system prompts, tool setup — `plugins/lib/n00n/subagent.lua:35-245`
- Existing review skills: `adversarial`, `security-review`, `code-review`
- GitHub/git tool (#236) will add `n00n-git` crate with gix, expose `n00n.git` Lua table, and add `plugins/git`/`plugins/github`
- DEFAULT_BUILTINS in n00n-config defines which plugins are enabled by default — `n00n-config/src/lib.rs:60-88`

## Map (if code)

- Entry points:
  - `plugins/review/init.lua` - Tool registration and handler
  - `n00n-lua/src/api/review.rs` - Rust host functions (if needed for git/github integration)

- Key symbols / files:
  - `plugins/skill/init.lua` - Skill discovery and loading
  - `plugins/arbor/init.lua` - Blast radius via `arbor diff`
  - `plugins/codegraph/init.lua` - Impact via `codegraph affected`
  - `plugins/lib/n00n/subagent.lua` - Subagent launch pattern
  - `~/.agents/skills/{adversarial,security-review,code-review}/SKILL.md` - Review skill templates
  - `n00n-config/src/lib.rs` - DEFAULT_BUILTINS list

- Call / data flow:
  1. User calls `review` tool with target/focus/depth
  2. Handler fetches diff (local `git diff` or future `github` tool)
  3. Extract changed files, call `arbor diff` or `codegraph affected` for blast radius
  4. Load appropriate skill based on `focus` (security-review for security, code-review for correctness, adversarial for all)
  5. Launch adversarial subagent with diff + blast radius + skill prompt
  6. Parse structured output into findings (severity, location, suggestion)
  7. Return findings or PR comment draft

## Open questions / gaps

- GitHub tool (#236) is not yet implemented — review tool should work with local `git diff` first, integrate with `github` tool when available
- Should review tool have its own Rust crate or use existing `n00n-git` when available?
- How to handle `comment_draft` output format — markdown template for GitHub PR comments?
- Should `focus=all` run multiple subagents in parallel (security + correctness + style) or one general pass?
- Permission scope granularity — `review.read` for diff access, `review.write` for posting comments, `review.subagent` for adversarial pass?
