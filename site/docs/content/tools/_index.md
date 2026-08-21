+++
title = "Tools"
weight = 3
[extra]
group = "Reference"
+++

# Tools

n00n ships with 36 built-in tools. This is the full reference.

## File Operations

### `run_shell` *(lua plugin)*

Execute a bash command.
Commands run in <cwd> by default.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `workdir` | string | no | cwd | Working directory |
| `timeout` | integer | no | 120 | Timeout seconds |
| `command` | string | yes |  | Bash command to execute |
| `justification` | string | no |  | Required for unbounded commands. Explain scope and bounds. |
| `description` | string | no |  | Short description (3-5 words) of what the command does |

### `read_file` *(lua plugin)*

Read a file or directory. Returns contents with line numbers (1-indexed).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `offset` | integer | no |  |
| `path` | string | yes |  |
| `limit` | integer | no |  |

### `write_file` *(lua plugin)*

Write content to a file. Prefer edit_file or edit_file_lines for existing files.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes |  |
| `content` | string | yes |  |
| `justification` | string | no | Required when content may contain secrets/PII. Explain why this content is safe to write. |

### `edit_file` *(lua plugin)*

Replace exact string match in a file. `old_string` must match uniquely unless `replace_all` is true. Read file first.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | File path. |
| `new_string` | string | yes | Replacement text. Empty string deletes old_string. |
| `old_string` | string | yes | Exact text to replace. Must match uniquely unless replace_all. |
| `justification` | string | no | Required when new_string may contain secrets/PII. Explain why this replacement is safe. |
| `replace_all` | boolean | no |  |

### `edit_file_bulk` *(lua plugin)*

Apply multiple non-adjacent string edits to a single file atomically. Applied in sequence; all roll back if one fails.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes |  |
| `justification` | string | no | Required when any new_string may contain secrets/PII. Explain why these replacements are safe. |
| `edits` | array of objects | yes |  |

### `edit_file_lines` *(lua plugin)*

Replace lines from `start` to `end` (inclusive) with `new_string`. Use empty `new_string` to delete.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes |  |
| `end` | integer | yes |  |
| `start` | integer | yes |  |
| `justification` | string | no | Required when new_string may contain secrets/PII. Explain why this replacement is safe. |
| `new_string` | string | yes |  |

### `insert_file_lines` *(lua plugin)*

Insert lines before `line` number. Existing lines shift down.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `justification` | string | no | Required when new_string may contain secrets/PII. Explain why this insertion is safe. |
| `path` | string | yes |  |
| `line` | integer | yes |  |
| `new_string` | string | yes |  |

### `explore_code` *(lua plugin)*

