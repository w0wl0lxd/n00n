Maki is an AI coding agent (like Claude Code and opencode), that is built bottom up to optimize costs and number of tokens used, while not sacrificing performance too much.

## Code guidelines

- No trivial comments
- Minimal bloat (KISS, DRY, SRP)
- No unnecessary state (variables, fields, arguments)
- Each line of code should justify its existence
- Follow Rust idioms and best practices
- Latest Rust features can be used
- Descriptive variable and function names
- No wildcard imports
- Import types at top of file and use short names everywhere (e.g. `use std::sync::Arc;` then `Arc<T>`, never `std::sync::Arc<T>` inline)
- Keep consts at top of file, right after imports
- Explicit error handling with `Result<T, E>` over panics
- Use `color_eyre` when the specific error is not as important
- Use custom error types using `thiserror` for domain-specific errors
- Place unit tests in the same file using `#[cfg(test)]` modules
- Add dependencies to global `Cargo.toml`, and then set workspace=true in specific package
- Try solving with existing dependencies before adding new ones
- Prefer well-maintained crates from crates.io
- Be mindful of allocations in hot paths
- Prefer structured logging (wide logs with a bunch of useful fields)
- Provide helpful error messages
- Use #[test_case] when writing tests, and use snake_case for naming the tests
- No need for bullshit tests (e.g. tautology)
- No inline magic numbers or strings
- In tests const error/status messages and assert against the shared constant
- Add #[derive(Copy)] only on structs with 1 primitive field
- NO TRIVIAL COMMENTS

## Testing

- cargo clippy --all --tests -- -D warnings
- cargo nextest run --workspace

Read `justfile` for more.

## Architecture

Rust workspace, key crates:

- **maki-ui**: Uses ratatui for an interactive UI (elm like architecture)
- **maki-providers**: Integration with LLM providers via APIs (e.g. Anthropic, Z.AI)
- **maki-agent**: An async agent loop that runs on smol + tools descriptions and implementations
- **maki-interpreter**: code_execution tool implementation using pydantic/monty (a minimal python sandbox)
- **maki-code-index**: index tool implementation using tree-sitter (returns a compact skeleton of a source file)
- **maki-storage**: Persistent state across runs (e.g. sessions, auth)
