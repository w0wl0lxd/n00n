# Plan: CacheHealth for all non-OpenAI providers

**Goal**: Every provider emits `ProviderEvent::CacheHealth` after a turn so the status bar always reflects the current provider's cache state.

**Architecture**: Centralize `OpenAiCompatProvider` CacheHealth emission in a helper method, then wire every provider to call it or emit an explicit "no cache" event.

**Tech Stack**: Rust workspace, `n00n-providers`, `flume`, `serde_json`.

## Global Constraints

- `unsafe_code` is denied.
- No `unwrap_or`, `unwrap_or_default`, or `.ok()` on `Result`.
- TDD: failing or missing test first, implementation, refactor.
- All production changes pass `cargo clippy --all --tests -- -D warnings`.

## Tasks

1. **Extend `OpenAiCompatConfig`** with `cache_ttl_seconds` and update `build.rs` to read it (default 0).
2. **Add `OpenAiCompatProvider::emit_cache_health`** helper that reads `TokenUsage` and the configured TTL.
3. **Update OpenRouter and Mistral** TOMLs (`cache_ttl_seconds = 300`) and Rust to use the helper.
4. **Wire remaining OpenAI-compat providers** to the helper: `deepseek.rs`, `zai/mod.rs`, `tensorx.rs`, `synthetic.rs`, `opencode.rs`, `custom.rs`.
5. **Wire `local.rs`** for both `compat.do_stream` and `responses::do_stream` paths.
6. **Emit `valid_until=0` CacheHealth** for `cursor/mod.rs`, `copilot/mod.rs` (responses), and `devin.rs`.
7. **Add/update unit tests** for `emit_cache_health` and affected provider streams.
8. **Verify** with `cargo fmt --all`, `cargo check -p n00n-providers`, `RUST_TEST_THREADS=1 cargo test -p n00n-providers`, `cargo clippy --all --tests -- -D warnings`.
