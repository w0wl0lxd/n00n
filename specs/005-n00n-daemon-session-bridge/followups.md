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

**Base:** merge or rebase onto #149 after #149 lands (or stack `feat/agent-cli-simplify` onto #149).

| Item | Notes |
|------|-------|
| Land PR #134 background agent server (`agents/<id>/control.sock`, `agent.json`) | #149 `WorkerBackend` already proxies this layout; empty dir is OK today. |
| Align CLI: keep thin `n00n agent {list,status,…}` as daemon client; fold #134 `run --background` under same umbrella | Avoid two competing CLIs. Prefer daemon sock when present. |
| Verb×backend: worker pause/resume/stop/message via existing `ClientCommand` | Already stubbed in proxy; wire real sock protocol from #134. |
| Identity: path-safe worker ids vs TUI `N00nId` strings | Keep `backend` discriminant; never coerce one into the other. |

**Do not** pretend a multiplexed worker plane exists before #134’s per-agent socks land.

## Stacked PR-C — Steer / control wire (PR #129)

**Base:** #149 + preferably after #129’s agent/UI control-role work, or stack #129 first then adapt.

| Item | Notes |
|------|-------|
| Extend `SessionRequest::Prompt` with `steer` / `control` (default false) | Spec Phase 1a leftover; #129 already teaches agent/UI about control messages. |
| Daemon `MessageOpts` → TUI `Prompt` flags | CLI `agent message` already sets `steer=true, control=true` in opts; TUI bridge currently drops them. |
| Plugin `agent_control` message path | Prefer daemon/session bridge once flags exist; keep in-process `n00n.session.prompt` for TUI-only. |

Orthogonal to sock topology; do not block #149 on it.

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
  ├─ PR-B  absorb #134 worker runtime (stacked)
  └─ PR-C  steer/control flags (#129 + MessageOpts wiring)
```

## Verification gates (any PR in this stack)

- `cargo test -p n00n-daemon`
- `cargo clippy -p n00n-daemon -p n00n --all-targets -- -D warnings`
- Manual: TUI up → second terminal `n00n agent list` shows `backend=tui` row
- Manual: `n00n agent pause <tui-id>` → typed unsupported (not cancel)
