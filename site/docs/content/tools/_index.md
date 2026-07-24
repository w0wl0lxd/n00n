+++
title = "Tools"
weight = 3
[extra]
group = "Reference"
+++

# Tools

n00n ships with 28 built-in tools. This is the full reference.

## File Operations

### `bash` *(lua plugin)*

Execute a bash command.
Commands run in <cwd> by default.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `command` | string | yes |  | The bash command to execute |
| `workdir` | string | no | cwd | Working directory |
| `timeout` | integer | no | 120 | Timeout in seconds |
| `description` | string | no |  | Short description (3-5 words) of what the command does |

### `read` *(lua plugin)*

Read a file or directory. Returns contents with line numbers (1-indexed).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `offset` | integer | no |  |
| `path` | string | yes |  |
| `limit` | integer | no |  |

### `write` *(lua plugin)*

Write content to a file, replacing existing content. Creates parent directories. Always read first. Never create files unless necessary. Never proactively create docs (*.md, README) unless requested.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `content` | string | yes |  |
| `path` | string | yes |  |

### `edit` *(lua plugin)*

Replace an exact string match in a file. old_string must appear exactly once unless replace_all is true. Read file first. When copying from read output, exclude line number prefix (e.g. `42: `). Prefer over write for targeted changes. Use replace_all for renaming.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `replace_all` | boolean | no |  |
| `path` | string | yes |  |
| `old_string` | string | yes |  |
| `new_string` | string | yes |  |

### `multiedit` *(lua plugin)*

Make multiple find-and-replace edits to a single file atomically. Prefer over edit for multiple changes. Read file first. old_string must match exactly, including whitespace. Each edit must match exactly once unless replace_all. Edits applied in sequence. If any edit fails, none are written. Ensure earlier edits don't affect later edits.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `edits` | array | yes |  |
| `path` | string | yes |  |

### `edit_lines` *(lua plugin, opt-in)*

Edit lines by number. Replaces lines from `start` to `end` (inclusive) with `new_string`. Use empty `new_string` to delete. Do not use with batch.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `start` | integer | yes |  |
| `path` | string | yes |  |
| `new_string` | string | yes |  |
| `end` | integer | yes |  |

### `insert_lines` *(lua plugin, opt-in)*

Insert lines before a given line number. Lines at `line` and below shift down. Existing lines preserved. Do not use with batch.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes |  |
| `line` | integer | yes |  |
| `new_string` | string | yes |  |

### `glob` *(lua plugin)*

Find files by glob pattern. Respects .gitignore. Returns absolute paths sorted by modification time (newest first). Prefer speculative parallel searches over sequential glob+grep.

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

Query a pre-indexed semantic codegraph for cross-file structural analysis. Returns verbatim source code grouped by file, plus a dependency impact "blast radius" summary with caller counts and test coverage info. Typically uses fewer tokens than broad grep + read for the same cross-file question.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `projectPath` | string | no | Absolute path to the project (defaults to current workspace) |
| `query` | string | yes | Natural language question or symbol/file names to explore (e.g. 'AuthService login', 'GraphTraverser BFS impact') |

### `arbor` *(lua plugin)*

Graph-based code analysis using Arbor. Returns structured, compact
caller/callee/project maps; prefer it over broad grep or unfiltered reads
for relationship and impact questions.

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

Execute Python code in a sandboxed interpreter with tools as callable functions. Use for chained/dependent tool calls and filtering/processing results. Faster than sequential tool calls. Tools are async: `result = await read(path='file.txt')`. Use `asyncio.gather()` for concurrency. Available libs: re, asyncio, sys, os, json. Fresh sandbox each run. 30s script timeout (`timeout` param); time awaiting tool calls doesn't count.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `timeout` | integer | no | 30 | Script execution timeout in seconds |
| `code` | string | yes |  | Python code to execute. Tools are async functions that return strings (not objects). You MUST await every call: `result = await read(path='/file')`. Use `await asyncio.gather(...)` for concurrency. |

### `question` *(lua plugin)*

Ask the user questions during execution. Supports single/multi-select, custom answers, and tabbed multi-question forms. Put recommended options first with "(Recommended)" suffix.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `questions` | array | yes | List of questions to ask the user |

## Agent & Knowledge

### `agent_control` *(lua plugin)*

