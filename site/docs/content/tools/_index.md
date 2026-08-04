+++
title = "Tools"
weight = 3
[extra]
group = "Reference"
+++

# Tools

n00n ships with 33 built-in tools in this full reference, including opt-in edit sub-tools.

Use `explore_code` first for a general codebase question. Choose `index_file` for one-file structure, `map_code` for graph relationships, `map_codegraph` for cross-file structure or impact, and `search_text` for ranked search. Do not treat `explore_code` intents as separate tools.

The canonical inventory contains only the tools listed below. `activate_tool` is a compatibility plugin and is not a default built-in tool; current deferred loading uses `search_tools` for one capability and `load_toolset` for a namespace.

## Explore & Search

### `explore_code` *(lua plugin)*

Unified codebase exploration router. Picks the best backend for the question:
- **file** or **skeleton** intent (or a file path): compact single-file skeleton via `index_file`
- **relations** or **trace** intent: caller/callee maps, trace paths, blast radius via `map_code`
- **cross_file** intent (default for NL questions): structural cross-file analysis via `map_codegraph`
- **search** intent: keyword or natural-language search via `search_text`
- **symbol** intent: symbol drill-down via `map_codegraph node`
- **impact** intent: blast-radius analysis via `map_codegraph impact`

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `token_budget` | integer | no | Maximum response budget for the selected backend. |
| `path` | string | no | File path for skeleton queries. A file extension selects the index_file backend in auto mode. |
| `from_symbol` | string | no | Starting symbol for a trace path. |
| `symbol` | string | no | Symbol to inspect or use as a relation endpoint. |
| `use_cache` | boolean | no | Reuse a cached exploration result when available. |
| `intent` | string | no | Router backend: auto, file, skeleton, relations, cross_file, search, symbol, impact, or trace. |
| `to_symbol` | string | no | Destination symbol for a trace path. |
| `query` | string | no | Question, symbol, or file path to explore. Required unless `command` is provided. |
| `command` | string | no | Optional precise Arbor command for relations or impact queries. |
| `mode` | string | no | Search mode for search_text (bm25, hybrid, or semantic). |
| `project` | string | no | Project root for map_code/map_codegraph queries (defaults to cwd). |

### `index_file` *(lua plugin)*

Return a compact overview of a source file: imports, types, function signatures, and structure with line numbers in []. Use first to understand structure before `read_file`. Do not use for full contents or directory discovery; use `read_file` or `search_files` instead. Supports source files and markdown, and reports an error for unsupported languages.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | Source file or markdown path to summarize. |

### `search_files` *(lua plugin)*

Find files by glob pattern. Use when you know a filename shape or directory pattern. Do not use to search file contents; use `search_code` or `search_text` instead. Use `read_file` after finding a path. Respects .gitignore and sorts matches by mtime.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `pattern` | string | yes | Glob pattern such as `src/**/*.rs`. |
| `path` | string | no | Directory or project root in which to match (defaults to cwd). |

### `search_code` *(lua plugin)*

Search file contents using regex. Use for literal or regex matches when you know the text to find. Do not use for symbol relationships or one-file structure; use `map_codegraph`, `map_code`, or `index_file` instead. Use `run_batch` for independent searches. Respects .gitignore. Results grouped by file, sorted by modification time. Prefer speculative parallel searches over sequential glob+grep. Do NOT wrap pattern in quotes or double-escape (e.g. `\[` not `\\[`). Multi-line matching auto-enabled when pattern contains `\n`, `(?s)`, or `(?m)`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `include` | string | no | Optional glob filter such as `*.rs`. |
| `path` | string | no | Directory or file root in which to search (defaults to cwd). |
| `pattern` | string | yes | Regex or literal text to find. Do not add shell-style quotes. |
| `context_after` | integer | no | Lines of context after each match. |
| `limit` | integer | no | Maximum number of matching lines to return. |
| `context_before` | integer | no | Lines of context before each match. |

### `search_text` *(lua plugin)*

