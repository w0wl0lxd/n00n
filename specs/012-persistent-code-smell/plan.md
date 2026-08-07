# Implementation Plan: Persistent code-smell and comment index

**Branch**: `012-persistent-code-smell` | **Date**: 2026-08-03 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/012-persistent-code-smell/spec.md`

## Summary

Build a dedicated Tantivy-backed index for conflict markers and code smells. The index lives in `.n00n/smells`, is populated by `n00n-smell index <repo>`, and is searchable via `n00n-smell search <repo> <query>`. It is also exposed to the Lua plugin system as `n00n.smell` and a built-in `smell` tool, following the same pattern as `n00n-semble`.

## Technical Context

- **Language/Version**: Rust 2024, rustc 1.97+
- **Primary Dependencies**: `tantivy` 0.26.1, `n00n-git` (for conflicts scanner), `n00n-search` (for index conventions), `serde`, `thiserror`, `clap`
- **Storage**: Tantivy index in `.n00n/smells/tantivy_index` plus `metadata.json`
- **Testing**: `cargo nextest run --workspace`, unit tests inside `n00n-smell/src/lib.rs`
- **Target Platform**: Linux (n00n primary)
- **Project Type**: Rust workspace library + CLI + Lua plugin
- **Performance Goals**: Index 100k-line repo under 10s, search under 500ms
- **Constraints**: Must pass `cargo clippy --all --tests -- -D warnings`; no `unsafe`; no new wildcard imports
- **Scale/Scope**: Local repository smell index; no distributed or remote support for v1

## Constitution Check

- No new `unsafe` blocks.
- All fallible paths return `Result` with typed errors.
- Tests are written first/parallel to implementation.

## Project Structure

### Documentation (this feature)

```text
specs/012-persistent-code-smell/
├── spec.md
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
└── tasks.md
```

### Source Code (repository root)

```text
n00n-smell/
├── Cargo.toml
└── src/
    ├── lib.rs          # SmellIndex, SmellFinding, search/index API
    ├── main.rs         # CLI: index, search
    └── error.rs        # SmellError

n00n-lua/src/api/
├── smell.rs            # n00n.smell Lua bindings
├── mod.rs              # register n00n.smell

plugins/smell/
├── init.lua            # built-in smell tool

Cargo.toml              # workspace member + n00n-smell workspace dep

site/docs/content/tools/_index.md  # generated docs updated by just gen-docs
n00n-token-profile/baselines/cold_start.json  # regenerated if tool count changes
```

## Complexity Tracking

No complexity violations; the new crate mirrors `n00n-semble` patterns and stays focused on one responsibility.
