# Quickstart: Native Explore Tools

**Feature**: Native Explore Tools  
**Date**: 2026-07-27

## Prerequisites

- Rust toolchain matching `rust-version` in `Cargo.toml`.
- No `arbor`, `codegraph`, or `semblem` binaries on `PATH`.
- A fixture repository with supported source files (e.g., `n00n` itself).

## Build and Test

```bash
cargo build --release --bin n00n
cargo nextest run --workspace
cargo clippy --all --tests -- -D warnings
cargo deny check
```

## Validate `arbor`

```bash
# Run n00n and invoke:
# arbor { command = "callers", symbol = "parse_file", project = "." }
# arbor { command = "map", project = ".", token_budget = 1024 }
```

Expected: results return without an external `arbor` process.

## Validate `codegraph`

```bash
# Ensure .codegraph/ exists (run codegraph init if needed before migration, then build native n00n).
# codegraph { query = "how does tool dispatch work", projectPath = "." }
```

Expected: grouped source snippets and blast-radius summary without the `codegraph` CLI.

## Validate `semblem`

```bash
# semblem { command = "search", repo = ".", query = "agent loop" }
```

Expected: ranked code snippets. By default, results use BM25 search.

## Regression Tests

```bash
# No external binaries on PATH
unset PATH; export PATH="/usr/bin:/bin"
cargo test -p n00n-lua
cargo test -p n00n-agent
```

All tests MUST pass.

## Performance Check

```bash
# Compare tool call latency and token size against baseline
cargo run --bin dynamic_tool_size -- --tools arbor,codegraph,semblem
```

Acceptance: latency within 10% of CLI baseline and tool definitions use no more tokens than the current versions.
