+++
title = "Tools"
weight = 3
[extra]
group = "Reference"
+++

# Tools

n00n ships with 30 built-in tools. This is the full reference.

## File Operations

### `bash` *(lua plugin)*

Execute a bash command.
Commands run in <cwd> by default.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `workdir` | string | no | cwd | Working directory |
| `timeout` | integer | no | 120 | Timeout seconds |
| `command` | string | yes |  | Bash command to execute |
| `justification` | string | no |  | Required when command is broad/unbounded. Explain scope and bound assumptions. |
| `description` | string | no |  | Short description (3-5 words) of what the command does |

### `read` *(lua plugin)*

Read a file or directory. Returns contents with line numbers (1-indexed).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `offset` | integer | no |  |
| `path` | string | yes |  |
| `limit` | integer | no |  |

### `write` *(lua plugin)*

Write content to a file. Prefer edit or edit_lines for existing files.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `content` | string | yes |  |
| `path` | string | yes |  |

### `edit` *(lua plugin)*

Replace exact string match in a file. `old_string` must match uniquely unless `replace_all` is true. Read file first.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `replace_all` | boolean | no |  |
| `path` | string | yes |  |
| `old_string` | string | yes |  |
| `new_string` | string | yes |  |

### `multiedit` *(lua plugin)*

Apply multiple non-adjacent string edits to a single file atomically. Applied in sequence; all roll back if one fails.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `edits` | array | yes |  |
| `path` | string | yes |  |

### `edit_lines` *(lua plugin)*

Replace lines from `start` to `end` (inclusive) with `new_string`. Use empty `new_string` to delete.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `start` | integer | yes |  |
| `path` | string | yes |  |
| `new_string` | string | yes |  |
| `end` | integer | yes |  |

### `insert_lines` *(lua plugin)*

Insert lines before `line` number. Existing lines shift down.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes |  |
| `line` | integer | yes |  |
| `new_string` | string | yes |  |

### `glob` *(lua plugin)*

Find files by glob pattern. Respects .gitignore. Returns matching paths sorted by mtime.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `pattern` | string | yes |  |
| `path` | string | no |  |

### `grep` *(lua plugin)*

Search file contents using regex. Respects .gitignore. Results grouped by file, sorted by modification time. Prefer speculative parallel searches over sequential glob+grep. Do NOT wrap pattern in quotes or double-escape (e.g. `\[` not `\\[`). Multi-line matching auto-enabled when pattern contains `\n`, `(?s)`, or `(?m)`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `include` | string | no |  |
| `path` | string | no |  |
| `pattern` | string | yes |  |
| `context_after` | integer | no |  |
| `limit` | integer | no |  |
| `context_before` | integer | no |  |

### `index` *(lua plugin)*

Return a compact overview of a source file: imports, types, function signatures, and structure with line numbers in []. ~70-90% more efficient than reading full file. Use FIRST to understand structure before read with offset/limit. Supports source files and markdown. Falls back with error on unsupported languages.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes |  |

### `view_image` *(lua plugin)*

View an image file (png, jpeg, gif, webp) as vision input. Use instead of `read` for images. Paths: absolute, relative, or ~/. Oversized images downscaled automatically (animated gif/webp keep only first frame).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `crop` | array | no | [x,y,w,h]; <=8000 edge/4MP. |
| `path` | string | yes |  |
| `allow_gif_animation` | boolean | no | Raw GIF opt-in. |
| `tile_width` | integer | no | Default 2000; max 4MP. |
| `tile_index` | integer | no | One-based tile. |
| `static_image` | boolean | no | First-frame PNG. |
| `tile_height` | integer | no | Default 2000; max 4MP. |

### `codegraph` *(lua plugin)*

Query a semantic codegraph for cross-file structural analysis. Missing or stale project indexes are initialized or refreshed automatically. Returns verbatim source grouped by file plus a blast-radius summary. Typically cheaper than broad grep + read for cross-file questions.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `projectPath` | string | no | Project path (defaults to workspace). |
| `query` | string | yes | Question or symbol/file names to explore. |

