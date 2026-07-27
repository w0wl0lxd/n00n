# Stacked follow-ups for n00n-daemon / agent control

Parent: PR #149 (`feat/n00n-daemon-session-bridge`).

## Land in #149 (this PR)

| Item | Why here |
|------|----------|
| TUI process binds `daemon.sock` with `TuiCallbackBackend` + `WorkerBackend` | Spec success criterion #2: live TUI sessions appear in CLI `agent list` while UI is up. Without this, hybrid Approach 3 is CLI-only against workers. |
| Tear down listener on UI exit / `/reload` | Spec lifecycle: TUI owns sock while up; stale sock replaced on bind. |
| Scoped tools + tooned cards (already landed) | Phase 2 UX; independent of worker runtime. |

Out of #149 once the above lands: **ready for review** — run `./scripts/smoke-daemon.sh` before merge.

## Stacked PR-B — Absorb / land worker runtime (PR #134)

**Status:** Absorbed into #149 (same branch). `n00n agent run --background` owns per-agent `control.sock`; list/status/message/pause/resume/stop prefer `daemon.sock` when present, else talk to worker socks / `agent.json` directly.

| Item | Notes |
|------|-------|
| Land PR #134 background agent server (`agents/<id>/control.sock`, `agent.json`) | Done on #149 |
| Align CLI: keep thin control verbs as daemon-first client; `run --background` under same `n00n agent` | Done |
| Verb×backend: worker pause/resume/stop/message via existing `ClientCommand` | Done; `WorkerBackend` proxies the same layout |
| Identity: path-safe worker ids vs TUI `N00nId` strings | Keep `backend` discriminant |

**Do not** pretend a multiplexed worker plane exists before #134’s per-agent socks land.

## Stacked PR-C — Steer / control wire (PR #129)

**Status:** Absorbed into #149. `SessionRequest::Prompt { steer, control }`, control DisplayRole, team resume via `paused_team`, plugin message/resume send `steer=true, control=true`, TUI bridge forwards `MessageOpts`.

| Item | Notes |
|------|-------|
| Extend `SessionRequest::Prompt` with `steer` / `control` (default false) | Done |
| Daemon `MessageOpts` → TUI `Prompt` flags | Done |
| Plugin `agent_control` message path | Done (scoped tools + cards retained) |

## Later / optional — landed in #149

| Item | Status |
|------|--------|
| Sock ownership protocol (`daemon.lock` sidecar: pid, role, transport, endpoint) | Done — TUI may replace; worker/headless blocked while live owner exists |
| Windows transport | Done — loopback TCP listener; endpoint advertised in lock |
| Authz / peer credential checks on UDS | Done (Linux) — SO_PEERCRED uid match on accept |
| ACP / print-mode registration | Done — headless `ControlPlane` via `session_daemon`; print is list/status only |

## Dependency sketch

```text
#149 daemon plane + TUI registration + scoped tools + optional transport/lock/auth/headless
  ├─ #129 control/steer + team resume   (absorbed)
  └─ #134 background worker socks/CLI   (absorbed)
```

## Verification gates (any PR in this stack)

**Automated (run `./scripts/smoke-daemon.sh`):**

- `cargo test -p n00n-daemon` — includes UDS round-trip, worker fixture list, worker pause mock-socket, TUI pause unsupported
- `cargo test -p n00n --bins -- tui_bridge` — TUI `backend=tui` list over `daemon.sock`
- `cargo test -p n00n --bins -- agent::`
- `cargo clippy -p n00n-daemon -p n00n --tests -- -D warnings`

**Manual (optional; superseded by tests above):**

- TUI up → second terminal `n00n agent list` shows `backend=tui` row
- `n00n agent pause <tui-id>` → typed unsupported (not cancel)
- `n00n agent run --background` then list/pause/message/stop (requires provider credentials)

## Next sprint

Sprint 2 complete — see `sprint-2.md`. PR #149 ready for review after `./scripts/smoke-daemon.sh` is green.
