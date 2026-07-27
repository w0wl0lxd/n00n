# Contract: Cursor Connect Transport

**Feature**: 007-cursor-native-provider  
**Version**: 0.1 (draft)

## Connect Frame

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|    flags      |                  length (u32 BE)              |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       payload (length bytes)                |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

| Flag bit | Meaning |
|----------|---------|
| 0x00 | Data frame |
| 0x02 | End stream (trailer follows) |
| 0x01 | Compressed (must decompress or reject) |

**Limits** (n00n enforcer):
- Max frame payload: 16 MiB
- Max frames per turn: 100_000
- Max protobuf nesting depth: 32

## HTTP Request (AgentService/Run)

```http
POST /agent.v1.AgentService/Run HTTP/2
Host: agentn.<region>.api5.cursor.sh
Authorization: Bearer <access_token>
Content-Type: application/connect+proto
Connect-Protocol-Version: 1
Connect-Accept-Encoding: gzip
x-cursor-client-type: cli
x-cursor-client-version: cli-<version>
x-ghost-mode: false
x-cursor-streaming: true
x-request-id: <uuid>
x-original-request-id: <uuid>
te: trailers

<body: stream of Connect frames>
```

## Unary RPCs

Same auth/client headers. **Unary calls on api2 use `application/json`**, not Connect framing:

```http
POST /agent.v1.AgentService/GetUsableModels HTTP/2
Host: api2.cursor.sh
Authorization: Bearer <access_token>
Content-Type: application/json
Connect-Protocol-Version: 1
x-cursor-client-type: cli
x-cursor-client-version: cli-<version>

{}
```

Response: JSON (`GetUsableModelsResponse.models[]` with `modelId`, `displayModelId`, `aliases`).

| RPC | Request | Response |
|-----|---------|----------|
| `GetUsableModels` | `{}` | JSON model catalog |
| `GetServerConfig` | `{"telemEnabled":false}` | JSON incl. `agentUrlConfig.agentnUrl` |

Bidirectional `Run` uses Connect frames (see HTTP Request section above).

## Model Identifier Mapping (n00n → wire)

| n00n spec | Wire id |
|-----------|---------|
| `cursor/auto` | `default` |
| `cursor/default` | `default` |
| `cursor/<name>` | `<name>` |

## Provider Events (wire → n00n)

| Wire event (decoded) | `ProviderEvent` |
|----------------------|-----------------|
| Text delta | `TextDelta { text }` |
| Thinking delta | `ThinkingDelta { text }` |
| Turn ended + usage | `StreamResponse.usage` |
| Exec / mcp_args (unexpected) | `AgentError::Api` with code `cursor_tool_not_supported` |
| Trailer error JSON | `AgentError::Api` mapped by status |

## Auth Resolution Order

1. Dynamic script / custom provider resolved bearer
2. `CURSOR_API_KEY` / n00n KeyPool
3. Cursor IDE `state.vscdb` tokens (refresh if expired)
4. n00n OAuth store

Failure at all steps → `AgentError::Config` with login hint.

## Live Test Gate

Integration tests matching this contract run only when:

```bash
export N00N_CURSOR_LIVE_TESTS=1
export CURSOR_API_KEY=...   # or valid IDE tokens
```

## Open Questions (resolve in Phase 0)

- [ ] Is `x-cursor-checksum` required on agent hosts?
- [ ] Exact heartbeat interval and protobuf message type
- [ ] Minimum header set for Auto entitlement