### `arbor` *(lua plugin)*

Graph-based code analysis using Arbor. Returns compact caller/callee/project maps; prefer over broad grep for relationship/impact questions.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `command` | string | yes |  |
| `token_budget` | integer | no |  |
| `project` | string | no |  |
| `symbol` | string | no |  |

## Execution & Control

### `batch` *(lua plugin)*

Execute multiple independent tool calls concurrently. ALWAYS use batch for multiple independent calls. 1-25 tools per batch. Parallel execution, order not guaranteed. Partial failures don't stop others. Do NOT nest batch. Use code_execution for dependent operations.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `tool_calls` | array | yes | Array of tool calls to execute in parallel |

### `code_execution` *(lua plugin)*

Execute Python in sandboxed interpreter with tools as callable functions. Use for chained/dependent tool calls and filtering/processing. Faster than sequential tool calls. Tools are async: `result = await read(path='file.txt')`. Use `asyncio.gather()` for concurrency. Available libs: re, asyncio, sys, os, json. Fresh sandbox each run. 30s script timeout (`timeout` param); tool-call wait excluded. Output truncated beyond 500 lines or 16KB.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `timeout` | integer | no | 30 | Script timeout seconds |
| `code` | string | yes |  | Python code. Tools are async functions returning strings. MUST await every call: `result = await read(path='/file')`. Use `await asyncio.gather(...)` for concurrency. |

### `question` *(lua plugin)*

Ask the user questions during execution. Supports single/multi-select, custom answers, and tabbed multi-question forms. Put recommended options first with "(Recommended)" suffix.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `questions` | array | yes | List of questions to ask the user |

## Agent & Knowledge

### `agent_list` *(lua plugin)*

List live background agents (task/team/workflow sessions).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|

### `agent_status` *(lua plugin)*

Show status for one live background agent.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `agent_id` | string | yes | Live agent/session id. |

### `agent_control` *(lua plugin)*

Mutate a background agent: message, stop, resume, or manage policy. Prefer agent_list/agent_status for reads. Pause is unsupported on TUI sessions.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `message` | string | no | Steering text for message/resume. |
| `policy` | object | no | Policy payload when action=policy. |
| `action` | string | yes | Mutating control action. |
| `agent_id` | string | no | Target agent id. |

### `blackboard` *(lua plugin)*

Shared coordination for multi-agent sessions. Post observations, claim tasks atomically, query state.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `post` | object | no | Post data. |
| `only_active` | boolean | no | Active claims only. |
| `action` | string | yes | Action. |
| `query` | object | no | Query filters. |
| `status` | string | no | Status. |
| `post_id` | string | no | Post id. |
| `task_id` | string | no | Task id. |
| `claim` | object | no | Claim data. |

### `team` *(lua plugin)*

Run ALMAS team for SDLC goal. supervised=plan, autonomous=execute, swarm=decentralized rounds. background returns agent_id.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `human_escalation` | boolean | no |  | Pause on step failure; return run_id. |
| `resume` | string | no |  | Paused run_id to resume. |
| `ibn_gate` | boolean | no |  | Use information-bottleneck gate in swarm. |
| `goal` | string | yes |  | Goal. |
| `timeout_secs` | integer | no | 1800s | Wall-clock timeout before the team run is aborted. |
| `mode` | string | no |  | supervised=plan, autonomous=run, swarm=decentralized. |
| `waves` | boolean | no |  | Execute in waves with validation gates. |
| `max_wave_retries` | integer | no |  | Validation gate retries. |
| `max_agents` | integer | no | 16, no hard maximum | Team agent-call budget. |
| `checkpoints` | boolean | no |  | Persist checkpoints after each wave. |
| `compact` | boolean | no |  | TOON-encode retrieved context. |
| `model_tier` | string | no |  | Supervisor tier (weak/medium/strong). |
| `continue` | string | no |  | Human guidance when resuming. |
| `max_steps` | integer | no |  | Plan steps. |
| `max_concurrent` | integer | no |  | Swarm concurrency. |
| `quorum` | boolean | no |  | Require validator quorum. |
| `max_rounds` | integer | no |  | Swarm rounds. |
| `use_retrieval` | boolean | no |  | Ground steps with repo retrieval. |
| `model` | string | no |  | Exact model override. |
| `use_summary` | boolean | no |  | Use Summary Agent index for retrieval. |
| `thinking` | string/integer | no |  | Thinking mode. Default: "adaptive". |
| `background` | boolean | no |  | Start in background; return agent_id. |
| `auto_tier` | boolean | no |  | Auto-route tier from step prompt. |

