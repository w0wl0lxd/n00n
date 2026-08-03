# Audit and expand other ad-hoc CLI/API native tool opportunities — Research

## Summary

Based on audit of n00n's codebase, AGENTS.md, and existing native tool patterns, the highest-leverage ad-hoc CLI/API candidates for native tools are **cargo** (build/test/lint), **just** (task runner), and **docs** (generation/preview). These commands appear repeatedly in workflows, incur token costs from CLI output parsing, and have clear structured-output alternatives.

## Evidence

- **AGENTS.md** — documents standard cargo patterns: `cargo test`, `cargo clippy --all --tests -- -D warnings`, `cargo nextest run --workspace` (lines 10-17)
- **bash plugin RTK coverage** — auto-rewrites `cargo`, `docker`, `npm`, `pip`, `python`, `gh` through rtk but still requires CLI construction (`plugins/bash/init.lua:617`)
- **justfile** — defines `test`, `gen-docs`, `build` recipes used across CI and specs
- **n00n-docgen** — existing Rust crate for doc generation invoked via `just gen-docs`
- **DEFAULT_BUILTINS** — current native tools list in `n00n-config/src/lib.rs:60-84`
- **BUNDLED_PLUGINS** — Lua plugin registration pattern (`n00n-lua/src/loader.rs:29-74`)
- **Epic #240** — tracks tmux, github/gix, research, review tools as in-progress; issue #239 is the audit for remaining patterns

## Map

**Entry points:**
- AGENTS.md (root) — canonical agent behavior and CLI patterns
- plugins/bash/init.lua — RTK auto-rewrite logic and command safety checks
- n00n-config/src/lib.rs — DEFAULT_BUILTINS list
- n00n-lua/src/loader.rs — BUNDLED_PLUGINS registration

**Key symbols/files:**
- `n00n.api.register_tool` — Lua plugin registration pattern (30+ occurrences in plugins/)
- `rtk_rewrite()` — bash plugin function for command compression (`plugins/bash/init.lua:412`)
- `n00n-arbor`, `n00n-codegraph`, `n00n-search` — existing native Rust crates for code intelligence
- justfile — task runner recipes

**Call/data flow:**
- Agent → Lua plugin → `n00n.api.register_tool` → tool schema → handler → Rust API (`n00n-lua/src/api/mod.rs`) → external CLI or native crate

## Candidate Tool Table

| Tool | Current ad-hoc call | Frequency | Token cost | Feasibility | Priority |
|------|-------------------|-----------|------------|-------------|----------|
| **cargo** | `cargo test`, `cargo clippy`, `cargo nextest run` | High | High | Rust crate (`cargo_metadata`) or pure Lua with JSON parsing | **High** |
| **just** | `just test`, `just gen-docs`, `just build` | Medium | Medium | Pure Lua (parse justfile, exec tasks) | **Medium** |
| **docs** | `just gen-docs` (`n00n-docgen`) | Medium | Low | Wrap existing `n00n-docgen` crate | **Medium** |
| **docker/podman** | `docker build`, `podman run` | Low | High | Rust crate (`bollard`/`podman`) or bash wrapper | Low |
| **nix** | `nix develop`, `nix build` | Low | Medium | Rust crate (`nix-rs`) or bash wrapper | Low |
| **ssh/remote** | Not found in codebase | None | N/A | Rust crate (`ssh2`) | Low |
| **npm/pip** | Not found in codebase | None | N/A | Rust crate or bash wrapper | Low |
| **browser/playwright** | Not found in codebase | None | N/A | Rust crate (`headless-chrome`) | Low |

## Top 3-5 Recommended Next Tools

### 1. cargo (native Rust crate)
**Design:** Create `n00n-cargo` crate using `cargo_metadata` for structured queries (workspace members, dependencies) and `cargo-nextest` JSON output for test results. Expose `n00n.cargo` API in n00n-lua with methods: `test(package_filter)`, `check()`, `clippy()`, `build()`. Tool schema accepts `package`, `features`, `filter` params. Returns structured JSON with pass/fail counts, error locations, and warnings. Eliminates ~60-90% token cost from parsing verbose cargo output via RTK.

### 2. just (pure Lua plugin)
**Design:** Add `plugins/just/init.lua` that parses justfile (using existing tree-sitter-bash grammar or simple line parser) and exposes tasks as structured options. Tool schema: `task` (required), `args` (optional). Handler executes via `n00n.fn.jobstart` with RTK auto-rewrite. Returns structured output with exit code and filtered stdout.

### 3. docs (wrap n00n-docgen)
**Design:** Add `plugins/docs/init.lua` wrapping the existing `n00n-docgen` binary. Tool schema: `command` (enum: `generate`, `preview`, `check`). Handler calls `cargo run -p n00n-docgen` with appropriate args. Returns structured output with changed file count and validation errors.

### 4. docker/podman (deferred)
**Design:** If demand emerges, create `n00n-container` crate using `bollard` (Docker) or `podman` REST API. Tool schema: `command` (enum: `build`, `run`, `ps`, `exec`), `image`, `args`. Returns structured JSON for container status and logs. Low priority due to minimal usage in n00n workflows.

### 5. github/gix (already tracked in #236)
**Design:** In-progress per epic #240. Using embedded `gix`/`gitoxide` for repository operations. Not included in this audit's top recommendations as it is already scoped.

## Risks and Open Questions

- **cargo_metadata limitations:** May not support all cargo commands (e.g., nextest-specific flags). Fallback to bash with RTK for unsupported commands.
- **justfile parsing complexity:** Justfiles can have complex recipes, variables, and dependencies. Initial implementation should support simple recipes first.
- **Token savings estimation:** Need actual session data to quantify token savings for cargo/just tools. Current estimates based on RTK's 60-90% compression claims.
- **Dependency bloat:** Adding `cargo_metadata`, `bollard`, or other Rust crates increases binary size. Feature-gate optional tools.
- **Platform differences:** just, cargo, docker behave differently across platforms. Native tools must handle Windows/Unix differences gracefully.
