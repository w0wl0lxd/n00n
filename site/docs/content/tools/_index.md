+++
title = "Tools"
weight = 3
[extra]
group = "Reference"
+++

# Tools

n00n ships with 27 built-in tools. This is the full reference.

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
| `offset` | integer | no | Line number to start from (1-indexed) |
| `path` | string | yes | Absolute path to the file or directory |
| `limit` | integer | no | Max number of lines to read. Omitting the limit reads up to 2000 lines. |

### `write` *(lua plugin)*

Write content to a file, replacing existing content.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `content` | string | yes | The complete file content to write |
| `path` | string | yes | Absolute path to the file |

### `edit` *(lua plugin)*

Replace an exact string match in a file.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `replace_all` | boolean | no | false | Replace all occurrences |
| `path` | string | yes |  | Absolute path to the file |
| `old_string` | string | yes |  | Exact string to find (must match uniquely unless replace_all is true) |
| `new_string` | string | yes |  | Replacement string |

### `multiedit` *(lua plugin)*

Make multiple find-and-replace edits to a single file atomically.
Prefer this over edit when n00nng multiple changes to the same file.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `edits` | array | yes | Array of edit operations to apply sequentially |
| `path` | string | yes | Absolute path to the file |

### `edit_lines` *(lua plugin, opt-in)*

Edit lines by number. Replaces lines from `start` to `end` (inclusive) with `new_string`. Use empty `new_string` to delete a range. Do not use with the batch tool.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `start` | integer | yes | First line (1-indexed) |
| `path` | string | yes | Absolute path to the file |
| `new_string` | string | yes | Replacement text |
| `end` | integer | yes | Last line, inclusive |

### `insert_lines` *(lua plugin, opt-in)*

Insert lines before a given line number. Lines at `line` and below shift down. Existing lines are preserved. Do not use with the batch tool.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | Absolute path to the file |
| `line` | integer | yes | Line number to insert before (1-indexed). Use 1 to insert at the top. |
| `new_string` | string | yes | Text to insert |

### `glob` *(lua plugin)*

Find files by glob pattern.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `pattern` | string | yes |  | Glob pattern (e.g. **/*.rs, src/**/*.ts) |
| `path` | string | no | cwd | Directory to search in |

### `grep` *(lua plugin)*

Search file contents using regex.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `include` | string | no |  | File glob filter (e.g. *.c) |
| `path` | string | no | cwd | Directory to search in |
| `pattern` | string | yes |  | Regex pattern |
| `context_after` | integer | no |  | Context lines after match |
| `limit` | integer | no |  | Max match groups to return |
| `context_before` | integer | no |  | Context lines before match |

### `index` *(lua plugin)*

Return a compact overview of a source file: imports, type definitions, function signatures, and structure with their line numbers surrounded by []. ~70-90% more efficient than reading the full file.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | Absolute path to the file |

### `view_image` *(lua plugin)*

View an image file (png, jpeg, gif, webp) so you can actually see it; it is returned as vision input alongside the tool result. Use instead of `read` for images.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | Path to the image file |

### `codegraph` *(lua plugin)*

Query a pre-indexed semantic codegraph for cross-file structural analysis. Returns verbatim source code grouped by file, plus a dependency impact "blast radius" summary with caller counts and test coverage info.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `projectPath` | string | no | Absolute path to the project (defaults to current workspace) |
| `query` | string | yes | Natural language question or symbol/file names to explore (e.g. 'AuthService login', 'GraphTraverser BFS impact') |

### `arbor` *(lua plugin)*

Graph-based code analysis using Arbor.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `command` | string | yes |  |
| `token_budget` | integer | no |  |
| `project` | string | no |  |
| `symbol` | string | no |  |

## Execution & Control

### `batch` *(lua plugin)*

Executes multiple independent tool calls concurrently to reduce round-trips.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `tool_calls` | array | yes | Array of tool calls to execute in parallel |

### `code_execution` *(lua plugin)*

Execute Python code in a sandboxed interpreter with tools as callable functions.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `timeout` | integer | no | 30, max 300 | Timeout in seconds |
| `code` | string | yes |  | Python code to execute. Tools are async functions that return strings (not objects). You MUST await every call: `result = await read(path='/file')`. Use `await asyncio.gather(...)` for concurrency. |

### `question` *(lua plugin)*

Ask the user questions during execution. Use to gather preferences, clarify instructions, get decisions, or offer choices.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `questions` | array | yes | List of questions to ask the user |

## Agent & Knowledge

### `agent_control` *(lua plugin)*

Control background agents started by task or team.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `message` | string | no | Steering instructions. Required for message. |
| `action` | string | yes | Control action. |
| `agent_id` | string | no | Background agent id. Required for status, message, and stop. |

### `team` *(lua plugin)*

Run a bounded ALMAS team for an SDLC goal. Roles: product_manager, planner, developer, tester, reviewer. Modes: supervised (return plan), autonomous (execute plan), swarm (decentralized rounds with IBN gate). model overrides tiers; auto_tier routes by task. use_retrieval grounds work; compact TOON-encodes context; background returns agent_id. Default/hard budgets: 16/24 agents, 4 concurrent.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `max_agents` | integer | no | 16, max 24 | Team agent budget. |
| `background` | boolean | no |  | Start in background session; return agent_id. |
| `compact` | boolean | no |  | TOON-encode retrieved context (token-saving). |
| `model_tier` | string | no |  | Supervisor tier (weak/medium/strong). Default: strong. |
| `quorum` | boolean | no |  | Require validator quorum for autonomous/swarm. |
| `max_steps` | integer | no | 6, max 8 | Max plan steps. |
| `max_concurrent` | integer | no | 4, max 4 | Swarm concurrency. |
| `ibn_gate` | boolean | no |  | Use information-bottleneck fan-out gate in swarm. |
| `max_rounds` | integer | no | 2, max 4 | Swarm max rounds. |
| `goal` | string | yes |  | High-level SDLC goal. |
| `model` | string | no |  | Exact model for all agents. Overrides model_tier. |
| `use_retrieval` | boolean | no |  | Ground steps with repo retrieval. |
| `mode` | string | no |  | "supervised" (return plan), "autonomous" (run plan), "swarm" (decentralized rounds). |
| `auto_tier` | boolean | no |  | Route subagent tier from step prompt. Default: true unless model set. |
| `thinking` | string/integer | no |  | Thinking mode: "off", "adaptive", effort level, or token budget. Default: "adaptive". |

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

Create or update a structured todo list to track tasks.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `todos` | array | yes | The updated todo list |

### `memory` *(lua plugin)*

Persistent, project-scoped scratchpad for learnings, patterns, decisions, and gotchas across sessions.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `command` | string | yes | Command: view, write, delete |
| `path` | string | no | Relative path (e.g. 'architecture.md'). Omit to list all. |
| `content` | string | no | File content for 'write' |

### `skill` *(lua plugin)*

Load a skill for specific tasks.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | Name of the skill to load |

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

Fetch a URL and return its contents.

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