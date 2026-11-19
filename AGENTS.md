# n00n Agent Guide

n00n is an AI coding agent (like Claude Code and opencode), built bottom up to optimize costs and number of tokens used, while not sacrificing performance too much.

This file is the canonical source of truth for agent behavior in this repo. Prefer it over generic Rust advice.

## Build, Lint, and Test

```bash
cargo fmt --all
cargo check --all
cargo clippy --all --tests -- -D warnings
cargo nextest run --workspace
```

Run `n00n-providers` tests single-threaded if the dynamic script-discovery tests flake: `RUST_TEST_THREADS=1 cargo test -p n00n-providers`.
Read `justfile` for more recipes.

## Rust Hard Gates

The workspace lint configuration lives in the root `Cargo.toml` `[workspace.lints]` and `clippy.toml`; every crate opts in via `[lints] workspace = true`.

- `unsafe_code = "deny"` workspace-wide. Existing unsafe sits in `n00n-lua` (Luau runtime), `n00n-agent` and `n00n-ui` (process control), `n00n-config` (`.env` loading), `n00n-providers`, `n00n-storage`, and the root binary (self-updater in `src/update.rs`, process control in `src/cmd/agent.rs`). Find it with `rg 'allow\(unsafe_code\)' --glob '*.rs'`.
- New unsafe, FFI, global mutable state, `static mut`, or unchecked transmute-like behavior needs a written review. Give each unsafe use its own `#[allow(unsafe_code)]` and a SAFETY comment. `n00n-lua` is the only package with crate-level `#![allow(unsafe_code)]` (its `src/lib.rs` and `benches/luau_perf.rs` targets; it isolates the Luau FFI boundary). Do not add a crate-level exception elsewhere: it silences the lint for every later unsafe block in that crate.
- `unwrap_used`, `expect_used`, and `panic` are denied in production code. Tests are exempt via `clippy.toml`.
- `todo!`, `unimplemented!`, and `dbg!` macros are denied.
- `unwrap_or` and `unwrap_or_default` on `Option` and `Result` are denied through `clippy.toml` `disallowed-methods`. Clippy does not flag `.ok()` on `Result`, so code review enforces that rule. See Error Handling.
- `pedantic` clippy lints are warned and must pass under `cargo clippy --all --tests -- -D warnings` once the existing codebase is cleaned up.
- No wildcard imports.

## Code Style

- No trivial comments.
- No unnecessary state (variables, fields, arguments).
- Follow Rust idioms and best practices; latest Rust features can be used.
- Import types at top of file and use short names everywhere (e.g. `use std::sync::Arc;` then `Arc<T>`, never `std::sync::Arc<T>` inline).
- Keep consts at top of file, right after imports.
- No inline magic numbers or strings.
- `#[derive(Copy)]` only on structs with one primitive field.
- Prefer structured logging with wide, useful fields.

## Testing

- Use TDD: failing test, implementation, refactor.
- Place unit tests in the same file using `#[cfg(test)]` modules.
- Use `#[test_case]` and snake_case test names.
- No bullshit tests (e.g. tautology).
- No flaky tests (no weird sleeps).
- In tests, const error/status messages and assert against shared constants.

## Error Handling

- Propagate typed errors with `?`, `ok_or_else`, and `map_err`.
- Library crates use `thiserror`; binaries use `color-eyre`.
- Missing data, parse failures, dependency errors, closed channels, and invalid state must not be silently defaulted away, and `.ok()` must not discard a `Result` error. Return a typed error, reject the operation with a sanitized log, or use an explicitly named fallback that emits sanitized structured logs.

## Dependencies and Supply Chain

- Add new dependencies to the workspace `Cargo.toml` first, then `workspace = true` in the crate.
- Try solving with existing dependencies before adding new ones.
- For a new dependency, state its purpose, check maintenance and license compatibility, and run `cargo deny check`. Prefer versions published at least 7 days ago. Do not use floating ranges (`latest`, `*`, unbounded `>=`).
- Prefer well-maintained crates from crates.io.
- Disable default features when they pull unused networking, TLS, compression, native, proc-macro, or runtime surface.

