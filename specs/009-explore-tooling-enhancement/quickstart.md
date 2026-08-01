# Quickstart: Explore Tooling Enhancement

**Feature**: Explore Tooling Enhancement  
**Date**: 2026-08-01

## Prerequisites

- Rust toolchain matching `rust-version` in `Cargo.toml`.
- CodeGraph 1.5.0 CLI (optional, for full feature set).
- Arbor 2.5.0 CLI (optional, for full feature set).
- Semble 0.5.1 CLI (optional, for remote URLs and content filters).
- RTK CLI (optional, for bash command rewriting).
- A fixture repository with supported source files (e.g., n00n itself).

## Build and Test

```bash
cargo build --release --bin n00n
cargo nextest run --workspace
cargo clippy --all --tests -- -D warnings
cargo deny check
just explore-health
```

## Validate Explore Router

```bash
# File intent (should route to index)
explore { query = "src/main.rs", intent = "auto" }

# Relations intent (should route to arbor)
explore { query = "callers of parse_file", intent = "auto" }

# Cross-file intent (should route to codegraph)
explore { query = "how does tool dispatch work", intent = "auto" }

# Search intent (should route to semblem)
explore { query = "agent loop", intent = "search" }
```

Expected: Each query routes to the correct backend and returns appropriate results.

## Validate CodeGraph 1.5.0

```bash
# Callers via native SQLite
codegraph { command = "callers", symbol = "parse_file", project = "." }

# Sync to re-index
codegraph { command = "sync", project = "." }

# Node details
codegraph { command = "node", node_id = "123", project = "." }

# Impact analysis
codegraph { command = "impact", symbol = "parse_file", project = "." }
```

Expected: Results return from native SQLite queries where supported; CLI fallback for unsupported commands.

## Validate Arbor Expansion

```bash
# Entry points
arbor { command = "entry-points", project = "." }

# File graph
arbor { command = "file-graph", path = "src/main.rs", project = "." }

# Inspect
arbor { command = "inspect", symbol = "parse_file", project = "." }

# Check
arbor { command = "check", project = "." }
```

Expected: Results return from Arbor CLI; native in-memory fallback for callers/callees/trace_path when CLI unavailable.

## Validate Semblem Hybrid

### With upstream CLI

```bash
# Search with remote URL
semblem { command = "search", repo = "https://github.com/user/repo", query = "auth" }

# Search with content filter
semblem { command = "search", repo = ".", query = "config", content = "config" }

# Find related
semblem { command = "find_related", repo = ".", file_path = "src/auth.rs", line = 42 }
```

Expected: Results return from upstream Semble CLI with requested features.

### Without upstream CLI (BM25 fallback)

```bash
# BM25 search
semblem { command = "search", repo = ".", query = "agent loop", mode = "bm25" }

# Hybrid mode with nag
semblem { command = "search", repo = ".", query = "agent loop", mode = "hybrid" }
```

Expected: BM25 results returned; hybrid mode nags with vLLM presets and falls back to BM25.

## Validate RTK Hardening

```bash
# Git command (should be rewritten)
bash { command = "git status" }

# Cargo command (should be rewritten)
bash { command = "cargo test" }

# jq (should pass through unchanged)
bash { command = "jq '.foo' file.json" }

# yq (should pass through unchanged)
bash { command = "yq '.foo' file.yaml" }
```

Expected: Git and cargo commands are rewritten through rtk; jq and yq pass through unchanged; rtk availability is cached per session.

## Validate Prompts

```bash
# Check NATIVE_EFFICIENT_TOOLS in n00n-agent/src/prompt.rs
# Should include: explore, index, arbor, codegraph, semblem (without "optional")

# Check prompt hints in n00n-agent/src/prompts/*.md
# Should recommend: explore first, then index, then arbor/codegraph/semblem, then read, then grep/bash
```

Expected: Tools are positioned as first-tier exploration tools without optional qualifiers.

## Regression Tests

```bash
# Measure tool definition token sizes
# Should not exceed baseline

# Measure tool call latencies
# Should not exceed baseline

# Run full test suite
cargo nextest run --workspace
cargo clippy --all --tests -- -D warnings
cargo deny check
```

Expected: All tests pass; token sizes and latencies within baseline.

## Smoke Test Checklist

- [ ] Explore router routes correctly for all intents
- [ ] CodeGraph new commands return correct results
- [ ] Arbor new commands return correct results
- [ ] Semblem upstream CLI works with remote URLs and content filters
- [ ] Semblem BM25 fallback works without upstream CLI
- [ ] RTK rewrites commands and caches availability
- [ ] jq/yq pass through unchanged
- [ ] Prompts position tools as first-tier
- [ ] Tool definitions do not increase token count
- [ ] Tool call latencies do not regress
