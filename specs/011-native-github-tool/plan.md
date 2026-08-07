# Implementation Plan: Native GitHub Tool Using gix/gitoxide

**Branch**: `011-native-github-tool` | **Date**: 2026-08-03 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/011-native-github-tool/spec.md`

## Summary

Add native `git` and `github` tools to n00n using the `gix/gitoxide` crate for local git operations and `reqwest` for GitHub REST API access. This replaces shell-based `git` and `gh` commands with structured, typed access. The implementation creates a new `n00n-git` crate, exposes Lua API bindings in `n00n-lua/src/api/git.rs` and `n00n-lua/src/api/github.rs`, registers plugins in `plugins/git/` and `plugins/github/`, updates workspace Cargo.toml, adds permission scopes, and includes tests with temporary git repositories.

## Technical Context

**Language/Version**: Rust 2024 Edition, rustc 1.97+

**Primary Dependencies**: gix (0.86+), reqwest (workspace), thiserror, serde, serde_json, mlua

**Storage**: Git repositories (local), GitHub REST API (remote)

**Testing**: cargo test, cargo nextest, tempfile for temporary repos

**Target Platform**: Linux, macOS, WASM (where n00n runs)

**Project Type**: Rust workspace with native crates and Lua plugins

**Performance Goals**: Git operations no slower than CLI baseline; GitHub API calls with rate limit handling

**Constraints**: No unsafe code (workspace-wide deny), structured error handling, permission-scoped operations

**Scale/Scope**: Two new tools (git, github), one new crate (n00n-git), two Lua API modules, two plugins

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

- **DRY**: Git and GitHub tools are separate concerns with different permission scopes and APIs; no duplication expected.
- **SRP**: Each crate has a single responsibility (n00n-git for git operations, n00n-lua API modules for Lua bindings, plugins for tool registration).
- **KISS**: Implementation follows existing patterns (arbor, codegraph) with minimal complexity.
- **YAGNI**: No speculative hooks or unused parameters; only required operations are implemented.
- **OCP**: Permission scopes and tool registration follow existing patterns; no core module modifications expected.

**Status**: Pass

## Project Structure

### Documentation (this feature)

```text
specs/011-native-github-tool/
├── spec.md              # Feature specification
├── research.md          # Research findings (already exists)
├── plan.md              # This file
└── tasks.md             # Implementation tasks
```

### Source Code (repository root)

```text
n00n-git/                    # New crate for git operations
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── error.rs
│   ├── status.rs
│   ├── log.rs
│   ├── diff.rs
│   ├── branches.rs
│   ├── blame.rs
│   └── write.rs

n00n-lua/
├── src/
│   └── api/
│       ├── mod.rs          # Update to register git and github tables
│       ├── git.rs          # New: Lua bindings for git operations
│       └── github.rs       # New: Lua bindings for GitHub operations

plugins/
├── git/
│   └── init.lua            # New: git tool registration
└── github/
    └── init.lua            # New: github tool registration

n00n-config/
├── src/
│   └── lib.rs              # Update DEFAULT_BUILTINS and permission scopes

Cargo.toml                   # Update workspace members and dependencies
```

**Structure Decision**: Follow existing n00n patterns (arbor, codegraph) with a dedicated crate for core logic, Lua API modules for bindings, and plugins for tool registration. Git and GitHub are separate tools with separate permission scopes, following the research recommendation.

## Complexity Tracking

> No constitution violations; complexity tracking not required.
