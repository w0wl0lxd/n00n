# Feature Specification: Agent control scripting parity

**Branch**: `feat/agent-scripting-parity` (stacked on #149)

**Created**: 2026-07-27

**Status**: Active

## Competitor gap analysis (verified 2026-07-27)

| Capability | Claude Code | OpenCode | Cursor CLI | n00n (#149) | This PR |
|------------|-------------|----------|------------|-------------|---------|
| Machine-readable agent list | `claude agents --json` | HTTP `GET /session` | `agent ls` + stream-json | Global `--output-format json` emits raw `ControlResponse` NDJSON | `n00n agent list --json` → stable JSON array |
| Normalized lifecycle state | `state`: working, blocked, done, failed, stopped | per-session status map | varies | free-form `status` string | `state` field mapped to stable enum |
| Include completed agents | `--json --all` | list filters / archive | cross-workspace resume | shows all disk workers always | default hides terminal workers; `--all` shows them |
| Filter by project directory | `claude agents --cwd` | `directory` query param | workspace label on resume | none | `--cwd` on list |
| Interactive agent dashboard | `claude agents` TUI | requested `opencode agents` | N/A | `/sessions` in main TUI | deferred |
| Attach / respawn stopped session | `claude attach`, `claude respawn` | `opencode connect` | `agent --resume` | none | deferred |
| Tail agent logs | `claude logs <id>` | message history API | conversation API | status `output` snippet only | deferred (`status --json` exposes output) |
| HTTP REST control plane | no (local CLI) | yes | cloud API only | NDJSON UDS/TCP | deferred (by design) |
| Push status events / webhooks | hooks | SSE | stream-json | none | deferred |

## In scope

1. **`n00n agent list --json`** — JSON array of scripting views (not wire `ControlResponse`).
2. **`n00n agent status <id> --json`** — single scripting view object.
3. **`--all` on list** — include stopped/done/failed workers (default hides terminal worker rows).
4. **`--cwd <path>` on list** — filter by session working directory.
5. **`state` normalization** — map raw status to stable enum.
6. **Optional `cwd` on `AgentRecord`** — plumbed from TUI live/status payloads and worker `agent.json`.

## Out of scope

- Agent view TUI, attach/respawn, HTTP API, webhooks/SSE.
- Changing NDJSON daemon wire format (client-side enrichment only).

## Success criteria

1. `n00n agent list --json` prints a JSON array with `id`, `backend`, `state`, `status`.
2. `needs_input` TUI status maps to `state=needs_input`.
3. Default list hides stopped workers; `--all` includes them.
4. `--cwd` excludes sessions from other directories.
5. `./scripts/smoke-daemon.sh` and new unit tests pass; clippy clean.
