# Quickstart: Native Cursor Provider (development)

## Prerequisites

- Cursor account with Auto entitlement
- Linux dev environment with this repo on `feat/cursor-native`
- Optional: `cursor-agent` for A/B entitlement experiments and capture

## Auth setup (pick one)

```bash
# API key
export CURSOR_API_KEY="..."

# Or rely on IDE login (tokens in ~/.config/Cursor/User/globalStorage/state.vscdb)
cursor-agent status   # verify logged in
```

## Phase 0: traffic capture (bodies required)

```bash
cd /home/w0w/dev/n00n
mise run mitm-setup          # once
just cursor-capture          # Terminal A — writes spikes/cursor-capture-<stamp>/
just cursor-capture-e2e      # OR fully automated drive + validate
```

Manual drive (Terminal B while capture is running):

```bash
scripts/cursor_agent_proxied.sh -p --model auto 'Reply with exactly: pong'
# optional second turn for checkpoint blobs:
scripts/cursor_agent_proxied.sh -p --resume --model auto 'what did I ask?'
```

Validate:

```bash
just cursor-export spikes/cursor-capture-<stamp>
# PASS requires AgentService/Run with req_len>0 or resp_len>0
tail -f spikes/cursor-capture-<stamp>/live.log   # live visibility
```

### Capture footguns (fixed in scripts)

| Footgun | Effect | Fix in tree |
|---------|--------|-------------|
| `stream_large_bodies` without store | `(content missing)` empty bodies | both flags set |
| Buffering Run request fully | hung chat / no upstream | stream + store |
| `NODE_EXTRA_CA_CERTS` | ignored by cursor-agent | `NODE_TLS_REJECT_UNAUTHORIZED=0` via proxied wrapper |
| Only `HTTPS_PROXY` | agent may bypass proxy | `GLOBAL_AGENT_HTTP_PROXY` |
| Port 8080 busy | silent failure | preflight `ss` check |
| `while let Some(err)` on bad frames | infinite spin | FrameBuffer drains bad header |

## Frame fuzz (Connect codec)

```bash
just cursor-fuzz-frames
```

## Live provider tests

```bash
export N00N_CURSOR_LIVE_TESTS=1
cargo test -p n00n-providers --lib run::tests::run_default_model_live -- --nocapture
```

## Force legacy CLI transport (during transition)

```bash
export N00N_CURSOR_TRANSPORT=cli
export CURSOR_AGENT_TRUST=true
export CURSOR_AGENT_YOLO=true
```

## Model selection

```text
cursor/default    # Auto (wire: default) — P1 target
cursor/composer-2.5
```

## Troubleshooting

| Symptom | Likely cause |
|---------|----------------|
| `permission_denied` | Wrong `x-cursor-client-version` or client-type |
| `Unknown model ID: auto` | Sent display id; use wire `default` |
| HTTP 464 | HTTP/1.1 to agent host; need HTTP/2 |
| Empty response + timeout | Missing stream heartbeats |
| Capture `req_len=0 resp_len=0` on Run | Old script — pull latest + use `scripts/cursor_capture.sh` |
| localhost Connect errors in mitm | Harmless agent probes — ignore |
