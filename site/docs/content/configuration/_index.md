+++
title = "Configuration"
weight = 2
[extra]
group = "Getting Started"
+++

# Configuration

Settings go in `init.lua`, a Lua script that calls `n00n.setup()`. Same language as plugins.

Two places, both optional:

- **Global**: `~/.config/n00n/init.lua`
- **Project**: `.n00n/init.lua` (relative to your working directory)

When both exist, project settings override global ones. Neither file is required.

## Example

```lua
n00n.setup({
    ui = {
        splash_animation = true,
        mouse_scroll_lines = 5,
        theme = "tokyonight",
        tool_output_lines = {
            bash = 8,
            read = 5,
        },
    },
    agent = {
        max_output_lines = 3000,
    },
    provider = {
        default_model = "anthropic/claude-sonnet-4-6",
    },
    storage = {
        max_log_files = 5,
    },
    plugins = {
        bash = { timeout_secs = 180 },
        index = { max_file_size_mb = 4 },
    },
})
```

All fields are optional. Typos in field names cause an error right away.

`n00n.setup()` can only be called once per init.lua.

## Full Reference

### Top-level

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `always_yolo` | bool | `false` | Start every session with YOLO mode (skip permission prompts, deny rules still apply) |
| `always_fast` | bool | `false` | Start every session with Anthropic fast mode (Opus only; ignored otherwise) |
| `always_workflow` | bool | `false` | Start every session with workflow mode (task callable inside code_execution) |
| `always_thinking` | bool \| string | `false` | Start every session with extended thinking (true/"adaptive", "off", an effort level ("minimal" to "max"), or a token budget) |

### `ui`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `splash_animation` | bool | `true` | - | Show splash animation on startup |
| `mascot` | bool | `true` | - | Show the n00n mascot on the idle splash screen |
| `scrollbar` | bool | `true` | - | Show vertical scrollbar in scrollable areas |
| `flash_duration_ms` | u64 | `1500` | - | Duration of flash messages (ms) |
| `typewriter_ms_per_char` | u64 | `4` | - | Typewriter effect speed (ms/char) |
| `mouse_scroll_lines` | u32 | `3` | 1 | Lines per mouse wheel scroll |
| `max_input_lines` | u32 | `20` | 1 | Maximum visible input lines |
| `show_thinking` | bool | `true` | - | When true (default), show full model reasoning live and persisted. When false, hide reasoning behind an indicator (thinking> ...) with a click-to-expand hint, both while thinking and after it completes |

### `ui.theme`

Name of the color theme to load at startup, overriding the theme you last picked interactively. If unset, n00n keeps your last selection (the built-in default on first run). An unknown name is ignored with a warning.

Available themes: `ayu_dark`, `ayu_light`, `ayu_mirage`, `carbonfox`, `catppuccin_frappe`, `catppuccin_latte`, `catppuccin_macchiato`, `catppuccin_mocha`, `dracula`, `everforest_dark`, `fleet_dark`, `github_dark`, `gruvbox`, `gruvbox_light`, `kanagawa`, `material_darker`, `monokai_pro`, `night_owl`, `nightfox`, `nord`, `onedark`, `rose_pine`, `rose_pine_dawn`, `rose_pine_moon`, `solarized_dark`, `solarized_light`, `tokyonight`, `vscode_dark_plus`, `zenburn`.

Themes use 24-bit colors. n00n detects truecolor support from the environment, terminfo, and by asking the terminal itself, and falls back to the closest 256-color match when it is missing. If detection gets it wrong, set `N00N_TRUECOLOR=1` to force truecolor or `N00N_TRUECOLOR=0` to force the fallback.

### `ui.tool_output_lines`

How many lines of output to show per tool in the UI. All values are `usize` with a minimum of 1.

| Field | Default |
|-------|---------|
| `bash` | 5 |
| `code_execution` | 5 |
| `task` | 5 |
| `workflow` | 8 |
| `index` | 3 |
| `grep` | 3 |
| `explore` | 5 |
| `read` | 3 |
| `write` | 7 |
| `web` | 3 |
| `other` | 3 |

### `agent`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_output_bytes` | usize | `51200` | 1024 | Max tool output size (bytes) |
| `max_output_lines` | usize | `2000` | 10 | Max tool output lines |
| `max_continuation_turns` | u32 | `3` | 1 | Max automatic continuation turns |
| `compaction_buffer` | u32 \| string | `20%` | - | Context reserved for compaction: token count or percent of the context window (e.g. "20%") |
| `mcp_tool_desc_max_chars` | usize | `300` | 10 | Max MCP tool description length (characters) |

### `provider`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `default_model` | String | `none` | - | Default model identifier (e.g. `anthropic/claude-sonnet-4-6`) |
| `connect_timeout_secs` | u64 | `10` | 1 | HTTP connect timeout (seconds) |
| `low_speed_timeout_secs` | u64 | `120` | 1 | Low speed timeout (seconds with less than 1 byte received) |
| `stream_timeout_secs` | u64 | `300` | 10 | Streaming response timeout (seconds) |
| `openai_coding_plan_slots` | u64 | `4` | 1 | Maximum concurrent OpenAI Coding Plan streams per account (1-8) |

### `storage`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_log_bytes_mb` | u64 | `200` | 1 | Max total log size (MB) |
| `max_log_files` | u32 | `10` | 1 | Max number of log files to keep |
| `input_history_size` | usize | `100` | 10 | Number of input history entries to retain |

