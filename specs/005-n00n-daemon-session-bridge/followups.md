# Stacked follow-ups for n00n-daemon / agent control

Parent: PR #149 (`feat/n00n-daemon-session-bridge`).

## Land in #149 (this PR)

| Item | Why here |
|------|----------|
| TUI process binds `daemon.sock` with `TuiCallbackBackend` + `WorkerBackend` | Spec success criterion #2: live TUI sessions appear in CLI `agent list` while UI is up. Without this, hybrid Approach 3 is CLI-only against workers. |
| Tear down listener on UI exit / `/reload` | Spec lifecycle: TUI owns sock while up; stale sock replaced on bind. |
| Scoped tools + tooned cards (already landed) | Phase 2 UX; independent of worker runtime. |

Out of #149 once the above lands: declare draft ready for review on the control-plane surface.

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

## Later / optional (not blocking merge)

| Item | Notes |
|------|-------|
| Sock ownership protocol | If standalone `n00n agent daemon` and TUI both want the sock, prefer TUI replace-on-bind (current) or advertise PID in a sidecar lockfile. |
| Windows transport | Named pipes / stub already returns typed Unavailable on non-unix. |
| Authz / peer credential checks on UDS | `0600` sock is the v1 boundary; SO_PEERCRED later if multi-user state dirs appear. |
| ACP / print-mode registration | Out of scope unless those modes need remote control. |

## Dependency sketch

```text
#149 daemon plane + TUI registration + scoped tools
  ├─ #129 control/steer + team resume   (absorbed)
  └─ #134 background worker socks/CLI   (absorbed)

Optional later: sock ownership lockfile, Windows transport, peercred, ACP registration
```

## Verification gates (any PR in this stack)

- `cargo test -p n00n-daemon`
- `cargo clippy -p n00n-daemon -p n00n --tests -- -D warnings`
- Manual: TUI up → second terminal `n00n agent list` shows `backend=tui` row
- Manual: `n00n agent pause <tui-id>` → typed unsupported (not cancel)
- Manual: `n00n agent run --background` then list/pause/message/stop
