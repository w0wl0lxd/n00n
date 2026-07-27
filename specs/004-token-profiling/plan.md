# Token Profiling Implementation Plan

> **For agentic workers:** Use TDD. Steps use checkbox syntax.

**Goal:** Add `n00n-token-profile` with offline cold-start baselines and hard CI regression tests (PR1).

**Architecture:** Dev/test crate depends on `n00n-agent`, `n00n-lua`, `n00n-providers`. Builds the same cold-start tool/system payloads production uses, measures bytes+tokens via existing `count_*_for_model`, compares to committed JSON baseline.

**Tech Stack:** Rust workspace crate, serde_json, thiserror, nextest (via `just test`).

## Global Constraints

- No live LLM; no unwrap/expect in non-test code; `unsafe_code` deny.
- Fixture must use `ToolFilter::from_config` + pinned Vars (not `ToolFilter::All` / `Vars::new()`).
- Hard gates only on `main_tools_schemas` and `system_prompt`.
- Estimator metrics only; document ≠ billing.

---

### Task 1: Crate scaffold + failing regression test

**Files:**
- Create: `n00n-token-profile/Cargo.toml`
- Create: `n00n-token-profile/src/lib.rs`
- Create: `n00n-token-profile/src/error.rs`
- Create: `n00n-token-profile/tests/regression.rs`
- Modify: root `Cargo.toml` (members + workspace.dependency)

- [ ] Add workspace member and empty API stubs returning typed errors
- [ ] Write `tests/regression.rs` asserting cold-start profile has tools and passes baseline compare (fails until implemented)
- [ ] Implement profile + baseline compare
- [ ] Generate `baselines/cold_start.json` from fixture
- [ ] Point `scripts/dynamic_tool_size.rs` at crate helpers or leave thin wrapper
- [ ] `cargo fmt`, `cargo clippy -p n00n-token-profile --tests -- -D warnings`, `cargo nextest run -p n00n-token-profile`
