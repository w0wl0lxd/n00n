# Quickstart: Native Explore Tools

**Feature**: Native Explore Tools  
**Date**: 2026-07-27

## Prerequisites

- Rust toolchain matching `rust-version` in `Cargo.toml`.
- No `arbor`, `codegraph`, or `semblem` binaries on `PATH`.
- A fixture repository with supported source files (e.g., `n00n` itself).
- (Optional) Podman and a GPU with `nvidia-container-toolkit` to run vLLM presets.

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

Expected: results return without an external `arbor` process and indexing shows a moving progress indicator.

## Validate `codegraph`

```bash
# codegraph { query = "how does tool dispatch work", projectPath = "." }
```

Expected: grouped source snippets and blast-radius summary without the `codegraph` CLI.

## Validate `semblem`

### BM25-only (default)

```bash
# semblem { command = "search", repo = ".", query = "agent loop" }
```

Expected: ranked code snippets returned without any network call or model download.

### Hybrid/semantic without configured embedder

```bash
# semblem { command = "search", repo = ".", query = "agent loop", mode = "hybrid" }
```

Expected: a nag message listing vLLM Podman presets and a remote OpenAI-compatible option, followed by BM25 results.

### With local vLLM

1. Generate a preset command:

```bash
# semblem { command = "vllm_setup", preset = "light" }
```

2. Run the generated `podman run ...` command in another terminal.
3. Configure n00n to use `http://localhost:8000/v1/embeddings` with the preset model.
4. Run `semblem` with `mode = "hybrid"`.

Expected: hybrid BM25 + semantic results.

## Regression Tests

```bash
# No external binaries on PATH
unset PATH; export PATH="/usr/bin:/bin"
cargo test -p n00n-lua
cargo test -p n00n-agent
```

All tests MUST pass.

## Feature Flag Variants

```bash
# Minimal build without vLLM/static embed features
cargo build --release --bin n00n --no-default-features --features arbor,codegraph,semblem

# Full build with optional embedders
cargo build --release --bin n00n --features vllm,static-embed
```

## Performance Check

```bash
# Compare tool call latency and token size against baseline
cargo run --bin dynamic_tool_size -- --tools arbor,codegraph,semblem
```

Acceptance: latency within 10% of CLI baseline and tool definitions use no more tokens than the current versions.
