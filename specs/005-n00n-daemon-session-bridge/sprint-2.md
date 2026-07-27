# Sprint 2 — Agent control hardening & polish

Parent: PR #149 (landed). Spec: `spec.md`.

## Goal

Close the gap between “feature complete on branch” and “shippable product surface”: docs, plugin tests, CLI ergonomics, and CI coverage for non-Linux transports.

## In scope (ordered)

| # | Item | Rationale | Size |
|---|------|-----------|------|
| 1 | **Mark #149 ready / merge** | All spec success criteria + smoke gate green | S |
| 2 | **`agent_control` Lua spec tests** | Phase 2 tools have no `plugins/agent_control/tests/spec.lua`; regressions in tooned cards / policy path are untested | M |
| 3 | **`--state-dir` on agent list/status/message/pause/resume/stop** | Done — enables scripted smoke against temp dirs | S |
| 4 | **User docs: `n00n agent` command page** | `site/docs` has tools index but no dedicated agent-control CLI page | S |
| 5 | **TUI resume via daemon for paused-team** | #129 wired steer/control message; resume on TUI backend still typed unsupported unless paused-team path | M |
| 6 | **Windows CI smoke** | TCP transport landed; no automated test on Windows runner | M |
| 7 | **Stale `daemon.lock` recovery** | Crash mid-serve can leave lock until pid ages out; optional startup sweep when pid dead | S |

## Out of scope (defer)

- Multiplexed single-socket worker plane (per-agent socks remain source of truth).
- Remote/multi-user auth beyond Linux SO_PEERCRED + `0600` sock.
- Full E2E `n00n agent run --background` in CI (needs provider credentials); covered by mock-socket integration tests instead.
- Live TUI manual QA (automated via `tui_bridge` + registry UDS tests).

## Verification gate (sprint 2)

```bash
./scripts/smoke-daemon.sh
# after item 3:
N00N_STATE_DIR=/tmp/n00n-smoke ./target/debug/n00n agent list
```

## Suggested PR split

1. `test(agent-control): lua spec + smoke script` — items 2 + smoke doc
2. `feat(cli): --state-dir on agent control verbs` — item 3
3. `docs: n00n agent control` — item 4
4. `feat(daemon): tui resume paused-team` — item 5 (if product wants it this sprint)