Search indexed source code with BM25 keyword ranking. Use for natural-language or ranked code search after the index is available. Do not use for exact regex matches or call relationships; use `search_code` or `map_codegraph` instead. Builds a `.n00n/search/` index on first use.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `repo` | string | no | Project root (defaults to cwd). |
| `line` | integer | no | One-based line for `find_related`. |
| `file_path` | string | no | File path for `find_related`. |
| `query` | string | no | Keyword or natural-language query for `search`. |
| `command` | string | yes | Search, find related chunks for a file location, or report index savings. |
| `mode` | string | no | Ranking mode: bm25, hybrid, or semantic. |
| `content` | string | no | Content filter for search (docs, config, code, or all) |
| `top_k` | integer | no | Maximum number of result chunks. |

### `map_codegraph` *(lua plugin)*

Query a pre-indexed semantic codegraph for cross-file structural analysis. Returns verbatim source code grouped by file, plus a dependency impact "blast radius" summary with caller counts and test coverage info. Typically uses fewer tokens than broad grep + read for the same cross-file question. Use for end-to-end behavior, call paths, symbol relationships, and blast radius. Do not use without a `.codegraph/` index or for simple text search; use `search_code` or `search_text` instead. Use `index_file` for one-file structure.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `node_id` | string | no |  | Node ID for node command |
| `symbol` | string | no |  | Symbol name for callers/callees/impact/node commands |
| `projectPath` | string | no |  | Absolute project path (defaults to the current workspace). |
| `name` | string | no |  | Symbol name for node command |
| `query` | string | no |  | Natural language question or symbol/file names to explore (for explore/query commands) |
| `command` | string | yes |  | Graph operation to run, such as explore, callers, impact, node, query, or files. |
| `timeout_secs` | integer | no | 30 | Timeout in seconds for CodeGraph operations |
| `files` | array | no |  | File paths to analyze for the `affected` command. |
| `search` | string | no |  | Search query for query command |

### `map_code` *(lua plugin)*

Graph-based code analysis using Arbor. Returns structured, compact
caller/callee/project maps; prefer it over broad grep or unfiltered reads
for relationship and impact questions. Use when you need callers, callees, entry points, or change impact. Do not use for single-file structure or text search; use `index_file` or `search_code` instead. Use `map_codegraph` for semantic cross-file questions.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `token_budget` | integer | no | Maximum response budget. |
| `path` | string | no | Path for a path-scoped Arbor query. |
| `from_symbol` | string | no | Start symbol for a trace path. |
| `symbol` | string | no | Symbol for inspect, callers, or callees. |
| `command` | string | yes | Arbor operation to run. |
| `project` | string | no | Project root (defaults to cwd). |
| `operation` | string | no | Refactor operation when command is `refactor`. |
| `to_symbol` | string | no | Destination symbol for a trace path. |

## Files & Images

### `read_file` *(lua plugin)*

Read a file or directory with line numbers (1-indexed). Use before editing or when exact contents are needed. Do not use for images; use `view_image` instead. Use `index_file` first for large source files and `search_files` for discovery.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `offset` | integer | no | One-based starting line; omit to start at line 1. |
| `path` | string | yes | File or directory path to read. |
| `limit` | integer | no | Maximum lines to return; omit to use the tool limit. |

### `write_file` *(lua plugin)*

Write complete content to a new file or intentionally replace a whole file. Use when creating a file or replacing all of its contents. Do not use for targeted edits; use `edit_file`, `edit_file_bulk`, or `edit_file_lines` instead. Read first when preserving existing content.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `content` | string | yes | Complete file contents to write. |
| `path` | string | yes | File path to create or replace. |

### `edit_file` *(lua plugin)*

Replace an exact string in a file. Use for one targeted change after reading the file. Do not use to replace a whole file or several distant regions; use `write_file` or `edit_file_bulk` instead. `old_string` must match uniquely unless `replace_all` is true.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `replace_all` | boolean | no | false | Replace every match instead of requiring one unique match. |
| `path` | string | yes |  | File path to edit. |
| `old_string` | string | yes |  | Exact existing text to replace. |
| `new_string` | string | yes |  | Replacement text; empty deletes the matched text. |

