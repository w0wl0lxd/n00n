# Tasks: 007 Native Cursor Provider

**Branch**: `feat/cursor-native`  
**PR**: https://github.com/w0wl0lxd/n00n/pull/179

## Phase 0 — RE + spike (in progress)

- [x] Speckit + adversarial review
- [x] Legacy CLI fixes
- [x] Connect frame codec + fuzz tests (spin-loop footgun fixed)
- [x] Wire/auth/discovery JSON unary
- [x] Run encoder + HTTP/2 driver (`proto.rs`, `run.rs`)
- [x] Checkpoint blob store (`checkpoint.rs`)
- [x] Capture tooling with live tracing + body store
  - `scripts/cursor_capture.sh` + `cursor_capture_addon.py`
  - `scripts/cursor_agent_proxied.sh` (correct env)
  - `scripts/cursor_export_flows.sh` (FAIL if Run empty)
  - `scripts/cursor_capture_e2e.sh`
  - `just cursor-capture` / `cursor-capture-e2e` / `cursor-export` / `cursor-fuzz-frames`
- [x] Live capture PASS (`VALIDATION.txt` result=PASS)
- [x] Live `AgentService/Run` spike (`N00N_CURSOR_LIVE_TESTS=1`) — green via reqwest HTTP/2 streaming body (isahc `AsyncBody` stalled at `enqueued≫sent=1`). Also: gzip frames, checkpoint outbound+waker, model meta `fast=false`, ASK mode, checksum/session fingerprint headers, GetUsableModels warm, capture golden `pong` decode.
- [x] Checkpoint get/set wired into Run outbound queue (unit-tested; two-turn live capture still open)
- [ ] Two-turn checkpoint replay (needs stable agentn through mitm or SSLKEYLOG)
- [ ] Auto entitlement matrix

## Phase 1+ — see plan.md