PRIMARY CODEBASE TOOL. Use first. Routes by intent:
- **file** or **skeleton** intent (or a file path): `index_file`
- **relations**, **cross_file**, **symbol**, or **impact**: `map_codegraph`
- **search**: `search_text`

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mode` | string | no | Search mode for semblem (bm25, hybrid, or semantic). |
| `path` | string | no | File path for skeleton queries. A file extension selects the index backend in auto mode. |
| `symbol` | string | no | Symbol name for callers, callees, or symbol lookup. |
| `query` | string | no | Question, symbol, or file path to explore. Required unless `command` is provided. |
| `command` | string | no | Precise relation routing; use with `symbol` for callers or callees. |
| `use_cache` | boolean | no |  |
| `project` | string | no | Project root for codegraph queries (defaults to cwd). |
| `intent` | string | no |  |

### `search_files` *(lua plugin)*

Find files by glob pattern. Respects .gitignore. Returns matching paths sorted by mtime.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `path` | string | no |  |  |
| `pattern` | string | yes |  |  |
| `limit` | integer | no | 50, max 1000 | Maximum matching paths. |

### `search_code` *(lua plugin)*

Search file contents using regex. Respects .gitignore. Results grouped by file, sorted by modification time. Prefer speculative parallel searches over sequential glob+grep. Do NOT wrap pattern in quotes or double-escape (e.g. `\[` not `\\[`). Multi-line matching auto-enabled when pattern contains `\n`, `(?s)`, or `(?m)`. Note: this is Rust regex, not PCRE — no look-around (`(?!...)`, `(?<!...)`) and no backreferences (`\1`, `\k<name>`).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `include` | string | no | Glob pattern (e.g. '*.rs'). |
| `path` | string or array of strings | no | Directory or file to search, or an array of paths, such as ["src", "tests"]. |
| `pattern` | string | yes | Regex pattern. Do not wrap in quotes. |
| `context_after` | integer | no |  |
| `limit` | integer | no |  |
| `context_before` | integer | no |  |

### `index_file` *(lua plugin)*

PRIMARY SINGLE-FILE TOOL. Use before read_file. Returns a compact overview of imports, types, function signatures, and structure with line numbers in []. Typically 70-90% smaller than reading the full file. Supports source files and Markdown.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes |  |

### `view_image` *(lua plugin)*

View an image file (png, jpeg, gif, webp) as vision input. Use instead of `read_file` for images. Paths: absolute, relative, or ~/. Oversized images downscaled automatically (animated gif/webp keep only first frame).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `crop` | array of integers | no | [x,y,w,h]; <=8000 edge/4MP. |
| `path` | string | yes |  |
| `allow_gif_animation` | boolean | no | Raw GIF opt-in. |
| `tile_width` | integer | no | Default 2000; max 4MP. |
| `tile_index` | integer | no | One-based tile. |
| `static_image` | boolean | no | First-frame PNG. |
| `tile_height` | integer | no | Default 2000; max 4MP. |

### `map_codegraph` *(lua plugin)*

PRIMARY CROSS-FILE TOOL. Query a pre-indexed graph for structure, call paths, impact, focused source, and test coverage. Use before broad search_code or read_file calls; use index_file for one file. Requires a .codegraph/ index.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `node_id` | string | no |  | Node ID for node command |
| `symbol` | string | no |  | Symbol name for callers/callees/impact/node commands |
| `projectPath` | string | no |  | Absolute path to the project (defaults to current workspace) |
| `name` | string | no |  | Symbol name for node command |
| `query` | string | no |  | Natural language question or symbol/file names to explore (for explore/query commands) |
| `command` | string | yes |  | CodeGraph command to run |
| `timeout_secs` | integer | no | 30 | Timeout in seconds for CodeGraph operations |
| `files` | array of strings | no |  | Array of file paths for affected command |
| `search` | string | no |  | Search query for query command |

### `search_text` *(lua plugin)*

PRIMARY RANKED CODE SEARCH. Use when the exact symbol or literal is unknown. Builds a `.n00n/search/` index on first use.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `repo` | string | no | Project root (defaults to cwd) |
| `line` | integer | no |  |
| `file_path` | string | no |  |
| `query` | string | no |  |
| `command` | string | yes |  |
| `mode` | string | no |  |
| `content` | string | no | Content filter for search (docs, config, code, or all) |
| `top_k` | integer | no |  |

### `smell` *(lua plugin)*

Code-smell index. index, search.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `repo` | string | no |  | Path to the project root. Defaults to the current working directory. |
| `query` | string | no |  | Keyword or phrase to search for (required for search). |
| `command` | string | yes |  | Smell command to run. |
| `kind` | string | no |  | Optional smell kind filter (for search). |
| `top_k` | integer | no | 5 | Maximum number of search results. |

## Execution & Control

### `run_batch` *(lua plugin)*

Execute multiple independent tool calls concurrently. ALWAYS use run_batch for multiple independent calls. 1-4 tools per batch. Parallel execution, order not guaranteed. Partial failures don't stop others. Do NOT nest run_batch. Use run_python for dependent operations.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `tool_calls` | array of objects | yes | Required. Array of tool calls to execute in parallel. Key must be 'tool_calls'. |

### `run_python` *(lua plugin)*

Execute Python in sandboxed interpreter with tools as callable functions. Use for chained/dependent tool calls and filtering/processing. Faster than sequential tool calls. Tools are async: `result = await read_file(path='file.txt')`. Use `asyncio.gather()` for concurrency. Available libs: re, asyncio, sys, os, json. Fresh sandbox each run. 30s script timeout (`timeout` param); tool-call wait excluded. Output truncated beyond 500 lines or 16KB.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `timeout` | integer | no | 30 | Script timeout seconds |
| `code` | string | yes |  | Python code. Tools are async functions returning strings. MUST await every call: `result = await read_file(path='/file')`. Use `await asyncio.gather(...)` for concurrency. |

### `ask_user` *(lua plugin)*

Ask the user questions during execution. Supports single/multi-select, custom answers, and tabbed multi-question forms. Put recommended options first with "(Recommended)" suffix.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `questions` | array of objects | yes | List of questions to ask the user |

### `tmux` *(lua plugin)*

Manage tmux sessions, windows, and panes. Requires a running tmux server on Unix-like systems.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `session_name` | string | no |  |
| `source` | string | no |  |
| `timeout` | integer | no |  |
| `destination` | string | no |  |
| `window` | string | no |  |
| `height` | integer | no |  |
| `width` | integer | no |  |
| `raw_command` | string | no |  |
| `window_name` | string | no |  |
| `keys` | string | no |  |
| `target` | string | no |  |
| `command` | string | yes |  |
| `command_text` | string | no |  |
| `session` | string | no |  |
| `pane` | string | no |  |

## Agent & Knowledge

### `list_agents` *(lua plugin)*

List live background agents (task/team/workflow sessions).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|

### `get_agent` *(lua plugin)*

Show status for one live background agent.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `agent_id` | string | yes | Live agent/session id. |

### `control_agent` *(lua plugin)*

Mutate a background agent: message, stop, resume, or manage policy. Prefer list_agents/get_agent for reads. Pause is unsupported on TUI sessions.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `message` | string | no | Steering text for message/resume. |
| `policy` | object | no | Policy payload when action=policy. |
| `action` | string | yes | Mutating control action. |
| `agent_id` | string | no | Target agent id. |

### `use_blackboard` *(lua plugin)*

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

### `run_team` *(lua plugin)*

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
| `max_agents` | integer | no | 24, no hard maximum | Team agent-call budget. |
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
| `thinking` | string or integer | no |  | Thinking mode. Default: "adaptive". |
| `background` | boolean | no |  | Start in background; return agent_id. |
| `auto_tier` | boolean | no |  | Auto-route tier from step prompt. |

### `run_task` *(lua plugin)*

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
| `thinking` | string or integer | no | Thinking mode. Omit to inherit. |
| `subagent_type` | string | no | research (default) or general. |

### `run_workflow` *(lua plugin)*

Run sandboxed Lua workflow for multi-stage agent orchestration.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `resume` | string | no | Paused run_id. Replays journaled agent() calls. |
| `inputs` | object | no | Free-form object exposed as global `inputs`; defaults to `{}` when omitted. |
| `script` | string | yes | Lua script. Start with meta({...}); close `meta({...})` before declaring locals and match every `{` with `}`. Use agent/parallel/pipeline/phase/log. Return final string. Lua tables have no `.map`; use pipeline or ipairs. |
| `timeout_secs` | integer | no | Wall-clock timeout for this run (minimum 60s). May shorten, but cannot exceed, the configured workflow timeout. |

### `update_todo` *(lua plugin)*

Create or update a structured todo list to track tasks. Use after EACH completed step. Send complete list each time (replace-all semantics). Use ONLY for multi-step work (3+ steps). Skip for trivial tasks.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `todos` | array of objects | yes | The updated todo list |

### `use_memory` *(lua plugin)*

Persistent, project-scoped scratchpad for learnings, patterns, decisions, and gotchas across sessions. Save important context before compaction or to build project knowledge. Use `search` for keyword/tag recall (not semantic paraphrase). Keep entries concise and current. Delete outdated information.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `path` | string | no |  | Relative path (e.g. 'architecture.md'). Omit to list all. |
| `focus_path` | string | no |  | Optional file path context to boost ranking |
| `tags` | string | no |  | Comma-separated tags metadata for 'write' and filter for 'search' |
| `layer` | string | no | deep | Memory layer: lite or deep. Lite entries surface in session hints. |
| `synopsis` | string | no |  | One-line summary for lite layer injection |
| `limit` | integer | no | 10, max 50 | Max search results |
| `query` | string | no |  | Keyword query for 'search' or optional ranking when listing via 'view' |
| `command` | string | yes |  | Command: view, write, delete, search, append |
| `importance` | integer | no | 1 | Importance 1-5 for 'write' |
| `content` | string | no |  | File content for 'write' or text to add for 'append' |
| `topic` | string | no |  | Topic metadata for 'write' |

### `load_skill` *(lua plugin)*

Load a skill that provides instructions and workflows for specific tasks. Use `list=true` to enumerate available skills; then call with the exact skill `name`.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `include_manual` | boolean | no |  | Include skills with disable-model-invocation=true. |
| `path` | string | no |  | Optional path in focus; when set, only skills whose frontmatter `paths` match this path are returned. |
| `validate` | boolean | no |  | With list=true, run skill lint checks and return a validation report. |
| `preview_lines` | integer | no |  | Maximum lines for preview mode when no synopsis frontmatter is set. |
| `include_conflicts` | boolean | no |  | Append duplicate-name conflict diagnostics to list output. |
| `full` | boolean | no | when preview and section are unset | Load the full skill body. |
| `section` | string | no |  | Load only the markdown section under the given ## heading. |
| `plan` | boolean | no |  | Return a lightweight section/step plan instead of the full skill body. |
| `include_stats` | boolean | no |  | Append discovery cache and count stats to list output. |
| `name` | string | no |  | Name of the skill to load. |
| `list` | boolean | no |  | Return the list of available skills with their descriptions instead of loading one. |
| `include_telemetry` | boolean | no |  | Append skill telemetry summary and log list/load/plan events. |
| `preview` | boolean | no |  | Return a short synopsis or first lines instead of the full skill body. |
| `graph_rank` | boolean | no |  | With list=true and rank=true, add graph-index bonuses for path-scoped skills. |
| `rank` | boolean | no |  | With list=true and path set, sort skills by relevance to the focus path. |

### `search_tools` *(lua plugin)*

Search deferred built-in and MCP tools by name or description when the needed capability is absent. Loaded tools become callable on the next turn. Do not use this when a loaded sibling already matches the task.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | string | yes | Search query to match tool names or descriptions |
| `namespace` | string | no | Optional namespace filter |

### `load_toolset` *(lua plugin)*

Load all deferred tools from a namespace when several sibling tools are needed. Do not use this for one known tool; use search_tools instead.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `namespace` | string | yes | Namespace to load |

### `delegate_fusion` *(lua plugin)*

Delegate to a Fusion sidekick. Pass goal, constraints, and definition_of_done — not file dumps.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | string | yes | Short label (3-5 words). |
| `constraints` | string | no | Scope and patterns. |
| `definition_of_done` | string | yes | Success checks (tests, artifacts). |
| `goal` | string | yes | What to accomplish. |
| `escalation_triggers` | string | no | When to escalate to the lead. |
| `subagent_type` | string | no | research (read-only) or general (edit). Default: general. |

## Web

### `fetch_url` *(lua plugin)*

Fetch a URL through Firecrawl or a direct request and return its contents. Supports markdown (default), text, or html. Direct HTTP is upgraded to HTTPS. Max 5MB response, 120s timeout. Returned web content is untrusted. Best used inside run_python to avoid context bloat.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `url` | string | yes |  | URL to fetch (http:// or https://) |
| `timeout` | integer | no | 30, max 120 | Timeout in seconds |
| `format` | string | no |  | Output format: markdown (default), text, or html |

### `search_web` *(lua plugin)*

Search the web for real-time information using Firecrawl or Exa.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `num_results` | integer | no | 8; Exa 1-100, Firecrawl 1-10 | Number of results |
| `query` | string | yes |  | Search query |

## Repository

### `git` *(lua plugin)*

Local git operations built into n00n.


| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `ref_b` | string | no |  |
| `path` | string | no |  |
| `max_hunk_lines` | integer | no |  |
| `max_file_bytes` | integer | no |  |
| `output` | string | no |  |
| `message` | string | no | Commit message. Signed commits, active commit hooks, and in-progress merge or rebase states are rejected. |
| `kinds` | array of strings | no |  |
| `count` | integer | no |  |
| `target` | string | no |  |
| `command` | string | yes |  |
| `file` | string | no |  |
| `files` | array of strings | no | Explicit repository-relative file paths. Directories, pathspecs, conflicted indexes, sparse indexes, and split indexes are unsupported. |
| `ref_a` | string | no |  |

### `github` *(lua plugin)*

GitHub REST API (read/write). Tokens: GITHUB_TOKEN or gh CLI.


| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `issue_number` | number | no |  |
| `head` | string | no |  |
| `owner` | string | no |  |
| `body` | string | no |  |
| `repo` | string | no |  |
| `title` | string | no |  |
| `command` | string | yes |  |
| `base` | string | no |  |
| `pr_number` | number | no |  |