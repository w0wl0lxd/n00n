# Feature Specification: n00n-daemon + session bridge (fixed)

**Feature Branch**: `feat/n00n-daemon-session-bridge`

**Created**: 2026-07-27

**Status**: Active

**Input**: Approach 3 hybrid agent control; critique-fixed phased delivery; on-device, deterministic, no LLM summarization.

## Critique fixes (binding)

1. **Topology**: Registry/proxy over (a) TUI via existing `UiAction::Session` and (b) PR #134 **per-agent** `state_dir/agents/<id>/control.sock`. Not a fake “already multiplexed” worker plane.
2. **TUI ingress**: Daemon→TUI **only** through `ui_action_tx` / `SessionRequest`. No second UDS into ratatui.
3. **Verb×backend table** (mandatory):

| Verb | Tui | Worker (PR134 sock) |
|------|-----|---------------------|
| list/status | live sessions | `agents/*/agent.json` |
| message | `Prompt` (+ steer/control flags) | `ClientCommand::Message` |
| stop | `Cancel` | `ClientCommand::Stop` |
| pause | **Unsupported** (typed error) | `ClientCommand::Pause` |
| resume | **Unsupported** unless paused-team path later | `ClientCommand::Resume` |

4. **Identity**: Records carry `id`, `backend` (`tui`|`worker`), optional `session_id`. Lookup: optional `backend` hint, else tui then worker.
5. **Lifecycle**: Socket `state_dir/daemon.sock` (`0700` dir / `0600` sock). Stale sock replaced on bind. TUI exit tears down listener; worker entries remain on disk.
6. **Startup**: TUI starts in-process listener when UI is up. CLI client connects; if absent, still lists workers from disk (no hard dependency on TUI).
7. **Windows**: Unix UDS server/client are `cfg(unix)`. Non-unix builds compile with transport stubs that return typed errors.
8. **Phase 2 not blocked on daemon for TUI-only UX**: tools may use `n00n.session.*` when present; daemon client used for union/CLI.

## Phases

### Phase 1a — Control protocol + TUI backend
Wire protocol + `ControlPlane` + TUI backend via `UiAction::Session`. Extend `SessionRequest::Prompt` with `steer`/`control` (default false).

### Phase 1b — Worker registry proxy
Read PR #134 layout; proxy pause/resume/stop/message to per-agent socks. Empty dir ⇒ empty worker list.

### Phase 1c — Daemon UDS + thin CLI
NDJSON server/client; `n00n agent {list,status,message,pause,resume,stop}` thin client.

### Phase 2 — Scoped tools + encoding
Split into `agent_list`, `agent_status`, `agent_control` (mutating; `defer_loading=true`). Outputs: `n00n.json.tooned` for structured payloads; plain acks for pause/stop/message; JSON only for on-disk policy file. Human header/annotation/body — no raw dumps.

## Encoding / crate policy

- Prefer `sonic-rs` for all new JSON encode/decode in `n00n-daemon` (typed protocol + untyped `sonic_rs::Value`).
- `serde_json` only at forced boundaries: `SessionReply`, `toon-format` / `n00n.json.tooned`, `jsonschema`, and existing UI/Lua APIs typed as `serde_json::Value`. Convert explicitly at the edge; do not dual-type the wire protocol.
- Pin `sonic-rs` ≥7 days old (workspace: `0.5.8`). Apache-2.0; `cargo deny` allowlist compatible.
- Do not require `-C target-cpu=native` for correctness (CI does not set it); accept portable SIMD fallback.

## Rust hard gates (non-negotiable)

- `unsafe_code = deny` / `forbid(unsafe_code)` on new crates.
- No `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `dbg!` in production **or** tests for this feature (prefer `Result` tests / `assert!` / `match`).
- No silent `unwrap_or` / `unwrap_or_default` / `.ok()` on `Result` (workspace `clippy.toml` disallowed-methods).
- Typed `ControlError` / `thiserror` in the library; `color-eyre` only at binary edges.

## Success criteria

1. In-process server + client round-trip `health`/`list`.
2. TUI live session appears in `list` with `backend=tui`.
3. Worker `agent.json` fixtures appear with `backend=worker`.
4. `pause` against tui id returns typed unsupported error (not cancel).
5. `agent_list` / `agent_status` / `agent_control` tools render non-JSON human cards; llm_output is tooned or plain as specified.
6. `cargo test -p n00n-daemon` and plugin tests pass; clippy `-D warnings` on touched crates.
