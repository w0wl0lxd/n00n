# Implementation Plan: Native Explore Tools

**Branch**: `004-native-explore-tools` | **Date**: 2026-07-27 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/004-native-explore-tools/spec.md`

---

## Summary

This feature ports the `arbor`, `codegraph`, and `semblem` code-intelligence tools from external CLI/MCP dependencies to in-process Rust libraries exposed through n00n's existing Lua plugin layer. The work reuses upstream crates where possible, preserves existing tool schemas and output contracts, and defaults `semblem` to BM25-only to avoid runtime model downloads.

---

## Technical Context

**Language/Version**: Rust 2024 edition (workspace `rust-version = 1.97`).

**Primary Dependencies**:
- `arbor-core` + `arbor-graph` (Arbor parsing and graph queries).
- `cgz` / `codegraph` crate, or `rusqlite` fallback, for CodeGraph index access.
- `sonar-core` (Rust translation of Semble) for `semblem` search.

**Storage**: Existing project-side indexes remain under `.arbor/`, `.codegraph/`, and `semblem` cache directories. No new persistent storage in n00n itself.

**Testing**: `cargo test -p n00n-lua`, `cargo test -p n00n-agent`, `cargo clippy --all --tests -- -D warnings`, `cargo deny check`.

**Target Platform**: Linux primary; macOS secondary. All chosen crates are pure Rust or use widely supported native parsers.

**Project Type**: CLI/TUI agent with built-in Lua plugins.

**Performance Goals**: Tool call latency for `arbor` and `codegraph` on the n00n repo must not exceed the current external-CLI baseline.

**Constraints**:
- `unsafe_code = "deny"` workspace-wide; no new `unsafe` blocks without review.
- `unwrap_used` and `expect_used` denied in production code.
- New dependencies must pass `cargo deny check` and be added to workspace `Cargo.toml` first.
- Tool definitions must shrink or stay flat in token size after removing CLI-installation notes.

**Scale/Scope**: Three built-in tools, one new crate (`n00n-semble`), two rewritten crates (`n00n-arbor`, `n00n-codegraph`), and three Lua plugins.

---

## Constitution Check

*The project constitution is defined in `AGENTS.md`. The following gates apply before implementation:*

| Gate | Status | Notes |
|------|--------|-------|
| No new `unsafe` without review | Pass | The chosen crates do not require new `unsafe` blocks in n00n wrapper code. |
| `cargo clippy --all --tests -- -D warnings` | TBD | Must pass before PR. |
| `cargo deny check` | TBD | Must pass; dependency licenses are MIT. |
| No silent `.ok()` / default fallbacks | Pass | Errors from upstream crates will be mapped to typed `thiserror` variants. |
| TDD / failing test first | Pass | Each phase starts with a failing test or fixture assertion. |
| DRY/SRP | Pass | Each crate has one responsibility: parsing/indexing, query API, or Lua binding. |

---

## Project Structure

### Documentation (this feature)

```text
specs/004-native-explore-tools/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── spec.md              # User-facing specification
├── data-model.md        # Entity and schema contracts
├── quickstart.md        # Validation guide
├── contracts/           # Tool schemas and API contracts
│   ├── arbor.md
│   ├── codegraph.md
│   └── semble.md
└── checklists/
    └── requirements.md
```

### Source Code (repository root)

```text
n00n-arbor/              # Rewritten: in-process Arbor graph client
n00n-codegraph/          # New: in-process CodeGraph index client
n00n-semble/             # New: in-process Semble search client
n00n-lua/src/api/        # Add arbor.rs updates, codegraph.rs, semble.rs
plugins/                 # Update arbor, codegraph; add semble
n00n-agent/src/prompt.rs # Update NATIVE_EFFICIENT_TOOLS list
n00n-config/src/lib.rs  # Update DEFAULT_BUILTINS list
n00n-lua/src/loader.rs  # Update BUNDLED_PLUGINS list
Cargo.toml              # Workspace dependencies
```

**Structure Decision**: Keep one Rust crate per tool for clarity and optional feature flags. Lua plugins remain the tool surface so existing prompt and permission systems continue to work.

---

## Complexity Tracking

No constitution violations expected. If binary size grows significantly, we will add Cargo feature flags (`arbor`, `codegraph`, `semblem`) to let users disable individual tools.

---

## Phased Roadmap

### Phase 0: Validation Spikes

1. Add `arbor-core`/`arbor-graph` to a throwaway `n00n-arbor` branch and prove `GraphStore::load_graph` can read the `.arbor/` index built by the CLI.
2. Add `sonar-core` to a throwaway crate and verify `SonarIndex::from_path_cached` on the n00n repo.
3. Add `cgz` and verify it can open the `.codegraph/codegraph.db` produced by `codegraph` CLI 1.4.1. If not, switch to the `rusqlite` fallback.

### Phase 1: Design & Contracts

1. Define `data-model.md` for `ArborGraph`, `CodeGraphIndex`, `SembleIndex`, and `ToolOutputCard`.
2. Define tool schemas and output contracts in `contracts/`.
3. Finalize crate APIs and `n00n.<tool>` Lua tables.

### Phase 2: Arbor Native

1. Rewrite `n00n-arbor` to use `arbor-graph` + `arbor-core`.
2. Update `n00n-lua/src/api/arbor.rs` and `plugins/arbor/init.lua`.
3. Add fixture tests for `callers`, `callees`, `map`, `diff`, `query`, `status`.
4. Run `cargo check` and `cargo clippy`.

### Phase 3: Semble Native

1. Create `n00n-semble` wrapping `sonar-core`.
2. Add `n00n-lua/src/api/semblem.rs` and `plugins/semblem/init.lua`.
3. Add `semblem` to `BUNDLED_PLUGINS`, `DEFAULT_BUILTINS`, and `NATIVE_EFFICIENT_TOOLS`.
4. Default to BM25-only; gate semantic mode behind config.
5. Add tests for `search` and `find_related`.

### Phase 4: CodeGraph Native

1. Create `n00n-codegraph` using `cgz` or a `rusqlite` query layer.
2. Add `n00n-lua/src/api/codegraph.rs` and rewrite `plugins/codegraph/init.lua`.
3. Implement `explore`, `callers`, `callees`, `impact`, `status`, `ensure_indexed`.
4. Validate output matches the existing `codegraph explore` format.

### Phase 5: Integration & Rollout

1. Update tool descriptions to remove external CLI installation notes.
2. Run `cargo nextest run --workspace`, `cargo clippy --all --tests -- -D warnings`, `cargo deny check`.
3. Measure tool-definition token size and tool call latency against baseline.
4. Draft PR with performance comparison.

---

## Quick Validation Guide

See [quickstart.md](quickstart.md) for end-to-end validation steps.