Control background agents started by task, team, or workflow.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `message` | string | no | Steering instructions. |
| `policy` | object | no | Policy data for policy action. |
| `action` | string | yes | Control action. |
| `agent_id` | string | no | Background agent id. |

### `blackboard` *(lua plugin)*

Shared coordination substrate for multi-agent sessions. Post observations, claim tasks atomically, and query coordination state.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `post` | object | no | Post data for write action. |
| `only_active` | boolean | no | For list_claims: if true (default), return only active claims. If false, return all claims. |
| `action` | string | yes | Blackboard action. |
| `query` | object | no | Query parameters for query action. |
| `status` | string | no | Status for update_task action. |
| `post_id` | string | no | Post ID for read action. |
| `task_id` | string | no | Task ID for claim/release/update actions. |
| `claim` | object | no | Claim data for claim_task action. |

### `team` *(lua plugin)*

Run an ALMAS team for an SDLC goal. supervised returns a plan; autonomous executes it; swarm runs decentralized rounds. background returns an agent_id for agent_control.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `human_escalation` | boolean | no |  | Pause on step failure and return a resumable run_id. |
| `resume` | string | no |  | Paused run_id to resume. |
| `ibn_gate` | boolean | no |  | Use information-bottleneck fan-out gate in swarm. |
| `goal` | string | yes |  | High-level SDLC goal. |
| `use_summary` | boolean | no |  | Use the Summary Agent index for retrieval. |
| `mode` | string | no |  | "supervised" (return plan), "autonomous" (run plan), "swarm" (decentralized rounds). |
| `waves` | boolean | no |  | Execute plan in waves (plan, implement, validate) with validation gates. |
| `max_agents` | integer | no | 16, max 24 | Team agent budget. |
| `max_wave_retries` | integer | no | 3, max 5 | Max retries when validation gate fails. |
| `compact` | boolean | no |  | TOON-encode retrieved context (token-saving). |
| `model_tier` | string | no |  | Supervisor tier (weak/medium/strong). Default: strong. |
| `checkpoints` | boolean | no |  | Persist checkpoints after each wave for resume capability. |
| `max_steps` | integer | no | 6, max 8 | Max plan steps. |
| `max_concurrent` | integer | no | 4, max 4 | Swarm concurrency. |
| `quorum` | boolean | no |  | Require validator quorum for autonomous/swarm. |
| `max_rounds` | integer | no | 2, max 4 | Swarm max rounds. |
| `use_retrieval` | boolean | no |  | Ground steps with repo retrieval. |
| `model` | string | no |  | Exact model for all agents. Overrides model_tier. |
| `continue` | string | no |  | Human guidance appended when resuming. |
| `thinking` | string/integer | no |  | Thinking mode: "off", "adaptive", effort level, or token budget. Default: "adaptive". |
| `background` | boolean | no |  | Start in background session; return agent_id. |
| `auto_tier` | boolean | no |  | Route subagent tier from step prompt. Default: true unless model set. |

### `task` *(lua plugin)*

Launch one isolated agent; combine independent calls with batch. research (default) is read-only; general can edit. Each call starts fresh, so include context and ask for concise file:line results. Summarize returned results. auto_tier is opt-in. background returns agent_id.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | string | yes | Short (3-5 words) task description |
| `model_tier` | string | no | Capped tier: "weak", "medium", or "strong" |
| `auto_tier` | boolean | no | Pick model_tier from prompt automatically (opt-in). Overrides model_tier when set. |
| `background` | boolean | no | Start in background session; return agent_id immediately. |
| `model` | string | no | Exact model spec (optional). Overrides model_tier. |
| `output_schema` | object | no | JSON Schema (object) subagent result must match. Result returned as validated JSON string. |
| `prompt` | string | yes | Detailed task prompt for the agent |
| `thinking` | string/integer | no | Thinking mode: "off", "adaptive", effort level, or token budget. Omit to inherit user setting. |
| `subagent_type` | string | no | "research" (read-only, default) or "general" (can edit) |

### `workflow` *(lua plugin)*

Run a bounded, sandboxed Lua workflow for multi-stage agent orchestration.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `inputs` | object | no | Free-form object exposed to script as global `inputs`. |
| `script` | string | yes | Lua workflow script. First statement: meta({...}). Orchestrate with agent/parallel/pipeline/phase/log. Must return final answer as string. |
| `resume` | string | no | Prior run_id. Replays journaled agent() results; only spends tokens on new calls. |

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