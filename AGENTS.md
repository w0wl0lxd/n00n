# N00n Agent Guide

N00n is an AI coding agent (like Claude Code and opencode), built bottom up to optimize costs and number of tokens used, while not sacrificing performance too much.

This file is the canonical source of truth for agent behavior in this repo. Prefer it over generic Rust advice.

## Build, Lint, and Test

```bash
cargo fmt --all
cargo check --all
cargo clippy --all --tests -- -D warnings
cargo nextest run --workspace
```

Use `cargo test --workspace` if `cargo-nextest` is not installed.
Run `n00n-providers` tests single-threaded if the dynamic script-discovery tests flake: `RUST_TEST_THREADS=1 cargo test -p n00n-providers`.
Read `justfile` for more recipes.

## Rust Hard Gates

The workspace lint configuration is the law. It lives in the root `Cargo.toml` `[workspace.lints]` and `clippy.toml`; every crate opts in via `[lints] workspace = true`.

- `unsafe_code = "deny"` workspace-wide. Existing unsafe is limited to the Luau runtime in `n00n-lua`, process control in `n00n-agent`/`n00n-ui`, `.env` loading in `n00n-config`, and the self-updater in the root binary. Each call carries an explicit `#[allow(unsafe_code)]` and a SAFETY comment. New unsafe requires a written review.
- `unwrap_used`, `expect_used`, and `panic` are denied in production code. Tests are exempt via `clippy.toml`.
- `todo!`, `unimplemented!`, and `dbg!` macros are denied.
- `unwrap_or`, `unwrap_or_default`, `.ok()` on `Result`, and other silent-default patterns are denied. Missing data, parse failures, dependency errors, closed channels, and invalid state must become typed errors or explicit sanitized logged rejection.
- `pedantic` clippy lints are warned and must pass under `cargo clippy --all --tests -- -D warnings` once the existing codebase is cleaned up.
- No wildcard imports.
- No new `unsafe` blocks, FFI, global mutable state, `static mut`, or unchecked transmute-like behavior without a written review and a crate-level lint exception.

## Code Style

- No trivial comments.
- Minimal bloat (KISS, DRY, SRP).
- No unnecessary state (variables, fields, arguments).
- Each line of code should justify its existence.
- Follow Rust idioms and best practices; latest Rust features can be used.
- Descriptive variable and function names.
- Import types at top of file and use short names everywhere (e.g. `use std::sync::Arc;` then `Arc<T>`, never `std::sync::Arc<T>` inline).
- Keep consts at top of file, right after imports.
- Explicit error handling with `Result<T, E>` over panics.
- Use `thiserror` for domain-specific errors and `color-eyre` at binary edges.
- No inline magic numbers or strings.
- `#[derive(Copy)]` only on structs with one primitive field.
- Prefer structured logging with wide, useful fields.
- Provide helpful error messages.

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
- Missing data, parse failures, dependency errors, closed channels, and invalid state must not be silently defaulted away. Return an error, reject the operation, or use an explicitly named fallback that emits sanitized structured logs.
- Do not call `.ok()` on `Result` to discard errors.

## Dependencies and Supply Chain

- Add new dependencies to the workspace `Cargo.toml` first, then `workspace = true` in the crate.
- Try solving with existing dependencies before adding new ones.
- New dependencies require a purpose, a maintenance check, license compatibility, and `cargo deny check`. Prefer versions published at least 7 days ago. No floating ranges (`latest`, `*`, unbounded `>=`).
- Prefer well-maintained crates from crates.io.
- Disable default features when they pull unused networking, TLS, compression, native, proc-macro, or runtime surface.

## Trust Boundaries and Security

- Treat LLM and provider output as untrusted input. Validate against schemas, domain constraints, and source evidence before persistence or action.
- Do not commit production/private credentials, API keys, tokens, cookies, auth headers, or user data.
- Do not log raw provider payloads, prompts, credentials, or user session data.
- Validate and authorize HTTP, file, queue, config/env, LLM output, and provider callbacks before mutation or persistence.
- Apply prompt-injection defenses where documents, user text, or provider messages are included in prompts or tool calls.
- Tool execution requires allowlisted tools, scoped credentials, explicit user context, audit events, and refusal/denial tests.

## Worktree and Subagent Discipline

- Non-trivial or multi-file changes happen in a dedicated git worktree on a new branch.
- Do not revert, delete, reformat, stage, or commit unrelated user changes.
- Ship finished work: commit with a clear Conventional Commit message, push the branch, and open a draft PR. Do not force-push or push to main.
- Never add AI-agent attribution to commits, PRs, changelogs, or authored content.

## Research and Verification

- Before fixing an unfamiliar failure mode, third-party CLI or tool behavior, library or API behavior, or infra/CI/deployment issue, research the documented behavior first. Use `context7` for current docs, `exa` and web search for known issues, and `thoughtbox` to synthesize findings.
- Report real command results and separate unrelated red-baseline failures from touched-surface regressions.

## Architecture

Rust workspace, key crates in root dir:

- n00n-ui: ratatui interactive UI (elm-like architecture)
- n00n-providers: LLM provider integrations
- n00n-agent: async agent loop on smol
- n00n-interpreter: code_execution tool using pydantic/monty
- n00n-storage: persistent state
- n00n-config: user config
- n00n-lua: Lua plugin system with built-in plugins in ./plugins
- n00n-acp: ACP ndjson stdio server

Built-in Lua plugins in ./plugins: index, bash, glob, question, skill, memory, webfetch, websearch, todo_write, read, write, edit, task, workflow, code_execution, batch, team.

## Docs

Homepage: ./site/. User docs: ./site/docs/. Most pages are generated by `n00n-docgen/src/gen_*.rs`; run `just gen-docs` to regenerate.
Tone: warm, simple, concise, easy for non-native English, story telling. No em-dashes, no emojis, no AI tone.
