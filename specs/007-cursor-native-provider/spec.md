# Feature Specification: Native Cursor Provider

**Feature Branch**: `feat/cursor-native` / `007-cursor-native-provider`

**Created**: 2026-07-27

**Status**: Draft

**Input**: Replace the merged CLI-proxy Cursor provider with a first-tier native integration. Cursor supplies inference (including unlimited `auto`), caching, and billing entitlements. n00n remains the sole agent harness: tools, permissions, sessions, and UI.

---

## User Scenarios & Testing

### User Story 1 - Native Auto model through n00n (Priority: P1)

A Cursor subscriber wants to run `cursor/default` (display name `auto`) inside n00n with the same included/unlimited Auto entitlement they get in the Cursor CLI, without spawning `cursor-agent`.

**Why this priority**: Auto is the hard requirement and the primary cost-optimization path for Cursor users. The current CLI proxy adds latency, breaks auth injection for dynamic providers, and duplicates agent behavior n00n already owns.

**Independent Test**: On a machine with Cursor credentials but **without** `cursor-agent` on `PATH`, start n00n with `cursor/default`, send a multi-turn prompt, and verify streamed assistant text with usage accounting.

**Acceptance Scenarios**:

1. **Given** valid Cursor credentials and `cursor/default` selected, **When** the user sends a message, **Then** n00n streams assistant text via native Connect without spawning a subprocess.
2. **Given** a multi-turn n00n session, **When** the user sends follow-up messages, **Then** conversation context is preserved and responses continue without duplicating prior assistant turns.
3. **Given** an account entitled to unlimited Auto, **When** the user runs several turns on `cursor/default`, **Then** requests succeed without usage-limit errors that differ from the official CLI under the same account.
4. **Given** `cursor-agent` is absent from `PATH`, **When** the user selects `cursor/default`, **Then** the provider still works.

---

### User Story 2 - n00n owns the tool loop (Priority: P1)

A user wants Cursor to act as an inference backend only. n00n's existing tools (bash, edit, MCP, etc.) execute through n00n's permission model, not through Cursor's built-in agent tools.

**Why this priority**: Running through `cursor-agent` yolo mode delegates tool execution to Cursor and bloats the stack. Native integration must not silently run Cursor-side shell/write/MCP tools.

**Independent Test**: Enable n00n tools, select `cursor/default`, ask the agent to use a n00n tool, and verify the tool call is issued and executed by n00n — not by a Cursor subprocess.

**Acceptance Scenarios**:

1. **Given** n00n tools are enabled, **When** the model returns a tool call through the n00n provider interface, **Then** n00n executes the tool via its normal loop.
2. **Given** a native Cursor request, **When** the wire request is built, **Then** Cursor built-in agent tools are not enabled (no undeclared server-side tool execution path).
3. **Given** Cursor emits a server exec/tool frame anyway, **When** n00n receives it, **Then** the provider surfaces a typed error or maps it to n00n's tool channel without executing Cursor-side tools.

---

### User Story 3 - Automatic model catalog (Priority: P1)

A user wants model lists, tiers, and pricing metadata to stay current without a 3,000-line checked-in registry.

**Why this priority**: The merged PR ships a static `cursor_models.rs` that drifts from `cursor-agent --list-models` within days.

**Independent Test**: Call provider model discovery with live credentials and verify the list includes `auto` and at least one named model with context window metadata.

**Acceptance Scenarios**:

1. **Given** valid Cursor credentials, **When** n00n lists `cursor/*` models, **Then** models are fetched from Cursor's `GetUsableModels` (and related) APIs, not from a static file.
2. **Given** a newly released Cursor model, **When** the upstream catalog updates, **Then** n00n shows it after cache expiry without a code change.
3. **Given** discovery is temporarily unavailable, **When** the user selects a known model id, **Then** n00n falls back to sane defaults with a structured warning log.

---

### User Story 4 - Seamless auth from existing Cursor login (Priority: P2)

A user who already ran `cursor login` or uses Cursor IDE should not re-authenticate in n00n.

**Why this priority**: Lowest friction path; mirrors user expectation from other native RE projects.

**Independent Test**: With IDE tokens present and no `CURSOR_API_KEY`, native provider resolves a valid bearer token.

**Acceptance Scenarios**:

1. **Given** `cursorAuth/accessToken` exists in `state.vscdb`, **When** n00n starts the native provider, **Then** it loads and refreshes tokens automatically.
2. **Given** `~/.cursor/cli-config.json` contains cached auth, **When** IDE storage is absent, **Then** n00n falls back to CLI config tokens.
3. **Given** no stored tokens, **When** the user sets `CURSOR_API_KEY`, **Then** that key is used.
4. **Given** a dynamic provider script resolves scoped credentials, **When** native Cursor runs, **Then** those credentials are honored.

---

### User Story 5 - Legacy CLI path retained only when unique (Priority: P3)

Operators may keep the subprocess adapter only if it exposes behavior native Connect cannot reproduce during the transition.

**Why this priority**: Avoid maintaining two stacks unless the CLI gate-keeps entitlements we cannot yet replicate.

**Independent Test**: Compare Auto entitlement behavior between native and CLI paths under controlled experiments documented in `research.md`.

**Acceptance Scenarios**:

