# Research: Native Cursor Provider

**Feature**: 007-cursor-native-provider  
**Date**: 2026-07-27  
**Status**: In progress (Phase 0)

## Executive Summary

PR #150 integrated Cursor by shelling out to `cursor-agent --print --output-format stream-json`. That is a **third-party harness proxy**, not a first-tier provider. Cursor's actual inference path is:

```
HTTP/2 + Connect-RPC (application/connect+proto)
  → POST /agent.v1.AgentService/Run
  → regional host e.g. agentn.global.api5.cursor.sh (NOT api2.cursor.sh for runs)
  → bidirectional length-prefixed protobuf frames + ~5s heartbeats
```

`api2.cursor.sh` remains auth, OAuth, and metadata (`GetServerConfig`, token refresh).

n00n should implement this transport in Rust, declare **no Cursor-side tools**, and let the existing n00n agent loop own tool execution.

---

## Protocol Stack (confirmed from CLI bundle + community RE)

| Layer | Detail |
|-------|--------|
| Transport | HTTP/2 (ALPN `h2`; HTTP/1.1 gets 464 on agent hosts) |
| RPC | Connect protocol v1 (`connect-protocol-version: 1`) |
| Content-Type | `application/connect+proto` |
| Framing | `[flags: u8][length: u32 BE][protobuf payload]` |
| Compression | gzip accepted; compressed frames must be rejected or decoded safely |
| Trailers | End-of-stream JSON trailer may carry `error` |

### Primary RPCs

| RPC | Purpose |
|-----|---------|
| `agent.v1.AgentService/Run` | Streaming inference, tool exec channel, checkpoints |
| `agent.v1.AgentService/GetUsableModels` | Model catalog (unary) |
| `agent.v1.AgentService/GetDefaultModelForCli` | CLI default model resolution |
| `agent.v1.AgentService/GetAllowedModelIntents` | Permitted intents |
| `agent.v1.GetServerConfig` (via api2) | Resolve regional agent host |

Legacy (older IDE chat path, may not carry Auto entitlements):

- `aiserver.v1.ChatService/StreamUnifiedChatWithTools`
- `aiserver.v1.AiService/AvailableModels`

**Decision**: Target `AgentService/Run` for v1 because Auto routing and CLI entitlements are tied to the agent surface.

---

## Model ID Gotcha (critical for Auto)

| UI / CLI display | Wire id on `AgentService/Run` |
|------------------|-------------------------------|
| `auto` | `default` |
| `composer-2.5` | `composer-2.5` (verbatim) |