## Plugins

The `plugins` table turns plugins on or off and passes options to them. All bundled plugins are on by default. Set `enabled = false` to turn one off.

Each plugin checks its own options at startup. A typo, a wrong type, or an unknown plugin name gives you a clear error right away.

The edit plugin's extra tools are options too: `plugins.edit = { multiedit = false, edit_lines = true }`. The old `tools` table is gone. If your config still uses it, n00n stops at startup and shows you the new form.

```lua
n00n.setup({
    plugins = {
        bash = { timeout_secs = 180 },
        websearch = { enabled = false },
    },
})
```

### `plugins.bash`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_output_bytes` | integer | - | - | Override `agent.max_output_bytes` for this tool. |
| `max_output_lines` | integer | - | - | Override `agent.max_output_lines` for this tool. |
| `timeout_secs` | integer | `120` | 5 | Kill the command after this many seconds. A call's `timeout` param overrides it. |

### `plugins.code_execution`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_memory_mb` | integer | `50` | 10 | Memory limit for the Python sandbox (MB). |
| `max_output_bytes` | integer | - | - | Override `agent.max_output_bytes` for this tool. |
| `max_output_lines` | integer | - | - | Override `agent.max_output_lines` for this tool. |
| `ruff_fix` | boolean | `true` | - | Run Ruff --fix --unsafe-fixes and formatting before execution when Ruff is available. |
| `timeout_secs` | integer | `30` | 5 | Script execution time budget in seconds; waiting on tool calls does not count. A call's `timeout` param overrides it. |

### `plugins.codegraph`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_output_bytes` | integer | - | - | Override `agent.max_output_bytes` for this tool. |
| `max_output_lines` | integer | - | - | Override `agent.max_output_lines` for this tool. |

### `plugins.edit`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `edit_lines` | boolean | `false` | - | Provide the opt-in `edit_lines` tool. |
| `insert_lines` | boolean | `false` | - | Provide the opt-in `insert_lines` tool. |
| `multiedit` | boolean | `true` | - | Provide the `multiedit` tool. |

### `plugins.glob`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_output_bytes` | integer | - | - | Override `agent.max_output_bytes` for this tool. |
| `max_output_lines` | integer | - | - | Override `agent.max_output_lines` for this tool. |
| `search_result_limit` | integer | `100` | 10 | Max files returned per search. |

### `plugins.grep`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_line_bytes` | integer | `500` | 80 | Skip lines longer than this many bytes. |
| `max_output_bytes` | integer | - | - | Override `agent.max_output_bytes` for this tool. |
| `max_output_lines` | integer | - | - | Override `agent.max_output_lines` for this tool. |
| `search_result_limit` | integer | `100` | 10 | Max match groups per search. A call's `limit` param overrides it. |

### `plugins.index`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_file_size_mb` | integer | `2` | 1 | Refuse to index files larger than this many MB. |

### `plugins.read`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_line_bytes` | integer | `500` | 80 | Truncate lines longer than this many bytes. |
| `max_output_lines` | integer | - | - | Override `agent.max_output_lines` for this tool. |

### `plugins.skill`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `plugin_dev` | boolean | `true` | - | Offer the builtin n00n-plugin-dev skill for writing n00n plugins. |

### `plugins.task`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `auto_tier` | boolean | `false` | - | Route each subagent's model tier from its prompt (opt-in, off by default). |
| `max_concurrent` | integer | `4` | 1 | Concurrent subagents (hard max 8). |

### `plugins.webfetch`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_output_bytes` | integer | - | - | Override `agent.max_output_bytes` for this tool. |
| `max_output_lines` | integer | - | - | Override `agent.max_output_lines` for this tool. |
| `max_response_bytes` | integer | `5242880` | 1024 | Stop reading a response after this many bytes. |

### `plugins.websearch`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_output_bytes` | integer | - | - | Override `agent.max_output_bytes` for this tool. |
| `max_output_lines` | integer | - | - | Override `agent.max_output_lines` for this tool. |
| `max_response_bytes` | integer | `5242880` | 1024 | Stop reading a response after this many bytes. |

### `plugins.workflow`

| Field | Type | Default | Min | Description |
|-------|------|---------|-----|-------------|
| `max_agents_per_run` | integer | `24` | 1 | Agent-call budget per workflow (hard max 32). |
| `max_concurrent_agents` | integer | `4` | 1 | Concurrency per parallel()/pipeline() (hard max 8). |
| `max_concurrent_workflows` | integer | `2` | 1 | Concurrent workflows (hard max 4). |
| `timeout_secs` | integer | `600` | 1 | Hard deadline for one workflow run (cancels pure-Lua runaway loops via the VM watchdog). |

## Validation

If a value is below its minimum, n00n shows a `ConfigError` with the field name, value, and minimum.

## Directory layout

n00n uses XDG directories on Linux and macOS:

| Purpose | Path |
|---------|------|
| Config | `~/.config/n00n/` (init.lua, permissions.toml, mcp.toml) |
| Data | `~/.local/share/n00n/` |
| Logs | `~/.local/logs/n00n/` |
| State | `~/.local/state/n00n/` |

## Personal Instructions

On top of `AGENTS.md`, you can add your own instructions in two places:

- `AGENTS.local.md` at project root for per-project preferences (gitignored)
- `~/.config/n00n/AGENTS.md` for preferences that apply to all projects

Both are added to the system prompt at the start of every session.
