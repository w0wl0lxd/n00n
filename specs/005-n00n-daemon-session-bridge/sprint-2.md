# Sprint 2 — Agent control hardening & polish

Parent: PR #149 (`feat/n00n-daemon-session-bridge`). Spec: `spec.md`.

## Goal

Close the gap between “feature complete on branch” and “shippable product surface”: docs, plugin tests, CLI ergonomics, and CI coverage for non-Linux transports.

## Status: complete

| # | Item | Status |
|---|------|--------|
| 1 | **Mark #149 ready / merge** | Ready after smoke gate |
| 2 | **`agent_control` Lua spec tests** | `plugins/agent_control/tests/spec.lua` + `n00n-lua/tests/spec.rs` |
| 3 | **`--state-dir` on agent control verbs** | Done (`a72f77189`) |
| 4 | **User docs: `n00n agent` command page** | `site/docs/content/agent/_index.md` |
| 5 | **TUI resume via daemon for paused-team** | `tui_bridge::resume_one` + registry callback |
| 6 | **Windows CI smoke** | `tcp_client_server_health_and_list` (`#[cfg(windows)]`) |
| 7 | **Stale `daemon.lock` recovery** | `lock::sweep_stale` on resolve/bind |

## Verification gate

```bash
./scripts/smoke-daemon.sh
cargo test -p n00n-lua --test spec agent_control
N00N_STATE_DIR=/tmp/n00n-smoke ./target/debug/n00n agent list --state-dir /tmp/n00n-smoke
```

Windows runner picks up `tcp_client_server_health_and_list` via `just test`.
