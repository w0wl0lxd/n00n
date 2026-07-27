# Tasks: 007 Native Cursor Provider

**Branch**: `feat/cursor-native`  
**PR**: https://github.com/w0wl0lxd/n00n/pull/179

## Phase 0 — RE + spike (in progress)

- [x] Speckit artifacts (`spec.md`, `plan.md`, `research.md`, contracts)
- [x] Adversarial review (`adversarial-review.md`)
- [x] Legacy CLI fixes (is_error, resume, inactivity timeout, with_auth)
- [x] Connect frame codec (`connect.rs`) + unit tests
- [x] Wire model id map (`wire.rs`)
- [x] IDE auth reader (`auth.rs` + rusqlite)
- [x] JSON unary discovery spike (`discovery.rs` + live tests)
- [ ] **Corrected**: unary RPCs use `application/json` on api2 (not connect+proto)
- [ ] reqwest HTTP/2 client for `AgentService/Run` on agentn
- [ ] Minimal Run protobuf encoder (from shunt RE)
- [ ] Live Run spike with `default` + empty tools
- [ ] Two-turn `conversation_id` persistence experiment
- [ ] Auto entitlement matrix (cli headers × model wire id)
- [ ] Traffic capture doc (mitmproxy optional)

## Phase 1 — Native MVP

- [ ] `CursorNative` provider module
- [ ] Stream decode (text/thinking/usage)
- [ ] Heartbeat sender (~5s)
- [ ] Reject unexpected tool exec frames
- [ ] Default model → `cursor/default`

## Phase 2 — Dynamic catalog

- [ ] Wire `fetch_usable_models` into provider inventory
- [ ] TTL cache; deprecate `models_data.rs`
- [ ] `~/.cursor/cli-config.json` token fallback

## Phase 3 — Harness hardening

- [ ] Session / conversation_id persistence
- [ ] Usage accounting
- [ ] Integration tests under `N00N_CURSOR_LIVE_TESTS=1`

## Phase 4 — CLI demotion

- [ ] `N00N_CURSOR_TRANSPORT=cli` escape hatch
- [ ] Docs + quickstart update