### `edit_file_bulk` *(lua plugin)*

Apply multiple non-adjacent exact edits to one file atomically. Use when several independent replacements belong together. Do not use for one replacement or known line ranges; use `edit_file` or `edit_file_lines` instead. Edits run in sequence and all roll back if one fails.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `edits` | array | yes | Ordered exact edits applied atomically. |
| `path` | string | yes | File path to edit. |

### `edit_file_lines` *(lua plugin)*

Replace lines from `start` to `end` (inclusive) with `new_string`. Use for known line ranges after reading the file. Do not use for exact text matching or multiple distant edits; use `edit_file` or `edit_file_bulk` instead. Use empty `new_string` to delete.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `start` | integer | yes | First one-based line to replace. |
| `path` | string | yes | File path to edit. |
| `new_string` | string | yes | Replacement text; empty deletes the matched text. |
| `end` | integer | yes | Last one-based line to replace, inclusive. |

### `insert_file_lines` *(lua plugin)*

Insert text before a known one-based line. Use after reading the target range. Do not use for exact replacements or several distant edits; use `edit_file` or `edit_file_bulk` instead. Existing lines shift down.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | File path to edit. |
| `line` | integer | yes | One-based line before which to insert. |
| `new_string` | string | yes | Replacement text; empty deletes the matched text. |

### `view_image` *(lua plugin)*

View an image file (png, jpeg, gif, webp) as vision input. Use for visual inspection and use instead of `read_file` for images. Do not use for text files or unsupported formats; use `read_file` instead. Crop or tile only when the full image is too large.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `crop` | array | no |  | Crop rectangle [x, y, width, height]; maximum edge and pixel limits apply. |
| `path` | string | yes |  | Image path to decode (png, jpeg, gif, or webp). |
| `allow_gif_animation` | boolean | no |  | Allow the raw animated GIF when true; otherwise use a static frame. |
| `tile_width` | integer | no | 2000; provider edge limit applies | Tile width in pixels. |
| `tile_index` | integer | no |  | One-based tile index when the image is split into tiles. |
| `static_image` | boolean | no |  | Use only the first frame of an animated image. |
| `tile_height` | integer | no | 2000; provider edge limit applies | Tile height in pixels. |

## Shell & Execution

### `run_shell` *(lua plugin)*

Execute a bash command. Use for git, builds, tests, and other system commands. Do not use for file reads or edits; use the file siblings instead. Use `run_batch` for independent calls and `run_python` for dependent calls.
Commands run in <cwd> by default.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `workdir` | string | no | cwd | Working directory |
| `timeout` | integer | no | 120 | Timeout seconds |
| `command` | string | yes |  | Bash command to execute |
| `justification` | string | no |  | Required when command is broad/unbounded. Explain scope and bound assumptions. |
| `description` | string | no |  | Short description (3-5 words) of what the command does |

### `run_python` *(lua plugin)*

Execute Python in a sandboxed interpreter with tools as callable functions. Use for dependent calls, filtering, or transforming results. Do not use for one independent call; use that sibling directly, or use `run_batch` for independent calls. Tools are async: await every call and use `asyncio.gather()` for concurrency. Available libs: re, asyncio, sys, os, json. Fresh sandbox, 30s script timeout, and output truncated beyond 500 lines or 16KB.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `timeout` | integer | no | 30 | Script timeout seconds |
| `code` | string | yes |  | Python code. Tools are async functions returning strings. MUST await every call: `result = await read_file(path='/file')`. Use `await asyncio.gather(...)` for concurrency. |

### `run_batch` *(lua plugin)*

Execute multiple independent tool calls concurrently. Use when calls do not depend on each other's results. Do not use for dependent operations, filtering, or nested batches; use `run_python` instead. Use the individual sibling tool for a single call. 1-25 tools per batch, order not guaranteed, and partial failures do not stop other calls.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `tool_calls` | array | yes | Independent tool calls to execute concurrently; order is not guaranteed. |

### `ask_user` *(lua plugin)*

