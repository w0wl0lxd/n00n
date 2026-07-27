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
n00n agent status <id>
```

`list` prints tab-separated rows: id, backend (`tui` or `worker`), status, and title. Live TUI sessions show `backend=tui`. Background runs show `backend=worker`.

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

Several commands accept `--json` on `run` for structured output. Control verbs (`list`, `status`, `message`, etc.) speak the daemon NDJSON protocol internally; pipe through `n00n agent list` in scripts and parse the tab-separated table, or call the daemon from your own tooling using the same request shapes as the CLI.

## See also

- [Tools](/docs/tools/) — `agent_control` plugin for in-session control cards
- [Headless Mode](/docs/headless/) — one-shot `--print` and ACP embedding
