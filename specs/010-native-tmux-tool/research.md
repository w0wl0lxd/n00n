# Native tmux tool — Research

## Summary

The tmux tool should be implemented as a pure Lua plugin (`plugins/tmux/init.lua`) using `n00n.fn.jobstart` to invoke the tmux CLI with structured output formats (`-F`). No Rust crate is required for the initial implementation; the existing `tmux_interface` crate (v0.4.0, MIT) can be considered later if control mode streaming is needed.

## Evidence

- **Issue #235** — Specifies schema with commands: `list_sessions`, `list_windows`, `list_panes`, `new_session`, `kill_session`, `new_window`, `kill_window`, `send_keys`, `capture_pane`, `run_command`, `resize`, `break_pane`, `join_pane`.
- **Epic #240** — Part of native tools expansion following existing `plugins/<name>` + optional `n00n-<name>` crate pattern.
- **Tool registration pattern** — `n00n.api.register_tool` in `n00n-lua/src/api/tool.rs:671` with schema, handler, permission_scopes, header, restore functions.
- **Job control API** — `n00n.fn.jobstart`/`jobwait` in `n00n-lua/src/api/fn.rs:284` for spawning processes and collecting output.
- **Output limits pattern** — `plugins/lib/n00n/output_limits.lua` provides shared `max_output_lines`/`max_output_bytes` options used by bash, grep, websearch, etc.
- **Permission scopes** — `ToolSource::Lua { plugin }` in `n00n-agent/src/tools/registry.rs:68` and `DEFAULT_BUILTINS` in `n00n-config/src/lib.rs:60`.
- **tmux CLI availability** — tmux 3.7b installed with key commands: `list-sessions`, `list-windows`, `list-panes`, `new-session`, `kill-session`, `send-keys`, `capture-pane`.
- **Structured output** — tmux `-F` format strings (e.g., `#{session_name}`, `#{window_index}`, `#{pane_pid}`) produce pipe-delimited structured output.
- **tmux_interface crate** — Rust library `tmux_interface` v0.4.0 (MIT) provides typed wrappers but is not required for CLI-based approach.

## Map (code)

- Entry points:
  - `plugins/tmux/init.lua` — New plugin file with `n00n.api.register_tool`
  - `n00n-config/src/lib.rs:60` — Add `"tmux"` to `DEFAULT_BUILTINS`
  - `n00n-agent/src/tools/registry.rs` — No changes needed (Lua auto-registers)

- Key symbols / files:
  - `n00n-lua/src/api/tool.rs:671` — `register_tool` function
  - `n00n-lua/src/api/fn.rs:284` — `jobstart`/`jobwait` for process spawning
  - `plugins/lib/n00n/output_limits.lua` — Output limit utilities
  - `plugins/bash/init.lua:626` — Reference for permission_scopes function, output_limits usage
  - `plugins/arbor/init.lua:72` — Reference for command dispatch pattern

- Call / data flow:
  1. Plugin loads → `n00n.api.register_tool` → `ToolRegistry::register` with `ToolSource::Lua { plugin: "tmux" }`
  2. Tool handler → `n00n.fn.jobstart("tmux ...")` → `n00n.fn.jobwait(id, timeout)` → parse stdout
  3. Permission scopes function → returns `{ scopes = { "tmux.read" }, force_prompt = false/true }`
  4. Output limits → `output_limits.resolve(opts, ctx)` → truncates at max_lines/max_bytes

## Open questions / gaps

- **Control mode streaming** — Issue mentions optional future Rust helper for tmux control mode (`-C`). Is real-time streaming required for v1, or can it wait?
- **Target selector syntax** — Issue mentions `session`, `window`, `pane` target selectors. Should these follow tmux syntax (`session_name:window_index.pane_index`) or a custom schema?
- **Cross-platform support** — tmux is Unix-only. Should the tool fail gracefully on Windows, or is Unix-only acceptable?
- **Test strategy** — Issue mentions tests in `n00n-lua/tests/real_plugins_restore.rs` or `plugins/tmux/tests/`. Which approach is preferred given tmux requires a running server?