### `task` *(lua plugin)*

Launch isolated agent; combine independent calls with batch. research (default) = read-only; general = can edit. Each call starts fresh; include context and ask for concise file:line results. Summarize returned results. auto_tier opt-in. background returns agent_id.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | string | yes | Task summary (3-5 words). |
| `model_tier` | string | no | Tier: weak/medium/strong. |
| `auto_tier` | boolean | no | Auto-route tier from prompt. |
| `background` | boolean | no | Start in background; return agent_id immediately. |
| `model` | string | no | Exact model override. |
| `output_schema` | object | no | Output JSON schema. Result returned as validated JSON string. |
| `prompt` | string | yes | Task prompt. |
| `thinking` | string/integer | no | Thinking mode. Omit to inherit. |
| `subagent_type` | string | no | research (default) or general. |

### `workflow` *(lua plugin)*

Run sandboxed Lua workflow for multi-stage agent orchestration.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `inputs` | object | no | Free-form object exposed as global `inputs`. |
| `script` | string | yes | Lua script. Start with meta({...}). Use agent/parallel/pipeline/phase/log. Return final string. |
| `resume` | string | no | Paused run_id. Replays journaled agent() calls. |

### `todo_write` *(lua plugin)*

Create or update a structured todo list to track tasks. Use after EACH completed step. Send complete list each time (replace-all semantics). Use ONLY for multi-step work (3+ steps). Skip for trivial tasks.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `todos` | array | yes | The updated todo list |

### `memory` *(lua plugin)*

Persistent, project-scoped scratchpad for learnings, patterns, decisions, and gotchas across sessions. Save important context before compaction or to build project knowledge. Keep entries concise and current. Delete outdated information.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `command` | string | yes | Command: view, write, delete |
| `path` | string | no | Relative path (e.g. 'architecture.md'). Omit to list all. |
| `content` | string | no | File content for 'write' |

### `skill` *(lua plugin)*

Load a skill that provides instructions and workflows for specific tasks. Use `list=true` to enumerate available skills; then call with the exact skill `name`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | no | Name of the skill to load. |
| `list` | boolean | no | Return the list of available skills with their descriptions instead of loading one. |

### `tool_search` *(lua plugin)*

Search for deferred tools by name or description. Returns a list of tools that can be loaded on demand.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | string | yes | Search query to match tool names or descriptions |
| `namespace` | string | no | Optional namespace filter |

### `load_namespace` *(lua plugin)*

Load all tools from a namespace. Returns the list of tools that were loaded.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `namespace` | string | yes | Namespace to load |

## Web

### `webfetch` *(lua plugin)*

Fetch a URL and return its contents. Supports markdown (default), text, or html. HTTP auto-upgraded to HTTPS. Max 5MB response, 120s timeout. Best used inside code_execution to avoid context bloat.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `url` | string | yes |  | URL to fetch (http:// or https://) |
| `timeout` | integer | no | 30, max 120 | Timeout in seconds |
| `format` | string | no |  | Output format: markdown (default), text, or html |

### `websearch` *(lua plugin)*

Search the web for real-time information using Exa AI.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `num_results` | integer | no | 8 | Number of results to return |
| `query` | string | yes |  | Search query |