1. **Given** native Auto parity is proven, **When** users select `cursor/default`, **Then** the native provider is used by default.
2. **Given** a documented CLI-only gap remains, **When** the user sets `N00N_CURSOR_TRANSPORT=cli`, **Then** the legacy subprocess adapter is available under a distinct slug or flag.
3. **Given** the legacy path remains, **When** it runs, **Then** P1/P2 bugs from PR #150 review are fixed (error results, resume boundary, inactivity timeout, auth wiring).

---

### Edge Cases

- Cursor rotates `x-cursor-client-version` or agent host (`GetServerConfig`); n00n must surface a clear upgrade hint, not opaque `permission_denied`.
- HTTP/2-only agent endpoints reject HTTP/1.1 with 464; transport must negotiate h2.
- Display id `auto` must map to wire id `default` on `AgentService/Run`.
- Privacy mode (`x-ghost-mode: true`) may route to a different agent host; must be configurable and validated.
- Token refresh races between IDE and n00n must not corrupt stored credentials.
- Long streams must use inactivity timeouts, not fixed wall-clock kills.
- Protocol breakage when Cursor ships a new CLI bundle: frame parser fuzz tests catch panics; live tests catch semantic drift.

---

## Requirements

### Functional Requirements

- **FR-001**: The `cursor` built-in provider MUST speak Cursor `agent.v1.AgentService` over Connect+protobuf on HTTP/2 without spawning `cursor-agent` for normal chat.
- **FR-002**: The provider MUST support `cursor/default` (`auto`) as a first-class model with entitlement behavior equivalent to the official CLI for the same account.
- **FR-003**: The provider MUST map display model ids to wire ids (`auto` → `default`) per live protocol research.
- **FR-004**: The provider MUST discover models dynamically via Cursor APIs; static per-model registries MUST NOT be required for correctness.
- **FR-005**: The provider MUST integrate with n00n's `Provider` trait: streaming text/thinking deltas, token usage, and typed errors.
- **FR-006**: The provider MUST NOT execute Cursor built-in agent tools; n00n MUST remain the sole tool harness.
- **FR-007**: The provider MUST resolve auth from API key, Cursor IDE/CLI token storage (`state.vscdb`, `~/.cursor/cli-config.json`), n00n credential store, and dynamic provider scripts — preferring existing Cursor login over a new OAuth UI when tokens are present.
- **FR-008**: The provider MUST send the client identity headers required for CLI entitlements (`x-cursor-client-type`, `x-cursor-client-version`, ghost mode, request ids) as documented in `research.md`.
- **FR-009**: The provider MUST resolve agent base URL via `GetServerConfig` (or equivalent) with HTTPS `*.cursor.sh` validation and in-memory caching per process.
- **FR-010**: The legacy CLI adapter MAY remain behind an explicit flag or `cursor-cli` slug until native Auto parity is proven.
- **FR-011**: Research artifacts (wire layouts, header matrix, Auto entitlement experiments) MUST be checked into `specs/007-cursor-native-provider/` for transparency.

### Key Entities

- **CursorSession**: Maps n00n `SessionRef` to Cursor conversation/checkpoint identifiers when the wire protocol requires them.
- **CursorAuth**: Resolved bearer token, refresh material, machine id, and source (api_key, ide, oauth, script).
- **CursorModel**: Display id, wire id, tier hints, context/output limits, thinking support — from live discovery.
- **ConnectFrame**: Length-prefixed Connect envelope (flag, big-endian length, payload) for bidirectional streams.
- **AgentRunTurn**: One n00n turn's request stream (initial run frame, heartbeats) and decoded response events.

---

## Success Criteria

### Measurable Outcomes

- **SC-001**: `cursor/default` works with `cursor-agent` absent from `PATH` on a credentialed dev machine.
- **SC-002**: Auto entitlement parity: native and CLI paths produce the same success/failure class on a scripted 10-turn soak test (documented in `research.md`).
- **SC-003**: Model discovery returns ≥1 model including `auto` without any checked-in model list.
- **SC-004**: Provider unit tests cover frame codec, model id mapping, auth resolution, and stream decoding with ≥80% line coverage on new native modules.
- **SC-005**: `cargo clippy --all --tests -- -D warnings` and `cargo nextest run --workspace` pass with live Cursor tests gated behind `N00N_CURSOR_LIVE_TESTS=1`.
- **SC-006**: No silent success on Cursor `is_error` results; failures become typed `AgentError`s with retry/auth hints.

---

## Assumptions

- Cursor's agent wire protocol is proprietary and may change; the project accepts maintenance burden and documents RE openly.
- Community references ([shunt](https://github.com/pleaseai/shunt), [pi_agent_rust](https://github.com/Dicklesworthstone/pi_agent_rust), [cursor-opencode-provider](https://github.com/oakimov/cursor-opencode-provider), [cursor_api_demo](https://github.com/eisbaw/cursor_api_demo)) are starting points, not guarantees.
- `api2.cursor.sh` is auth/metadata; agent inference uses regional `agentn.*.api5.cursor.sh` hosts (current CLI behavior).
- v1 may send full message history per turn if checkpoint resume is not yet implemented, matching other stateless providers initially.
- Users running this feature are authorized Cursor account holders.

## Out of Scope (v1)

- Cloud Agents REST API (`/v1/agents`) — different product surface.
- Cursor-side MCP server management.
- Bundling or redistributing Cursor protobuf `.proto` files from proprietary bundles (generated/minimal hand-rolled schemas only).
- Windows-specific auth storage (may follow in P2 if Linux/macOS land first).