Ask the user questions during execution when a decision or missing input is required. Do not use when the answer is already known or can be inferred safely. Use `ask_user` for choices instead of guessing; put recommended options first with "(Recommended)" suffix. Supports single/multi-select, custom answers, and tabbed forms.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `questions` | array | yes | Questions requiring a user decision or missing input. |

## Agents & Coordination

### `list_agents` *(lua plugin)*

List live background agent sessions. Use to discover ids and current states before inspecting or controlling a session. Do not use when you already have one id; use `get_agent` instead. Use `control_agent` only for mutations.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|

### `get_agent` *(lua plugin)*

Show status for one live background agent. Use when you have its agent id and need progress or output. Do not use to discover ids; use `list_agents` instead. Use `control_agent` only when you need to mutate that session.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `agent_id` | string | yes | Live agent/session id. |

### `control_agent` *(lua plugin)*

Mutate a background agent by messaging, stopping, resuming, or managing policy. Use only when a live session must change. Do not use for discovery or status reads; use `list_agents` or `get_agent` instead. Pause is unsupported on TUI sessions.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `message` | string | no | New instructions for `message` or optional guidance for `resume`. |
| `policy` | object | no | Policy payload when action=policy. Use only with a validated rule scope. |
| `action` | string | yes | Mutating control action. |
| `agent_id` | string | no | Target agent id. |

### `run_task` *(lua plugin)*

Launch one isolated agent for a focused task. Use for independent research or a small delegated implementation with explicit context and concise file:line output. Do not use for multiple independent tasks; use `run_batch` or `run_team` instead. research is read-only, general may edit, and each call starts fresh; summarize returned results. background returns agent_id.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | string | yes | Task summary (3-5 words). |
| `model_tier` | string | no | Tier: weak/medium/strong. |
| `auto_tier` | boolean | no | Auto-route tier from prompt. |
| `background` | boolean | no | Start in background; return agent_id immediately. |
| `model` | string | no | Exact model override. |
| `output_schema` | object | no | Output JSON schema. Result returned as validated JSON string. |
| `prompt` | string | yes | Focused instructions and context for the isolated agent. State the question, scope, and expected file:line result. |
| `thinking` | string/integer | no | Thinking mode. Omit to inherit. |
| `subagent_type` | string | no | research (default) or general. |

### `run_team` *(lua plugin)*

Run an ALMAS agent team for a multi-step SDLC goal. Use when the work benefits from planning, parallel roles, validation, or review. Do not use for one focused task; use `run_task` instead. Do not use `run_workflow` unless you need scripted branching and resume. supervised plans, autonomous executes, swarm runs decentralized rounds; background returns agent_id.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `human_escalation` | boolean | no |  | Pause on step failure; return run_id. |
| `resume` | string | no |  | Paused run_id to resume. |
| `ibn_gate` | boolean | no |  | Use information-bottleneck gate in swarm. |
| `goal` | string | yes |  | Multi-step SDLC goal for the team. Include scope, constraints, and definition of done. |
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
| `thinking` | string/integer | no |  | Thinking mode. Default: "adaptive". |
| `background` | boolean | no |  | Start in background; return agent_id. |
| `auto_tier` | boolean | no |  | Auto-route tier from step prompt. |

### `run_workflow` *(lua plugin)*

Run a sandboxed Lua workflow for multi-stage agent orchestration. Use for branching, pipelines, or deterministic resume. Do not use for one focused agent or role-based SDLC work; use `run_task` or `run_team` instead.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `resume` | string | no | Paused run_id. Replays journaled agent() calls. |
| `inputs` | object | no | Free-form object exposed as global `inputs`. |
| `script` | string | yes | Lua script. Start with meta({...}). Use agent/parallel/pipeline/phase/log. Return final string. |
| `timeout_secs` | integer | no | Wall-clock timeout for this run (minimum 60s). May shorten, but cannot exceed, the configured workflow timeout. |

### `update_todo` *(lua plugin)*

Create or replace a structured todo list for multi-step work. Use for tasks with three or more steps and update after each completed step. Do not use for trivial one-step work; do not send a partial list because each call replaces the whole list. Use `use_blackboard` for shared multi-agent claims and observations.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `todos` | array | yes | The updated todo list |