Evidence: [shunt #177](https://github.com/pleaseai/shunt/issues/177), Cursor forum/docs on Cloud API `model: "default"`.

Request protobuf includes model metadata fields (f9/f14 in shunt's hand-rolled encoder): a catalog entry for `default` plus the selected model.

---

## Client Identity Headers (CLI entitlements)

Extracted from `cursor-agent` `2026.07.26-77e48ba` bundle (`index.js`):

```text
Authorization: Bearer <token>
Content-Type: application/connect+proto
Connect-Protocol-Version: 1
Connect-Accept-Encoding: gzip
x-cursor-client-type: cli
x-cursor-client-version: cli-2026.07.26-77e48ba
x-ghost-mode: true|false
x-cursor-streaming: true
x-request-id: <uuid>
x-original-request-id: <uuid>
x-dev-experiment-overrides: <optional>
te: trailers
```

Community RE ([cursor_api_demo](https://github.com/eisbaw/cursor_api_demo)) also documents:

- `x-cursor-checksum` (Jyh cipher + machine id)
- `x-cursor-client-os`, `x-cursor-client-arch`, `x-cursor-timezone`
- `x-session-id` (uuid v5 from token)

**Hypothesis (to validate live)**: Auto unlimited usage requires `client-type: cli` + correct `client-version` + account subscription fields; wrong version → `permission_denied`.

**Experiment matrix** (Phase 0 deliverable):

| Run | client-type | model wire id | Expect |
|-----|-------------|---------------|--------|
| A | cli | default | Auto works on entitled account |
| B | ide | default | May fail or different routing |
| C | cli | auto (wrong) | Unknown model error |
| D | cli | composer-2.5 | Named model; plan-gated |

---

## Auto Routing (server-side)

Protobuf fields observed in bundle:

- `auto_bucket_models` (repeated string)
- `auto_model_selected_display_message`
- `GetDefaultModelForCli` RPC

Server picks the backing model for `default`; client does not hardcode the route.

---

## n00n-as-Harness: Tool Strategy

**Goal**: Cursor provides tokens only; n00n runs tools.

`AgentService/Run` request field `mcp_tools` (f4) advertises client-executable tools. Empty tools encode to a text-only placeholder ([shunt agent.rs](https://github.com/pleaseai/shunt/blob/main/src/adapters/cursor/agent.rs)).

**Decision**:

1. Send **empty** `mcp_tools` for v1 inference-only turns.
2. Do **not** enable `--yolo` / Cursor CLI subprocess.
3. If server emits `exec_server_message` / `mcp_args` anyway, decode and either:
   - (preferred) fail with explicit "Cursor attempted server-side tool; use native text mode", or
   - (stretch) map to n00n `ToolUseStart` if wire format is stable.

n00n tool results re-enter on the next turn via normal message history (same pattern shunt uses for stateless tool bridging).

---

## Auth Sources

| Source | Location | Notes |
|--------|----------|-------|
| API key | `CURSOR_API_KEY`, n00n KeyPool | `loginWithApiKey` in CLI |
| IDE tokens | `~/.config/Cursor/User/globalStorage/state.vscdb` | `cursorAuth/accessToken`, `refreshToken`, `storage.serviceMachineId` |
| CLI config cache | `~/.cursor/cli-config.json` | `serverConfigCache.agentUrlConfig` |
| OAuth | `api2.cursor.sh/auth/poll`, refresh endpoints | For n00n UI login |
| Dynamic scripts | `resolve` subcommand JSON | Must flow into native provider |

Refresh: `POST https://api2.cursor.sh/oauth/token` (grant_type=refresh_token).

---

## Bidirectional Stream Behavior

Current CLI uses **paced full-duplex** request stream:

- Initial `AgentClientMessage` with `run_request`
- Periodic `client_heartbeat` (~5s) to keep turn alive
- Single-shot half-close → `internal: No exec result` (shunt #177)

n00n must implement heartbeat sender concurrent with response decoder.

---

## Reference Implementations (Rust/TS)

| Project | Value |
|---------|-------|
| [pleaseai/shunt `cursor/agent.rs`](https://github.com/pleaseai/shunt/blob/main/src/adapters/cursor/agent.rs) | Current wire (2026), HTTP/2, `default` mapping, empty tools |
| [pi_agent_rust `cursor.rs`](https://github.com/Dicklesworthstone/pi_agent_rust) | Connect frame codec, minimal protobuf |
| [cursor-opencode-provider](https://github.com/oakimov/cursor-opencode-provider) | GetServerConfig host resolution, TS protobuf |
| [eisbaw/cursor_api_demo](https://github.com/eisbaw/cursor_api_demo) | Auth reader, checksum, HTTP/2 client (older aiserver path) |

---

## PR #150 Legacy Adapter: Unique Value?

| Capability | CLI proxy | Native (target) |
|------------|-----------|-------------------|
| Auto unlimited | Yes (inherits CLI identity) | Must replicate headers (Phase 0 proof) |
| Named models | Yes | Yes via GetUsableModels |
| Cursor runs tools | Yes (yolo) | **No** (by design) |
| n00n tools | No (ignored `_tools`) | Yes |
| Dynamic auth | Broken (`Cursor::new` ignores script auth) | Fixed in FR-007 |
| Static model registry | 190 entries, drifts | Removed |
| Session resume | `--resume` cursor session id | Checkpoint/KV or history replay |
| Subprocess cost | High | None |

**Interim**: Keep CLI adapter as `cursor-cli` or `N00N_CURSOR_TRANSPORT=cli` only until experiment matrix proves native Auto parity.

---

## Unary vs Streaming Content-Type (live-verified 2026-07-27)

| RPC class | Host | Content-Type | Body |
|-----------|------|--------------|------|
| Unary (`GetUsableModels`, `GetServerConfig`, …) | `api2.cursor.sh` | `application/json` | JSON object (`{}` or `{"telemEnabled":false}`) |
| Bidirectional (`AgentService/Run`) | `agentn.*.api5.cursor.sh` | `application/connect+proto` | Connect-framed protobuf stream |

**Correction**: Earlier Phase 0 curl probes used `application/connect+proto` for unary calls and got HTTP 415. That was a **client encoding mistake**, not an auth or endpoint failure. Discovery can use existing `isahc` (HTTP/1.1); Run still needs HTTP/2 + Connect frames.

`GetServerConfig` path: `POST /aiserver.v1.ServerConfigService/GetServerConfig` (not `AiService`).

Response includes `agentUrlConfig.agentnUrl` → `https://agentn.global.api5.cursor.sh`.

### HTTP/2 client choice

Prefer enabling workspace `isahc` feature `http2` (curl/nghttp2) over adding `reqwest`/`tokio`. n00n is smol-based; isahc already powers all other providers.

### Checkpoint / KV protocol (user-required multi-turn)

From `cursor-agent` 2026.07.26 bundle:

| Direction | Message | Fields |
|-----------|---------|--------|
| Server → client | `AgentServerMessage.kv_server_message` (f4) | id; get_blob_args{blob_id} \| set_blob_args{blob_id,blob_data} |
| Client → server | `AgentClientMessage.kv_client_message` (f3) | id; get_blob_result{blob_data?} \| set_blob_result{error?} |
| Resume | `UserMessage.conversation_state_blob_id` (f10 bytes) | Opaque blob id from prior set |

n00n implements a per-session blob store (`checkpoint.rs`) and must answer get/set during `Run`, then attach `conversation_state_blob_id` on follow-up turns.

Capture helper: `mise run mitm-setup` then `scripts/cursor_capture.sh`.

---

## Phase 0 Tasks

- [x] JSON unary discovery (`GetUsableModels`, `GetServerConfig`)
- [x] Connect frame codec + unit tests
- [x] Hand-rolled Run frame encoder (`proto.rs`)
- [x] Checkpoint blob store + Kv parse/encode (`checkpoint.rs`)
- [x] mitmproxy via `mise run mitm-setup` + `scripts/cursor_capture.sh`
- [ ] Live `AgentService/Run` spike (isahc http2)
- [ ] Run entitlement experiment matrix (A–D)
- [ ] Document checksum requirement
- [ ] Two-turn checkpoint replay live test
- [ ] Fuzz target for Connect frames

---

## Risks

| Risk | Mitigation |
|------|------------|
| Protocol drift | Versioned client string from env override; live test gate |
| Auto entitlement lock to CLI fingerprint | RE + header parity tests; publish findings |
| HTTP/2 dep weight | Isolate in `n00n-providers` submodule `cursor_connect` |
| Tool frames on empty mcp_tools | Explicit detection + typed error |
| Legal/ToS | Open research doc; user-owned credentials only |