## Trust Boundaries and Security

- Treat LLM and provider output as untrusted input. Validate against schemas, domain constraints, and source evidence before persistence or action.
- Do not commit production or private credentials, API keys, tokens, cookies, auth headers, or user data. The gitleaks hook (`hk.pkl`) and the CI secrets job catch some leaks, but they do not replace this rule.
- Do not log raw provider payloads, prompts, credentials, or user session data.
- Validate and authorize HTTP, file, queue, config/env, LLM output, and provider callbacks before mutation or persistence.
- Apply prompt-injection defenses where documents, user text, or provider messages are included in prompts or tool calls.
- Tool execution requires allowlisted tools, scoped credentials, explicit user context, audit events, and refusal/denial tests.

## Worktrees, Commits, and Attribution

- Do non-trivial or multi-file changes in a dedicated git worktree on a new branch, so other work in the checkout stays intact.
- Do not revert, delete, reformat, stage, or commit unrelated user changes.
- Commit with a Conventional Commit message, push the branch, and open a PR. Do not force-push or push to `main`: shared history stays reviewable and recoverable.
- Never add AI-agent attribution to commits, PRs, changelogs, or authored content.

## Token-efficient exploration

Every tool result spends context tokens. Prefer structural, pre-indexed tools over broad searches and unfiltered file reads.

- **Explore first:** use `explore_code` for codebase questions; it routes to `index_file` (single-file skeleton, priority for file paths), `map_codegraph` (callers/callees, impact, cross-file structure), or `search_text` (keyword/semantic search). The router auto-detects intent from query patterns.
- **Cross-file structure:** use `map_codegraph` for end-to-end system questions, caller/callee relationships, call paths, and blast-radius impact checks (requires a `.codegraph/` index).
- **Single-file structure next:** use `index_file` to get a compact skeleton and exact line ranges before reading any source file.
- **Then targeted reads:** use `read_file` with `offset`/`limit` for the specific section you need.
- **Use `search_code` as a fallback:** for literal string matching or when no index is available, not as the default exploration tool.
- **Parallelize:** use `run_batch` for independent calls.
- **Filter and pipeline with `run_python`:** chain calls, filter large outputs, and transform data inside the sandboxed Python interpreter. Only the final result should reach the context window.
- **AST-aware search (`ast-grep`):** if you configure the `ast-grep` MCP server, use it for structural search and safe refactoring. It is more precise than `search_code` and safer than `sed`. Configure it in `.n00n/mcp.toml` as `[mcp.ast-grep]` with a `command` array and a `timeout` in milliseconds.
- **Compress structured data:** prefer `n00n.json.tooned` (lossless JSON/TOON passthrough) over plain JSON when passing structured data between tools or scripts.

`run_shell` rewrites supported commands through `rtk` when the `rtk` CLI is installed, which cuts output tokens. Use `rtk proxy <command>` when you need the raw output.

Do not use `run_shell` for file writes, moves, deletes, or broad destructive operations. Use `edit_file`/`edit_file_bulk`/`write_file` for those.

## Verification

- Before you fix an unfamiliar failure mode, third-party tool, library or API behavior, or CI issue, read its documented behavior first.
- Report real command results. Separate failures that already exist on the base branch from regressions in the code you touched.

## Architecture

Rust workspace, key crates in `crates/`:

- n00n-ui: ratatui interactive UI (elm-like architecture)
- n00n-providers: LLM provider integrations
- n00n-agent: async agent loop on smol
- n00n-interpreter: code_execution tool using pydantic/monty
- n00n-storage: persistent state
- n00n-config: user config
- n00n-lua: Lua plugin system with built-in plugins in ./plugins
- n00n-acp: ACP ndjson stdio server

Each built-in Lua plugin has its own directory in `./plugins` (for example `explore`, `bash`, `edit`, `team`, `codegraph`).

## Docs

Homepage: ./site/. User docs: ./site/docs/. Most pages are generated by `crates/n00n-docgen/src/gen_*.rs`; run `just gen-docs` to regenerate.
Tone: warm, simple, concise, easy for non-native English, story telling. No em-dashes, no emojis, no AI tone.