### `delegate_fusion` *(lua plugin)*

Delegate execution to the conservative Fusion sidekick while the lead plans and reviews. Use when Fusion is enabled and a scoped implementation can be reviewed by the lead. Do not use for security, sensitive, destructive, design, or review work; keep that on the lead. Pass goal, constraints, and definition_of_done, not file dumps.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | string | yes | Short label (3-5 words). |
| `constraints` | string | no | Scope and patterns. |
| `definition_of_done` | string | yes | Success checks (tests, artifacts). |
| `goal` | string | yes | What to accomplish. Use when Fusion is enabled and the lead explicitly wants sidekick execution. |
| `escalation_triggers` | string | no | When to escalate to the lead. |
| `subagent_type` | string | no | research (read-only) or general (edit). Default: general. |

## Context & Discovery

### `use_memory` *(lua plugin)*

Persistent, project-scoped scratchpad for learnings, patterns, decisions, and gotchas across sessions. Save important context before compaction or to build project knowledge. Use `search` for keyword/tag recall (not semantic paraphrase). Keep entries concise and current. Delete outdated information. Use when a learning should survive compaction or a later session. Do not use for temporary agent coordination; use `use_blackboard` instead. Use `search` before writing a duplicate note.

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

Load a skill that provides instructions and workflows for a specific task. Use `list=true` to discover skills, then call with the exact `name` when a matching skill exists. Do not use for generic project instructions; read AGENTS.md or use read_file instead. Do not load a skill when its instructions are already in context.

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
| `name` | string | no |  | Exact skill name returned by `list=true`. |
| `list` | boolean | no |  | Return the list of available skills with their descriptions instead of loading one. |
| `include_telemetry` | boolean | no |  | Append skill telemetry summary and log list/load/plan events. |
| `preview` | boolean | no |  | Return a short synopsis or first lines instead of the full skill body. |
| `graph_rank` | boolean | no |  | With list=true and rank=true, add graph-index bonuses for path-scoped skills. |
| `rank` | boolean | no |  | With list=true and path set, sort skills by relevance to the focus path. |

### `search_tools` *(lua plugin)*

Search deferred tools by name or description. Use when a needed capability is not loaded and its canonical name is unknown. Do not use when a loaded sibling already matches the task; call that sibling directly.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | string | yes | Capability, canonical tool name, or description text to match. |
| `namespace` | string | no | Optional namespace in which to search for deferred tools. |

### `load_toolset` *(lua plugin)*

Load all deferred tools from one namespace. Use when several sibling tools from that namespace are needed. Do not use for one capability or an unknown canonical name; use `search_tools` instead.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `namespace` | string | yes | Exact deferred namespace returned by `search_tools`. |

### `use_blackboard` *(lua plugin)*

Shared coordination for multi-agent sessions. Use to post findings, claim work atomically, or query shared state when agents collaborate. Do not use as a private scratchpad; use `use_memory` for durable project notes. Use `run_team` or `run_task` to launch agents.

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

## Web

### `fetch_url` *(lua plugin)*

Fetch a known URL and return its contents as markdown, text, or html. Use for current documentation or web pages when a URL is available. Do not use for discovery or a guessed URL; use `search_web` instead. Use `run_python` when filtering a large response. HTTP is upgraded to HTTPS; max 5MB and 120s.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `url` | string | yes |  | Known URL to fetch (http:// or https://). |
| `timeout` | integer | no | 30, max 120 | Timeout in seconds. |
| `format` | string | no |  | Output format: markdown (default), text, or html. |

### `search_web` *(lua plugin)*

Search the web for current information and source discovery. Use when you need current events, documentation, APIs, or anything not in local files and do not have a URL. Do not use for a known URL; use `fetch_url` instead. Prefer specific queries, never expose secrets or invent citations, and use `run_python` to filter large result sets.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `num_results` | integer | no | 8 | Number of results to return. |
| `query` | string | yes |  | Specific question or keywords to search. |