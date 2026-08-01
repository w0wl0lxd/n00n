# Implementation Plan: Explore Tooling Enhancement

**Branch**: `009-explore-tooling-enhancement` | **Date**: 2026-08-01 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/009-explore-tooling-enhancement/spec.md`

## Summary

This feature expands and refines the CodeGraph, Semblem, and Arbor integrations to make them first-tier exploration tools alongside index. Key changes include: upgrading CodeGraph to 1.5.0 and exposing additional commands (callers, callees, impact, affected, files, node, query, sync); expanding Arbor to expose entry-points, file-graph, inspect, path, refactor, check, summary, and trace; wrapping the upstream `semble` CLI v0.5.1 for remote URLs and content filters while keeping native BM25 fallback; enhancing the explore router with new intents (search, skeleton, symbol, impact, trace); updating prompts and tool descriptions to position these tools as first-tier; and hardening RTK integration in the bash plugin with session caching and broader coverage.

## Technical Context

**Language/Version**: Rust 2024 edition (workspace `rust-version = 1.97`), Lua 5.1 (Luau).

**Primary Dependencies**:
- `rusqlite` 0.40+ (bundled, `modern_sqlite` for WAL/FTS5) for CodeGraph SQLite access.
- `arbor-core` 2.5.0 + `arbor-graph` 2.5.0 (existing) for Arbor parsing and graph queries.
- `tantivy` 0.26+ (existing) for BM25 search in `n00n-search`.
- `semble` CLI v0.5.0+ (external, optional) for upstream semantic search.
- `rtk` CLI (external, optional) for bash command rewriting.

**Storage**: Existing project-side indexes under `.codegraph/`, `.arbor/`, and `.n00n/search/`. No new persistent storage in n00n itself.

**Testing**: `cargo nextest run --workspace`, `cargo clippy --all --tests -- -D warnings`, `cargo deny check`, `just explore-health`.

**Target Platform**: Linux primary; macOS secondary. All chosen dependencies are pure Rust or use widely supported native parsers.

**Project Type**: CLI/TUI agent with built-in Lua plugins.

**Performance Goals**:
- Tool call latency for arbor, codegraph, and semblem must not exceed current baseline.
- Tool definition token count must not increase beyond current baseline.
- RTK availability check must be cached per session to avoid repeated spawns.

**Constraints**:
- `unsafe_code = "deny"` workspace-wide; no new `unsafe` blocks.
- `unwrap_used` and `expect_used` denied in production code.
- New dependencies must pass `cargo deny check` and be published at least 7 days ago.
- Preserve existing input/output contracts; new commands are additive.
- No bundled credentials, cloud providers, or embedding models.
- Respect `output_limits`, `ExploreResult`, and existing tool output line budgets.

**Scale/Scope**: Extensions to existing crates (`n00n-codegraph`, `n00n-arbor`, `n00n-semble`), updates to Lua plugins (`explore`, `arbor`, `codegraph`, `semblem`, `bash`), and prompt updates in `n00n-agent`. No new crates.

## Constitution Check

*GATE: Must pass before Phase 1 research. Re-check after Phase 2 design.*

|| Gate | Status | Notes |
||------|--------|-------|
|| No new `unsafe` without review | Pass | No new unsafe blocks planned; existing CodeGraph SQLite access uses rusqlite safely. |
|| `cargo clippy --all --tests -- -D warnings` | TBD | Must pass before PR. |
|| `cargo deny check` | TBD | Must pass; new dependency on `semble` CLI is external (not a Rust crate), so no cargo deny impact. |
|| No silent `.ok()` / default fallbacks | Pass | Errors from upstream tools will be mapped to typed `thiserror` variants. |
|| TDD / failing test first | Pass | Each phase starts with a failing test or fixture assertion. |
|| DRY/SRP | Pass | Each crate/plugin has one responsibility; router logic is centralized in `plugins/explore/router.lua`. |
|| No bundled credentials or cloud providers | Pass | Semble CLI is user-installed; no API keys bundled. |

## Project Structure

### Documentation (this feature)

```text
specs/009-explore-tooling-enhancement/
├── plan.md              # This file
├── research.md          # Research decisions and rationale
├── data-model.md        # Data entities and relationships
├── quickstart.md        # Validation guide
├── contracts/           # Tool schemas and API contracts
│   ├── arbor.md
│   ├── codegraph.md
│   ├── semblem.md
│   ├── index.md
│   ├── explore.md
│   └── rtk.md
├── checklists/
│   └── requirements.md
└── tasks.md             # Implementation tasks
```

### Source Code (repository root)

```text
n00n-codegraph/          # Extend: add callers, callees, impact, affected, node, query, sync commands
n00n-arbor/              # Extend: add entry-points, file-graph, inspect, path, refactor, check, summary
n00n-semble/             # Extend: add upstream CLI wrapper for search, find-related, savings
n00n-lua/src/api/        # Update: codegraph.rs, arbor.rs, semble.rs for new commands
plugins/                 # Update: explore/init.lua, explore/router.lua, arbor/init.lua, codegraph/init.lua, semblem/init.lua, bash/init.lua
n00n-agent/src/          # Update: prompt.rs (NATIVE_EFFICIENT_TOOLS), prompts/*.md
n00n-config/src/         # Update: lib.rs (tool output line budgets if needed)
```

**Structure Decision**: This is an extension feature, not a new project. All changes are additive to existing crates and plugins. No new crates are created. The structure leverages the existing separation of concerns: Rust crates for core logic, Lua plugins for tool surface, and prompts for agent behavior.

## Complexity Tracking

No constitution violations expected. All changes are additive and build on existing patterns from spec 004.

## Phased Roadmap

### Phase 1: Baseline and Research

1. Verify CodeGraph 1.5.0 compatibility: install CLI, run `codegraph --version`, test new commands on a fixture repo.
2. Capture current latency/token baselines: time `arbor map`, `codegraph explore`, `semblem search` on n00n repo; measure tool definition token sizes.
3. Run `cargo nextest run --workspace`, `cargo clippy --all --tests -- -D warnings`, `cargo deny check` to establish green baseline.
4. Run `just explore-health` to verify current tool health.
5. Document research findings in `research.md`: CodeGraph 1.5.0 features, Arbor 2.5.0 commands, Semble CLI v0.5.1 capabilities, RTK coverage.

### Phase 2: Router + Prompts

1. Extend `plugins/explore/router.lua` with new intents: `search`, `skeleton`, `symbol`, `impact`, `trace`.
2. Update `plugins/explore/init.lua` schema to include new intents and routing logic.
3. Update `n00n-agent/src/prompt.rs` `NATIVE_EFFICIENT_TOOLS` to remove "optional" qualifiers from arbor/codegraph.
4. Update `n00n-agent/src/prompts/system.md`, `general.md`, `research.md` to position explore/index/arbor/codegraph/semblem before grep/bash.
5. Update tool descriptions in `plugins/arbor/init.lua`, `plugins/codegraph/init.lua`, `plugins/semblem/init.lua` to remove external CLI installation notes.
6. Add tests for new router intents in `plugins/explore/router.lua`.

### Phase 3: CodeGraph Expansion

1. Update `n00n-codegraph/Cargo.toml` to target CodeGraph 1.5.0 (no Rust dependency change, just CLI version expectation).
2. Extend `n00n-codegraph/src/lib.rs` with new commands: `callers`, `callees`, `impact`, `affected`, `files`, `node`, `query`, `sync`.
3. Extend `n00n-codegraph/src/db.rs` with native SQLite queries for new commands where possible.
4. Add Lua API functions in `n00n-lua/src/api/codegraph.rs` for new commands.
5. Update `plugins/codegraph/init.lua` to expose new commands through the tool schema.
6. Add tests for new CodeGraph commands in `n00n-codegraph/src/lib.rs`.

### Phase 4: Arbor Expansion

1. Extend `n00n-arbor/src/lib.rs` with new CLI commands: `entry-points`, `file-graph`, `inspect`, `path`, `refactor`, `check`, `summary`, `trace`.
2. Improve in-memory `GraphIndex` fallbacks for existing commands (callers, callees, trace).
3. Add Lua API functions in `n00n-lua/src/api/arbor.rs` for new commands.
4. Update `plugins/arbor/init.lua` to expose new commands through the tool schema.
5. Add tests for new Arbor commands in `n00n-arbor/src/lib.rs`.

### Phase 5: Semblem Hybrid

1. Add upstream `semble` CLI wrapper in `n00n-semble/src/lib.rs` for `search`, `find-related`, and `savings`.
2. Support `--content docs/config/all` flags and remote git URLs in the wrapper.
3. Update `plugins/semblem/init.lua` to call upstream CLI when available, fall back to native BM25.
4. Keep existing embedder nag logic for hybrid/semantic modes without embedder.
5. Add tests for upstream CLI wrapper and BM25 fallback in `n00n-semble/src/lib.rs`.

### Phase 6: RTK Hardening

1. Update `plugins/bash/init.lua` to cache rtk availability per session (store in session-local variable).
2. Broaden rtk rewrite coverage: add more command patterns (e.g., `docker`, `npm`, `pip` if supported by rtk).
3. Ensure `jq` and `yq` pass through unchanged (already implemented, verify).
4. Update prompt hints in `plugins/bash/init.lua` to explicitly recommend rtk-wrapped bash.
5. Add tests for rtk availability caching in `plugins/bash/init.lua`.

### Phase 7: Docs + Config

1. Update `AGENTS.md` token-efficient section to reflect new tool hierarchy.
2. Update `n00n-config/src/lib.rs` tool output line budgets if needed (consider per-tool budgets).
3. Regenerate site docs with `just gen-docs`.
4. Update `quickstart.md` with validation commands for new features.

### Phase 8: Verification

1. Run full test suite: `cargo nextest run --workspace`, `cargo clippy --all --tests -- -D warnings`, `cargo deny check`.
2. Run `just explore-health` to verify all tools are healthy.
3. Manual smoke tests: test explore router with various intents, test new CodeGraph/Arbor/Semble commands, test RTK rewriting.
4. Measure tool definition token sizes and tool call latencies against baseline.
5. Draft PR with performance comparison.

## Quick Validation Guide

See [quickstart.md](quickstart.md) for end-to-end validation steps.
