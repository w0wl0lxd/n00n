# Adversarial Review: 007 Native Cursor Provider

**Date**: 2026-07-27  
**Reviewer stance**: Assume the plan is wrong until live evidence confirms it.  
**Verdict summary**: **PLAUSIBLE with material gaps** — direction is sound, several claims are overstated or unverified, and multi-turn native behavior is the highest-risk unknown.

---

## REFUTED or Corrected Claims

| Claim in plan/research | Counter-evidence | Fix |
|------------------------|------------------|-----|
| Wire id `auto` always fails on `AgentService/Run` | Live CLI accepts both `--model auto` and `--model default` (both returned `ok`). Shunt's failure may apply only to **direct** wire calls without CLI translation. | Document: CLI normalizes; native must still send `default` until proven otherwise. |
| `api2.cursor.sh` hosts `AgentService/Run` (pi_agent_rust) | Shunt #177: Run moved to `agentn.*.api5.cursor.sh`; api2 is auth/metadata. | Already in research.md; remove any pi references to api2 Run. |
| Phase 1 "fix legacy P1 bugs in parallel" | Legacy fixes **already landed** on `feat/cursor-native` (commit bf05b4d1f). | Update plan Phase 1 checklist — done. |
| Phase 2 "with_auth for dynamic/custom" | **Already implemented** in same commit. | Move to completed; Phase 2 is discovery + sqlite auth only. |
| Phase 2 "OAuth/PKCE in n00n UI" | User chose **existing Cursor token grab** over new OAuth UI for v1. | Removed from v1 scope; sqlite/cli-config first. |
| Naive `curl --http2` reproduces Connect unary | **Partially refuted**: `connect+proto` unary → HTTP 415, but **`application/json` `{}` succeeds** on api2 for `GetUsableModels` and `GetServerConfig` (33 KiB model catalog live-verified). | Phase 0: JSON unary on api2 for discovery/config; `connect+proto` framing only for bidirectional `Run` on agentn. |

---

## PLAUSIBLE but Unverified (must prove in Phase 0)

| Claim | Risk if wrong | Verification |
|-------|---------------|--------------|
| Empty `mcp_tools` ⇒ no Cursor-side tool execution | Server may still invoke built-in tools | Capture Run request/response with empty f4; confirm no `exec_server_message` |
| `x-cursor-client-type: cli` required for Auto unlimited | Auto works on subscription but routes to paid model | Soak test: compare usage/billing class native vs CLI |
| `x-cursor-checksum` optional on agent hosts | 401/403 on all requests | A/B header matrix with live Rust client |
| Bidirectional stream + 5s heartbeats required | Hung streams or early close | Capture cursor-agent Run stream timing |
| `conversation_id` reuse gives multi-turn without checkpoints | Context loss or duplication | Two-turn native test with same conversation_id |
| HTTP/2 mandatory (464 on HTTP/1.1) | Cannot use isahc | Confirmed by shunt; replicate with reqwest/hyper spike |

---

## Critical Gaps (missing from original plan)

### G1 — Multi-turn without CLI prompt serialization

PR #150 hacked multi-turn via text prompts + `--resume`. Native `AgentService` uses:

- `conversation_id` server-side state, and/or
- `KvClientMessage` checkpoint blobs (complex), and/or
- Full history in a single `UserMessage` (size limits).

**pi_agent_rust explicitly does not reimplement checkpoints** and only sends latest user text — relying on Cursor server memory via `conversation_id`.

**Plan fix**: Phase 0 spike must validate `conversation_id` persistence across two native turns before Phase 1 exit gate.

### G2 — Auth storage is SQLite, not JSON

Tokens live in `state.vscdb` (`cursorAuth/accessToken`). No `rusqlite` in workspace yet.

**Plan fix**: Add `rusqlite` (bundled) to `n00n-providers` for `auth.rs`; read `~/.cursor/cli-config.json` as secondary source.

### G3 — Thinking stream semantics

Cursor docs: thinking events suppressed in `--print` stream-json mode. Native wire may emit `thinking_delta` — unverified for n00n harness.

**Plan fix**: Document in contract; test in Phase 1 live stream.

### G4 — `isahc` cannot do HTTP/2 agent calls

Workspace HTTP is curl-backed HTTP/1.1 only. Native provider **requires new dependency** (hyper or reqwest + http2).

**Plan fix**: Phase 0 spike picks reqwest 0.12 + rustls; run `cargo deny check` before merge.

### G5 — Tool loop vs AgentService exec channel

Even with empty `mcp_tools`, server might emit tool frames. Plan says "typed error" — acceptable for v1, but **not** full n00n tool parity until we either map `mcp_args` → n00n tools or confirm server never emits exec when f4 empty.

### G6 — Default model slug mismatch

Built-in default is still `cursor/composer-2.5` in inventory; user hard-requires Auto.

**Plan fix**: Change default to `cursor/default` or `cursor/auto` once native lands; map both to wire `default`.

### G7 — Supply chain / maintenance

Protocol RE is intentional portfolio work but creates **ongoing breakage risk** when Cursor ships new CLI bundles weekly.

**Mitigation**: Env override `N00N_CURSOR_CLIENT_VERSION`; weekly live test gate; document in quickstart.

---

## Hallucination Check (community sources)

| Source | Trust level | Notes |
|--------|-------------|-------|
| shunt `cursor/agent.rs` | High for 2026 wire | Matches cursor-agent bundle version string |
| pi_agent_rust `cursor.rs` | Medium | api2 URL likely stale; frame codec still valid |
| eisbaw cursor_api_demo | Medium for auth | aiserver path older than agent.v1 |
| cursor-opencode-provider | High for GetServerConfig | TS protobuf committed |

---

## Recommended Plan Adjustments (applied)

1. Phase 0 exit gate expanded: **successful GetUsableModels from Rust** (not curl), plus **two-turn conversation_id** spike.
2. Phase 1 scope: `default`/`auto` streaming + sqlite auth (not API-key-only).
3. Phase 2: full `GetUsableModels` catalog replaces static registry (user v1 requirement).
4. Legacy CLI demoted only after SC-002 soak — keep `N00N_CURSOR_TRANSPORT=cli`.
5. Add `adversarial-review.md` + update `research.md` experiment matrix with curl failure notes.

---

## Verdict

**Proceed with implementation**, but treat Phase 0 as **blocking research**, not parallel paperwork. The plan's architecture (native Connect, n00n harness, dynamic discovery) matches user intent. The highest risk to project success is **multi-turn state** and **proving Auto entitlements without the CLI subprocess** — not frame codec complexity.
