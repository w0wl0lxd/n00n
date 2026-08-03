# Tasks for 010-cache-health-non-openai

- [ ] T001 Add `cache_ttl_seconds: u64` to `OpenAiCompatConfig`.
- [ ] T002 Update `build.rs` to parse optional `cache_ttl_seconds` with default 0.
- [ ] T003 Set `cache_ttl_seconds = 300` in `openrouter.toml` and `mistral.toml`.
- [ ] T004 Add `OpenAiCompatProvider::emit_cache_health` method.
- [ ] T005 Replace manual emission in `openrouter.rs` with helper call.
- [ ] T006 Replace manual emission in `mistral.rs` with helper call.
- [ ] T007 Wire `deepseek.rs`, `zai/mod.rs`, `tensorx.rs`, `synthetic.rs`, `opencode.rs`, and `custom.rs` to emit cache health via helper.
- [ ] T008 Wire `local.rs` (`compat.do_stream`) and responses paths in `local.rs`, `custom.rs`, `copilot/mod.rs` to emit cache health.
- [ ] T009 Emit `valid_until=0` CacheHealth in `cursor/mod.rs` and `devin.rs`.
- [ ] T010 Add/update unit tests in `openai_compat.rs` and affected provider tests.
- [ ] T011 Run `cargo fmt --all`.
- [ ] T012 Run `cargo check -p n00n-providers`.
- [ ] T013 Run `RUST_TEST_THREADS=1 cargo test -p n00n-providers`.
- [ ] T014 Run `cargo clippy --all --tests -- -D warnings`.
- [ ] T015 Commit, push, and open draft PR.
