# Implementation Plan: Native Cursor Provider

**Branch**: `feat/cursor-native` | **Date**: 2026-07-27 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/007-cursor-native-provider/spec.md`

## Summary

Replace the merged `cursor-agent` subprocess provider with a **native HTTP/2 Connect** client to `agent.v1.AgentService`. Cursor supplies inference (hard requirement: `auto` / wire `default` with CLI entitlements). n00n remains the sole harness: tools, permissions, sessions. Dynamic model discovery replaces the static 3k-line registry. Legacy CLI adapter stays behind a flag only until native Auto parity is proven.

## Technical Context

**Language/Version**: Rust (workspace `rust-toolchain.toml`)

**Primary Dependencies** (new / changed):
- HTTP/2: enable `isahc` `http2` feature (curl/nghttp2) — preferred over `reqwest`/`tokio` to stay on smol
- `rusqlite` (bundled) for IDE `state.vscdb` token read
- Existing: `n00n-providers`, `n00n-storage`, `n00n-config`, `serde_json`, `smol`

**Storage**: n00n credential store + optional read of Cursor IDE SQLite (`state.vscdb`)

**Testing**: `cargo nextest`, `#[test_case]`, live gate `N00N_CURSOR_LIVE_TESTS=1`, `cargo-fuzz` on Connect frame decoder

**Target Platform**: Linux first (developer machine), then macOS auth paths

**Performance Goals**: Subprocess elimination; first token latency within 2× of CLI proxy on same prompt

**Constraints**:
- `unsafe_code = deny` workspace-wide
- No silent defaults on auth/protocol failures
- Inactivity-based stream timeouts
- Automatic model catalog (no static 190-entry file)

**Scale/Scope**: ~2–4 new modules in `n00n-providers`, refactor existing `cursor.rs` → `cursor_cli.rs`, new `cursor_native/`

## Constitution Check

| Gate | Status |
|------|--------|
| TDD: failing tests before implementation | Required per phase |
| Typed errors, no unwrap in prod | Required |
| New deps in workspace `Cargo.toml` first | `isahc` http2 + `rusqlite` landed; Run spike in progress |
| Trust boundary: provider output untrusted | Frame size caps, recursion limits on protobuf decode |
| Tests for auth/refusal paths | Required in SC-004 |

## Project Structure

### Documentation (this feature)

```text
specs/007-cursor-native-provider/
├── spec.md
├── plan.md              # this file
├── research.md
├── quickstart.md
├── contracts/
│   └── cursor-connect.md
├── checklists/
│   └── requirements.md
└── tasks.md             # /speckit.tasks output (next step)
```

### Source Code

```text
n00n-providers/src/providers/cursor/
├── mod.rs           # Legacy CLI Provider (current default)
├── models_data.rs   # Static registry (DELETE after discovery lands)
├── connect.rs       # Connect frame codec
├── wire.rs          # Headers + auto→default map
├── auth.rs          # IDE sqlite + API key resolve
├── discovery.rs     # JSON unary GetUsableModels / GetServerConfig
├── proto.rs         # Hand-rolled AgentClientMessage encode/decode
├── run.rs           # HTTP/2 AgentService/Run driver + heartbeats
└── checkpoint.rs    # KvClientMessage blob store (multi-turn)
scripts/
└── cursor_capture.sh    # mitmproxy helper (`mise run mitm-setup`)
```

**Structure Decision**: Flat `cursor/` modules (no nested `native/` yet). CLI stays in `mod.rs` until Phase 4; native Run lives in `run.rs` and is selected via env/`Provider` facade later.

## Phased Delivery

### Phase 0 — Protocol RE & Spikes (current)

**Deliverables**: `research.md` experiments, captured traces, frame codec crate-within-crate, entitlement matrix results.

| Task | Method |
|------|--------|
| Unary discovery | JSON `application/json` on api2 via isahc (**done**) |
| HTTP/2 Run spike | isahc `http2` POST to `agentn…/AgentService/Run` |
| Frame codec | Port shunt encode/decode + unit tests (**done**) |
| Auto experiment A–D | Live scripts, document in `research.md` |
| Checksum probe | Compare requests with/without `x-cursor-checksum` |
| Checkpoint RE | Capture `KvServerMessage` get/set blob cycle; implement store |

**Exit gate**: Live Rust `GetUsableModels` (**done**). Live `AgentService/Run` with wire `default` + empty tools returns text. Two-turn spike using **KvClientMessage checkpoint replay** (user decision) passes before Phase 1.

### Phase 1 — Native MVP (`cursor/default` + streaming)

- `CursorNative` implements `Provider::stream_message`
- Auth: IDE SQLite (`state.vscdb`) + API key + dynamic script auth
- `GetServerConfig` host resolution
- `Run` with empty `mcp_tools`, heartbeats, text/thinking deltas
- Model id map: `auto` → `default` (`wire.rs`)
- Inactivity timeout on stream read
- ~~Legacy P1 bugs~~ (done on `feat/cursor-native` bf05b4d1f)

**Exit gate**: SC-001, SC-006 on live tests; two-turn checkpoint blob replay without duplicate/lost context.

### Phase 2 — Dynamic model catalog

- `GetUsableModels` with TTL cache (replaces `models_data.rs` / `gen_cursor_models.py`)
- Default model → `cursor/default` (Auto)
- Optional: `~/.cursor/cli-config.json` token fallback

**Exit gate**: SC-003.

### Phase 3 — n00n harness hardening

- Tool frame detection → typed error (no Cursor execution)
- Multi-turn via **KvClientMessage checkpoint store** (user decision); conversation_id is secondary
- Usage/token accounting from stream frames
- Persist conversation_id + blob store keys in n00n session storage

**Exit gate**: User Story 2 acceptance scenarios.

### Phase 4 — Legacy demotion

- Default transport = native
- CLI → `cursor-cli` slug or `N00N_CURSOR_TRANSPORT=cli`
- Docs + changelog
- Delete static registry if discovery stable 2 weeks

**Exit gate**: SC-002 parity soak test.

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected |
|-----------|------------|------------------------------|
| Hand-rolled protobuf subset | No official `.proto` redistribution | `prost` from stolen protos — license/fragility |
| HTTP/2 dependency | Agent hosts reject HTTP/1.1 | Subprocess CLI — user rejected as bloat |
| Bidirectional stream + heartbeats | Required by current `Run` wire | Single POST body — server error |

## Verification Commands

```bash
cargo fmt --all
cargo clippy --all --tests -- -D warnings
cargo nextest run -p n00n-providers
RUST_TEST_THREADS=1 cargo test -p n00n-providers
N00N_CURSOR_LIVE_TESTS=1 cargo nextest run -p n00n-providers -- cursor_live
cargo fuzz run connect_frame -- -max_total_time=60   # after fuzz target lands
```

## Next Step

Run `/speckit.tasks` (or author `tasks.md`) to break Phase 0 into assignable work items, then begin TDD on `connect.rs` frame codec.
