# Quickstart: Native Cursor Provider (development)

## Prerequisites

- Cursor account with Auto entitlement
- Linux dev environment with this repo on `feat/cursor-native`
- Optional: `cursor-agent` for A/B entitlement experiments only

## Auth setup (pick one)

```bash
# API key
export CURSOR_API_KEY="..."

# Or rely on IDE login (tokens in ~/.config/Cursor/User/globalStorage/state.vscdb)
cursor-agent status   # verify logged in
```

## Phase 0: entitlement experiment

```bash
# Compare CLI vs future native spike (document results in research.md)
cursor-agent -p "ping" --model auto --output-format stream-json --yolo --trust

# List models (note display id "auto")
cursor-agent models | head -5
```

## Live tests (once implemented)

```bash
export N00N_CURSOR_LIVE_TESTS=1
cargo nextest run -p n00n-providers -- cursor_live
```

## Force legacy CLI transport (during transition)

```bash
export N00N_CURSOR_TRANSPORT=cli
export CURSOR_AGENT_TRUST=true
export CURSOR_AGENT_YOLO=true
# n00n uses subprocess adapter — tools run in Cursor, not n00n
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
| Usage limit on Auto | Header/account mismatch vs CLI |
