+++
title = "Agent Control"
weight = 6
[extra]
group = "Reference"
+++

# Agent Control

`n00n agent` is the CLI for background workers and the shared control plane. Use it from scripts, another terminal, or the `agent_control` plugin while the TUI is running.

When the TUI is up, it owns `daemon.sock` (Unix) or advertises a loopback TCP port (Windows) via `daemon.lock`. Background workers register per-agent control sockets under `agents/<id>/`. The CLI prefers the daemon when it is reachable, then falls back to worker sockets on disk.

## List and status

```bash
n00n agent list
n00n agent list --json
n00n agent list --all
n00n agent list --cwd /path/to/project
n00n agent status <id>
n00n agent status <id> --json
```

`list` prints tab-separated rows: id, backend (`tui` or `worker`), status, and title. Live TUI sessions show `backend=tui`. Background runs show `backend=worker`.

`--json` emits a stable JSON array (list) or object (status) for scripting, similar to `claude agents --json`. Each row includes a normalized `state` field (`working`, `needs_input`, `idle`, `running`, `stopped`, `done`, `failed`, …) alongside the raw `status` string.

By default, stopped/completed background workers are hidden from `list`. Pass `--all` to include them (Claude `--json --all` parity).

`--cwd` filters to agents whose working directory matches the given path (TUI sessions and workers that recorded a cwd at start).

Use `--state-dir` to point at a non-default state directory (useful for tests and isolated installs):

```bash
n00n agent list --state-dir /tmp/n00n-smoke
```

## Run in background

```bash
n00n agent run --background --prompt "review auth.rs" --mode task
n00n agent run --background --id my-team --prompt "ship feature X" --mode team
```

`--background` starts a headless worker and returns immediately. The agent id is printed (or use `--id` to choose one). Team, task, and workflow modes accept the same flags as foreground `run`.

## Control verbs

```bash
n00n agent message <id> "continue with tests"
n00n agent pause <id>
n00n agent resume <id>
n00n agent stop <id>
```

`message` queues a user turn. For worker agents it goes to the worker control socket. For live TUI sessions, steer/control flags are set so the prompt is treated as an operator message.

| Verb | TUI backend | Worker backend |
|------|-------------|----------------|
| `list` / `status` | Live sessions from the UI | `agent.json` + control socket |
| `message` | Queues prompt with steer/control | Proxied to worker |
| `pause` | Unsupported | Pauses the worker loop |
| `resume` | Only when session has a paused team run | Resumes the worker |
| `stop` | Cancels the TUI session | Stops the worker |

## Worker-only daemon

If no TUI is running, start a worker-only listener:

```bash
n00n agent daemon
```

This binds the control plane with the worker backend only. The TUI replaces this listener when it starts.

## JSON output

Use `--json` on `list` and `status` for machine-readable output suitable for status bars, notifications, and orchestration scripts. The shape is intentionally stable and separate from the internal daemon NDJSON wire protocol.

`run --json` still uses the global `--output-format json` flag for one-shot foreground runs.

## See also

- [Tools](/docs/tools/) — `agent_control` plugin for in-session control cards
- [Headless Mode](/docs/headless/) — one-shot `--print` and ACP embedding
