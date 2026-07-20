+++
title = "Lua API"
weight = 6
[extra]
group = "Reference"
+++

# Lua API

Noon plugins are plain Lua files. Everything a plugin can touch lives under
one global table: `noon`. This reference documents every module, function,
and method. It is generated straight from the source code by `noon-docgen`.

The API tries to mirror Neovim as much as possible (`noon.fs`, `noon.uv`,
`noon.treesitter`, `noon.keymap`, `noon.base64`), signatures are kept identical
so code can be copy-pasted between the two without too many modifications.

Plugins run compiled to native code (Luau JIT). If you are debugging a
plugin and want full backtraces, start noon with `--no-jit`: it runs your
Lua on the interpreter with complete debug info instead.

A small plugin looks like this:

```lua
noon.api.register_command({
  name = "greet",
  description = "Say hello from Lua",
  handler = function()
    noon.ui.flash("hello from a plugin!")
  end,
})
```

## How to read this reference

Signatures use Neovim notation: `{path}` is a required argument, `{opts?}`
is optional, and `{...}` is variadic.

One convention to remember: fallible runtime operations return a
`(value, err)` pair instead of throwing. Check `err` before using `value`:

```lua
local text, err = noon.fs.read("config.json")
if err then
  noon.log.error("read failed: " .. err)
  return
end
```

Lua errors are reserved for programmer mistakes, like passing a number where
a string belongs.

## Overview

| Module | What it is for |
| --- | --- |
| [`noon`](#noon) | The global entry point. |
| [`noon.api`](#noon-api) | Plugin registration. |
| [`noon.agent`](#noon-agent) | Subagent primitives for plugins that need to talk to an LLM. |
| [`noon.agent.Session`](#noon-agent-Session) | A subagent session with its own conversation history. |
| [`noon.async`](#noon-async) | Tools for running things concurrently in Lua plugins. |
| [`noon.async.Semaphore`](#noon-async-Semaphore) | A counting semaphore for limiting how many tasks run at once. |
| [`noon.async.Permit`](#noon-async-Permit) | One slot in a semaphore, obtained from `Semaphore:acquire()`. |
| [`noon.base64`](#noon-base64) | Base64 encoding and decoding, modelled after `vim.base64`. |
| [`noon.env`](#noon-env) | Paths to noon's own directories (config, state, logs, legacy). |
| [`noon.fn`](#noon-fn) | Process and environment helpers, modeled after Neovim's `vim.fn` job |
| [`noon.fs`](#noon-fs) | File-system utilities, modelled after `vim.fs` and `vim.uv`. |
| [`noon.image`](#noon-image) | Small building blocks for working with images: probe metadata, decode |
| [`noon.image.Image`](#noon-image-Image) | A decoded image you can inspect, resize, and re-encode. |
| [`noon.interpreter`](#noon-interpreter) | Run Python code in a memory-safe, time-limited sandbox. |
| [`noon.json`](#noon-json) | JSON encoding, decoding, and schema validation. |
| [`noon.json.SchemaValidator`](#noon-json-SchemaValidator) | A compiled JSON Schema validator. |
| [`noon.keymap`](#noon-keymap) | Key mappings, modeled after `vim.keymap`. |
| [`noon.log`](#noon-log) | Structured logging for plugins. |
| [`noon.net`](#noon-net) | HTTP client for fetching web content. |
| [`noon.session`](#noon-session) | Host session primitives. |
| [`noon.text`](#noon-text) | Text transformation utilities. |
| [`noon.treesitter`](#noon-treesitter) | Tree-sitter parsing and query API. |
| [`noon.treesitter.language`](#noon-treesitter-language) | Language registry for tree-sitter grammars. |
| [`noon.treesitter.query`](#noon-treesitter-query) | Query compilation and lookup. |
| [`noon.treesitter.Query`](#noon-treesitter-Query) | A compiled tree-sitter query. |
| [`noon.treesitter.Tree`](#noon-treesitter-Tree) | A parsed syntax tree. |
| [`noon.treesitter.Node`](#noon-treesitter-Node) | A single node in a parsed syntax tree. |
| [`noon.treesitter.LanguageTree`](#noon-treesitter-LanguageTree) | Manages parsing of a source string for a single language. |
| [`noon.ui`](#noon-ui) | Functions for building interactive UI. |
| [`noon.ui.Win`](#noon-ui-Win) | Handle to a floating or split window. |
| [`noon.ui.Buf`](#noon-ui-Buf) | A content buffer that holds styled lines of text. |
| [`noon.uv`](#noon-uv) | System and environment utilities, modelled after `vim.uv`. |
| [`noon.yaml`](#noon-yaml) | YAML encoding and decoding. |

## noon {#noon}

The global entry point. Every API lives under this table.

---

### `noon.setup()` {#noon-setup}

```lua
noon.setup({config})
```

Apply your personal configuration. This is only available inside `init.lua` (not in plugins) and can be called at most once. The table accepts the same keys as the Configuration reference.

**Parameters:**

- `{config}` (`table`) Configuration table.

**Example:**

```lua
noon.setup({
model = "opus",
keymaps = false,
})
```

---

### `noon.split()` {#noon-split}

```lua
noon.split({s}, {sep}, {opts?})
```

Split {s} at each occurrence of {sep} and return the pieces as a
list. Mirrors Neovim's `vim.split`, so code using it can be copied
between Neovim and noon. {sep} is a Lua pattern unless `plain` is
set; an empty {sep} splits into single characters.

**Parameters:**

- `{s}` (`string`) String to split.
- `{sep}` (`string`) Separator: a Lua pattern, or literal text with `plain`.
- `{opts?}` (`table?`) Optional settings:
  - `plain` (`boolean?`) treat {sep} as literal text instead of a pattern.
  - `trimempty` (`boolean?`) drop empty pieces from the start and end of the result.

**Returns:** (`table`) List of split pieces.

**Example:**

```lua
noon.split("a,b,c", ",")                   -- { "a", "b", "c" }
noon.split("x*y*z", "*", { plain = true }) -- { "x", "y", "z" }
noon.split("\nhello\nworld\n", "\n", { trimempty = true }) -- { "hello", "world" }
```


## noon.api {#noon-api}

Plugin registration. This is where you tell noon about your tools,
slash commands, and prompt contributions.

Most plugins only need `register_tool` and maybe `register_prompt_hint`.
Call these at the top level of your plugin file (during load).

```lua
noon.api.register_tool({ name = "greet", ... })
noon.api.register_prompt_hint({ slot = "tool_usage", content = "..." })
```

---

### `noon.api.register_tool()` {#noon-api-register_tool}

```lua
noon.api.register_tool({spec})
```

Register a new tool the agent can call. This is the main way plugins add
capabilities to the agent. The tool is queued during plugin load and
committed to the registry once the plugin finishes loading.

Your {spec} table must include a name, a description (the model reads it
to decide when to use the tool), a JSON Schema for the input, and a handler
function. The handler receives `(input, ctx)` and returns either a plain
string or a table with richer output fields.

**Parameters:**

- `{spec}` (`table`) Tool specification:
  - `name` (`string`) Required. ASCII identifier, up to 64 chars ([a-zA-Z_][a-zA-Z0-9_]*).
  - `description` (`string`) Required. Non-empty description shown to the model.
  - `schema` (`table`) Required. JSON Schema object describing the tool's input parameters.
  - `handler` (`function`) Required. Called with `(input, ctx)` when the tool is invoked.
    Must return a string or a table with any of these fields:
    - `llm_output` (`string`) Text sent to the model.
    - `is_error` (`boolean`) When true, the result is treated as an error.
    - `content` (`string`) Alias for llm_output (legacy).
    - `body` (`BufHandle`) Rich rendered body shown in the UI.
    - `header` (`BufHandle`) One-line header shown before the body.
    - `format` (`string`) "plain" (default) or "markdown".
    - `annotation` (`string`) Short label shown next to the tool call.
    - `written_path` (`string`) Path of a file written by the tool.
    - `diff_path` (`string`) Path for a diff output block.
    - `diff_before` (`string`) Before text of the diff.
    - `diff_after` (`string`) After text of the diff.
    - `image` (`table`) { media_type: string, data: string } base64 image.
    - `instructions` (`table`) Array of { path, content } blocks injected as context.
    - `state` (`any`) Serializable state forwarded to restore.
  - `audiences` (`string[]`) Which model audiences see the tool. Values: "main", "sub", "all". Default: all audiences.
  - `kind` (`string`) Optional grouping label (e.g. "filesystem").
  - `timeout` (`number`) Execution timeout in seconds. 0 or false disables. Default: inherits agent deadline.
  - `header` (`function`) Optional. Called before execution, returns a string or BufHandle for the one-line header.
  - `restore` (`function`) Optional. Called to re-render a previous tool result. Receives `(tool_name, input, output, ctx)`.
  - `start` (`function`) Optional. Called when the tool call starts, before the handler runs.
  - `describe` (`function`) Optional. Returns a custom description string for the current context.
  - `examples` (`table`) Optional. Array of example input objects for documentation.
  - `permission_scopes` (`string|function`) Field name in schema (string) or `function(input)` returning a list of path scopes that need write permission.
  - `mutable_path` (`string`) Schema field name (type: string) for the primary path the tool writes.
  - `start_annotation` (`string|table`) Schema field used to annotate the start header with a count (string) or timeout (`{ field, kind="timeout" }`).

**Example:**

```lua
noon.api.register_tool({
  name = "word_count",
  description = "Count words in a file.",
  kind = "read",
  schema = {
    properties = { path = { type = "string", description = "File path" } },
    required = { "path" },
  },
  handler = function(input)
    local f = io.open(input.path, "r")
    if not f then return { llm_output = "file not found", is_error = true } end
    local n = 0
    for _ in f:read("*a"):gmatch("%S+") do n = n + 1 end
    f:close()
    return tostring(n) .. " words"
  end,
})
```

---

### `noon.api.register_command()` {#noon-api-register_command}

```lua
noon.api.register_command({spec})
```

Register a slash-command that appears in the user input bar.

Slash commands let the user trigger plugin actions by typing `/name` in the
input. Use them for interactive workflows that do not need the model, like
browsing memory files or toggling settings.

**Parameters:**

- `{spec}` (`table`) Command specification:
  - `name` (`string`) Required. The command name (without the leading slash).
  - `description` (`string`) Optional. Short description shown in the command palette.
  - `handler` (`function`) Required. Called when the user runs the command.

**Example:**

```lua
noon.api.register_command({
  name = "/hello",
  description = "Say hello",
  handler = function()
    noon.ui.flash("Hello from my plugin!")
  end,
})
```

---

### `noon.api.register_prompt_hint()` {#noon-api-register_prompt_hint}

```lua
noon.api.register_prompt_hint({spec})
```

Add a piece of text to an aggregate prompt slot. Multiple plugins can each
contribute to the same slot, and all contributions are concatenated.

Good for things like tool usage guidelines or extra context that should
appear alongside other plugins' hints. If you need to own the whole slot
(e.g. identity or tone), use `set_prompt` instead.

Throws if you pass a singleton slot name.

**Parameters:**

- `{spec}` (`table`) Hint specification:
  - `slot` (`string`) Required. Aggregate slot name (e.g. "tool_usage", "general").
  - `content` (`string|function`) Required. Static text, or a `function()` that returns a string. Max 1 MiB.
  - `prompt` (`string|string[]`) Optional. Restrict to specific prompt ids (e.g. "system").

**Example:**

```lua
noon.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Prefer **grep** over reading entire files.",
})
```

---

### `noon.api.register_options()` {#noon-api-register_options}

```lua
noon.api.register_options({spec})
```

Declare the options your plugin accepts under `plugins.<name>` in
`noon.setup`, and get back what the user set merged with your defaults.
Call it once, at the top level of your plugin file.

An unknown key, a wrong type, or a value below `min` fails the plugin
load with a clear message, so users catch typos right away. Bad specs
fail the load too. The specs also feed the generated configuration docs.

**Parameters:**

- `{spec}` (`table`) Map of option name to a spec table:
  - `default` (`boolean|number|string`) Optional. Used when the user sets nothing. Its Lua type becomes the option type.
  - `type` (`string`) Required when there is no default: "boolean", "integer", "number", or "string".
  - `min` (`number`) Optional. Minimum accepted value, numeric options only.
  - `desc` (`string`) Required. One line shown in the configuration docs.

**Returns:** (`table`) Merged options: the user's value where set, otherwise the default, or nil when neither exists.

**Example:**

```lua
local opts = noon.api.register_options({
  timeout_secs = { default = 120, min = 5, desc = "Kill the command after this many seconds." },
  max_output_lines = { type = "integer", desc = "Override agent.max_output_lines for this tool." },
})
```

---

### `noon.api.set_prompt()` {#noon-api-set_prompt}

```lua
noon.api.set_prompt({spec})
```

Set a singleton prompt slot. Only one plugin owns each singleton slot at a
time, so calling this replaces any previous value from your plugin.

Use this for slots like "identity" or "tone" where a single coherent value
makes more sense than combining fragments. For aggregate slots like
"tool_usage", use `register_prompt_hint` instead.

Throws if you pass an aggregate slot name.

**Parameters:**

- `{spec}` (`table`) Spec fields mirror `register_prompt_hint`:
  - `slot` (`string`) Required. Singleton slot name (e.g. "identity", "tone").
  - `content` (`string|function`) Required. Static text or a `function()` returning a string. Max 1 MiB.
  - `prompt` (`string|string[]`) Optional. Restrict to specific prompt ids.

**Example:**

```lua
noon.api.set_prompt({
  slot = "tone",
  content = "Be concise. No filler words.",
})
```

---

### `noon.api.get_tools()` {#noon-api-get_tools}

```lua
noon.api.get_tools({opts?})
```

Return a list of all registered tools. Useful for building UI that shows
available tools or for checking which tools are enabled.

Each entry has the tool's name, schema, audiences, and an `enabled` flag.
Describe callbacks are not invoked (the static description is used).

**Parameters:**

- `{opts?}` (`table?`) Options:
  - `config` (`table`) Optional config table with a `disabled_tools` string[] field used to compute the `enabled` flag on each entry.

**Returns:** (`table[]`) Array of tool entries: { name, schema, audiences, kind?, enabled }.

**Example:**

```lua
local tools = noon.api.get_tools()
for _, t in ipairs(tools) do
  print(t.name, t.enabled)
end
```

---

### `noon.api.get_tool()` {#noon-api-get_tool}

```lua
noon.api.get_tool({name})
```

Look up a single tool by name. Returns its metadata table or nil if the
tool does not exist. For Lua-registered tools the returned table also
includes `header` and `restore` handle functions (wrapped so they never
throw).

**Parameters:**

- `{name}` (`string`) Exact tool name.

**Returns:** (`table|nil`) Tool entry with fields { name, schema, audiences, kind?, header?, restore? }, or nil if not found.

**Example:**

```lua
local t = noon.api.get_tool("bash")
if t then
  print("bash audiences:", table.concat(t.audiences, ", "))
end
```

---

### `noon.api.create_autocmd()` {#noon-api-create_autocmd}

```lua
noon.api.create_autocmd({event}, {opts})
```

Listen for one or more events. Returns an id you can pass to
`del_autocmd` later to remove the listener.

Built-in events fired by the host: `"TurnStart"`, `"TurnEnd"`,
`"TurnError"`, `"SessionReset"`. Plugins can also fire their own
events with `exec_autocmds`.

**Parameters:**

- `{event}` (`string|string[]`) Event name or list of names.
- `{opts}` (`table`) Options:
  - `callback` (`function`) called with an ev table `{ id, event, match, data }`.
  - `once` (`boolean`) remove the handler after it fires once (default false).
  - `pattern` (`string|string[]`) only fire when the pattern matches. `"*"` matches everything. Omit to match all.

**Returns:** (`integer`) Autocmd id.

**Example:**

```lua
local id = noon.api.create_autocmd("TurnEnd", {
  callback = function(ev)
    print("turn ended: " .. ev.event)
  end,
})
```

---

### `noon.api.del_autocmd()` {#noon-api-del_autocmd}

```lua
noon.api.del_autocmd({id})
```

Remove a previously registered autocmd. Does nothing if the {id}
does not exist.

**Parameters:**

- `{id}` (`integer`) Id returned by `create_autocmd`.

**Example:**

```lua
noon.api.del_autocmd(id)
```

---

### `noon.api.exec_autocmds()` {#noon-api-exec_autocmds}

```lua
noon.api.exec_autocmds({event}, {opts?})
```

Fire one or more events manually. Every matching autocmd callback
runs synchronously before this function returns.

**Parameters:**

- `{event}` (`string|string[]`) Event name or list of names to fire.
- `{opts?}` (`table?`) Options:
  - `pattern` (`string`) passed to callbacks as `ev.match`.
  - `data` (`any`) arbitrary value passed as `ev.data`.

**Example:**

```lua
noon.api.exec_autocmds("MyEvent", {
  pattern = "init",
  data = { msg = "hello" },
})
```

---

### `noon.api.declare_slot()` {#noon-api-declare_slot}

```lua
noon.api.declare_slot({name}, {default})
```

Create a named extension point owned by your plugin. You provide a
{default} function, and other plugins can wrap it with layers using
`set_slot`. The returned callable runs the full chain: outermost
layer first, then inward, ending at {default}.

Throws if another plugin already owns a slot with the same {name}.

**Parameters:**

- `{name}` (`string`) Unique slot name, e.g. `"myplugin.render"`.
- `{default}` (`function`) Default implementation, called when no layers wrap it.

**Returns:** (`function`) Callable that dispatches through all layers.

**Example:**

```lua
local render = noon.api.declare_slot("myplugin.render", function(text)
  return text:upper()
end)
print(render("hello")) -- HELLO
```

---

### `noon.api.set_slot()` {#noon-api-set_slot}

```lua
noon.api.set_slot({name}, {wrapper})
```

Add a layer around an existing (or future) slot. Layers wrap the
default from the outside in. Each layer receives `prev` as its
first argument. Call `prev(...)` to continue down the chain.
Calling `prev` more than once throws.

You can call this before the owner runs `declare_slot`. The layer
is queued and attached when the slot is declared.

**Parameters:**

- `{name}` (`string`) Slot name to wrap.
- `{wrapper}` (`function`) Layer: `function(prev, ...)`. Call `prev(...)` to continue.

**Example:**

```lua
noon.api.set_slot("myplugin.render", function(prev, text)
  return prev("[" .. text .. "]")
end)
```

---

### `noon.api.get_slots()` {#noon-api-get_slots}

```lua
noon.api.get_slots()
```

List all known slots and their current state. Useful for debugging
which plugins own or wrap each slot.

**Returns:** (`table`) Map of slot name to `{ owner, declared, fillers }`.

**Example:**

```lua
for name, info in pairs(noon.api.get_slots()) do
  print(name, info.owner, info.declared)
end
```


## noon.agent {#noon-agent}

Subagent primitives for plugins that need to talk to an LLM.

This module gives you the building blocks: resolve which model to use,
build a system prompt, list available tools, call a tool directly, or
open a full session with its own conversation history.

Policy like retries, validation, and concurrency lives in the calling
plugin, not here.

```lua
local tools = noon.agent.tools(ctx, { audience = "general_sub" })
local sess = noon.agent.session(ctx, {
  system = "You are a helpful assistant.",
  tools = tools,
})
local r = sess:prompt("Hello!")
print(r.text)
sess:close()
```

---

### `noon.agent.resolve_model()` {#noon-agent-resolve_model}

```lua
noon.agent.resolve_model({ctx}, {opts?})
```

Look up the model that the current agent is using, or pick a cheaper one.
You might want a cheaper model for simple subtasks (summaries, classification)
without hard-coding a model name.

The returned table has fields: `id` (string), `tier` (string),
`provider` (string), `spec` (string).

**Parameters:**

- `{ctx}` (`LuaCtx`) Agent context.
- `{opts?}` (`table?`) Optional fields:
  - `tier` (`string?`) target tier, e.g. `"fast"`, `"mid"`, `"best"`. Clamped to
    the parent tier so you cannot escalate.
  - `spec` (`string?`) exact model spec string, e.g. `"claude-3-5-haiku-20241022"`.
    Takes precedence over `tier`.

**Returns:** (`table?`, `string?`) Model table on success, or `(nil, err)` on failure.

**Example:**

```lua
local model, err = noon.agent.resolve_model(ctx, { tier = "fast" })
if err then error(err) end
print(model.spec, model.tier)
```

---

### `noon.agent.system_prompt()` {#noon-agent-system_prompt}

```lua
noon.agent.system_prompt({ctx}, {opts})
```

Build a system prompt from a built-in template. Environment variables like
`{cwd}` are substituted automatically. Use this when you need a ready-made
prompt for a subagent session.

**Parameters:**

- `{ctx}` (`LuaCtx`) Agent context.
- `{opts}` (`table`) Required fields:
  - `prompt_id` (`string`) one of `"research"`, `"general"`, `"system"`.

  Optional fields:

  - `instructions` (`string|boolean?`) extra text appended to the prompt.
    `true` loads instructions from the project `.noon/instructions` file.
    `false` or nil omits them.

**Returns:** (`string?`, `string?`) The assembled prompt string, or `(nil, err)` on failure.

**Example:**

```lua
local prompt, err = noon.agent.system_prompt(ctx, {
  prompt_id = "research",
  instructions = true,
})
if err then error(err) end
```

---

### `noon.agent.tools()` {#noon-agent-tools}

```lua
noon.agent.tools({ctx}, {opts})
```

Get the list of tool definitions for a given audience. Pass the result
straight into `noon.agent.session()` or use it to inspect what tools are
available.

**Parameters:**

- `{ctx}` (`LuaCtx`) Agent context.
- `{opts}` (`table`) Required fields:
  - `audience` (`string`) tool audience filter, e.g. `"general"`, `"subagent"`,
    `"general_sub"`.

  Optional fields:

  - `only` (`string[]?`) include only these tool names.
  - `except` (`string[]?`) exclude these tool names.
  - `include_mcp` (`boolean?`) include MCP tools. Default: `true`.
  - `workflow` (`boolean?`) use workflow-mode descriptions. Default: `false`.
  - `spec` (`string?`) evaluate capability exclusions against this model spec.

**Returns:** (`table?`, `string?`) Array of tool definition tables, or `(nil, err)` on failure.

**Example:**

```lua
local defs, err = noon.agent.tools(ctx, {
  audience = "general_sub",
  except = { "bash", "write" },
})
if err then error(err) end
print(#defs .. " tools available")
```

---

### `noon.agent.call_tool()` {#noon-agent-call_tool}

```lua
noon.agent.call_tool({ctx}, {name}, {input}, {opts?})
```

Run a tool by name and wait for the result. This is how you call built-in
tools (like `read`, `bash`, `glob`) from Lua without going through the LLM.

Live events (streaming output, annotations) are delivered through optional
callbacks while the tool runs.

**Parameters:**

- `{ctx}` (`LuaCtx`) Agent context.
- `{name}` (`string`) Tool name, e.g. `"bash"`, `"read"`.
- `{input}` (`table|any`) Tool input (JSON-serializable). Must match the tool's `input_schema`.
- `{opts?}` (`table?`) Optional fields:
  - `timeout` (`integer?`) deadline in seconds.
  - `on_live_buf` (`function?`) called with a `BufHandle` for each live buffer
    the tool publishes. Must not yield.
  - `on_annotation` (`function?`) called with an annotation string for each
    annotation event. Must not yield.

**Returns:** (`string?`, `string?`) Tool output text, or `(nil, err)` on failure.

**Example:**

```lua
local out, err = noon.agent.call_tool(ctx, "bash", {
  command = "ls -la",
  timeout = 10,
})
if err then error(err) end
print(out)
```

---

### `noon.agent.session()` {#noon-agent-session}

```lua
noon.agent.session({ctx}, {opts})
```

Create a new subagent session. The session inherits the parent model and
MCP handle unless you override them. You get back a `Session` object that
you can send messages to with `:prompt()`.

This is the main way to spin up a sub-conversation with its own history
and tool set.

**Parameters:**

- `{ctx}` (`LuaCtx`) Agent context.
- `{opts}` (`table`) Optional fields:
  - `model_spec` (`string?`) model spec string to use instead of the parent model.
  - `system` (`string?`) system prompt. Defaults to empty.
  - `tools` (`table?`) tool definitions array (from `noon.agent.tools()`).
  - `local_tools` (`table?`) map of `name -> spec` for Lua-backed tools. Each spec
    requires `description` (string), `input_schema` (table), and
    `handler` (function). The handler receives the input table and must return
    `(string)` or `(nil, err)`.
  - `name` (`string?`) display name for logs and UI.
  - `audience` (`string?`) tool audience for capability gating. Default: `"general_sub"`.
  - `thinking` (`string|integer?`) thinking mode: `"off"`, `"adaptive"`, an
    effort level (`"minimal"`, `"low"`, `"medium"`, `"high"`, `"xhigh"`,
    `"max"`), or a budget integer (token count). Inherits parent setting
    if omitted.
  - `fast` (`boolean?`) use fast mode. Inherits parent setting if omitted.

**Returns:** ([`Session?`](#noon-agent-Session), `string?`) Session handle, or `(nil, err)` on failure.

**Example:**

```lua
local tools = noon.agent.tools(ctx, { audience = "general_sub" })
local sess, err = noon.agent.session(ctx, {
  system = "You are a research assistant.",
  tools = tools,
  name = "researcher",
})
if err then error(err) end
local result = sess:prompt("Summarize this file.")
sess:close()
```


## noon.agent.Session {#noon-agent-Session}

A subagent session with its own conversation history.

Create one with `noon.agent.session()`, then send messages with
`:prompt()`. The session remembers previous turns, so you can have
a multi-step conversation. Call `:close()` when you are done, or let
garbage collection handle it.

---

### `Session:prompt()` {#Session-prompt}

```lua
Session:prompt({message})
```

Send a message to the subagent and wait for its full response. The agent
loop runs to completion, calling tools as needed. Conversation history is
kept across calls, so you can have a multi-turn conversation.

The returned table has fields: `text` (string), `duration_ms` (integer),
`input_tokens` (integer), `output_tokens` (integer).

**Parameters:**

- `{message}` (`string`) User message to send.

**Returns:** (`table?`, `string?`) Result table on success, or `(nil, err)` on failure.

**Example:**

```lua
local r, err = sess:prompt("What files are in this project?")
if err then error(err) end
print(r.text)
print(r.input_tokens .. " input, " .. r.output_tokens .. " output tokens")
```

---

### `Session:close()` {#Session-close}

```lua
Session:close()
```

Close the session and flush its history back to the parent agent. You can
call this multiple times safely. If you forget, it runs automatically when
the session is garbage collected.


## noon.async {#noon-async}

Tools for running things concurrently in Lua plugins.

Use `run` to fire off background tasks, `gather` or `join` to run
several functions at once, and `semaphore` to limit concurrency.
The `await` and `wrap` helpers bridge callback-based APIs into
coroutine-friendly calls.

```lua
local results = noon.async.gather({
  function() return fetch("a.txt") end,
  function() return fetch("b.txt") end,
})
```

---

### `noon.async.run()` {#noon-async-run}

```lua
noon.async.run({fn}, {on_finish?})
```

Fire off a function as a new async task. It runs in the background and
you do not wait for it. If you need the result, pass an {on_finish}
callback.

**Parameters:**

- `{fn}` (`function`) Zero-argument function to execute.
- `{on_finish?}` (`function?`) Optional callback `function(err, result)`. Called once {fn} completes.

**Example:**

```lua
noon.async.run(function()
  local data = expensive_fetch()
  process(data)
end)
```

---

### `noon.async.await()` {#noon-async-await}

```lua
noon.async.await({argc}, {fn}, {...})
```

Turn a callback-based function into a normal call you can use in a coroutine. It calls `fn(..., callback)`, inserting the callback at position {argc}, then suspends your coroutine until the callback fires. You get back whatever the callback was called with.

**Parameters:**

- `{argc}` (`integer`) Total number of positional arguments {fn} expects (including the callback). Must be >= 1.
- `{fn}` (`function`) Callback-based function to call.
- `{...}` (`any`) Extra arguments forwarded to {fn} before the injected callback.

**Returns:** (`...`) Values passed by the caller to the injected callback.

**Example:**

```lua
local result = noon.async.await(2, http.get, url)
```

---

### `noon.async.wrap()` {#noon-async-wrap}

```lua
noon.async.wrap({argc}, {fn})
```

Create a coroutine-friendly wrapper around a callback-based function. The wrapper calls `noon.async.await` for you, so you can use the result like a normal function call.

**Parameters:**

- `{argc}` (`integer`) Callback position, forwarded to `noon.async.await`.
- `{fn}` (`function`) Callback-based function to wrap.

**Returns:** (`function`) Wrapped function you can call like a normal function.

**Example:**

```lua
local get = noon.async.wrap(2, http.get)
local body = get(url)
```

---

### `noon.async.join()` {#noon-async-join}

```lua
noon.async.join({max_jobs}, {fns})
```

Run all functions in {fns} with at most {max_jobs} going at once. Waits until every function has finished. Unlike `gather`, this does not return individual results.

**Parameters:**

- `{max_jobs}` (`integer`) Maximum number of functions running at the same time.
- `{fns}` (`table`) Array of zero-argument functions to execute.

**Example:**

```lua
noon.async.join(4, {
  function() process(files[1]) end,
  function() process(files[2]) end,
  function() process(files[3]) end,
})
```

---

### `noon.async.gather()` {#noon-async-gather}

```lua
noon.async.gather({fns})
```

Run all functions in {fns} at the same time and collect their results.
Unlike `join`, this gives you back the return value (or error) from each
function. The results are in the same order as the input.

Each entry in the result array has `ok` (boolean), and either `value`
(on success) or `err` (string, on failure).

**Parameters:**

- `{fns}` (`table`) Array of zero-argument functions.

**Returns:** (`table`) Array of result tables, one per function.

**Example:**

```lua
local results = noon.async.gather({
  function() return fetch("a.txt") end,
  function() return fetch("b.txt") end,
})
for i, r in ipairs(results) do
  if r.ok then print(r.value) else print("error: " .. r.err) end
end
```

---

### `noon.async.semaphore()` {#noon-async-semaphore}

```lua
noon.async.semaphore({n})
```

Create a counting semaphore that allows at most {n} concurrent permits.
Use this to limit how many tasks hit a resource at the same time.

**Parameters:**

- `{n}` (`integer`) Maximum number of concurrent permits. Values below 1 are clamped to 1.

**Returns:** ([`noon.async.Semaphore`](#noon-async-Semaphore)) A new semaphore.

**Example:**

```lua
local sem = noon.async.semaphore(5)
-- each task acquires a permit before doing work
local permit = sem:acquire()
do_work()
permit:release()
```


## noon.async.Semaphore {#noon-async-Semaphore}

A counting semaphore for limiting how many tasks run at once.

Create one with `noon.async.semaphore(n)`, then call `:acquire()` to
get a permit before doing work. If the task is cancelled, the acquire
is cancelled too.

---

### `Semaphore:acquire()` {#Semaphore-acquire}

```lua
Semaphore:acquire()
```

Wait for a permit from the semaphore. Your coroutine suspends until a slot
opens up. If the owning task is cancelled, the acquire is cancelled too.

**Returns:** ([`noon.async.Permit`](#noon-async-Permit)) A permit handle. Call `:release()` when done, or let it be garbage collected.

**Example:**

```lua
local sem = noon.async.semaphore(3)
local permit = sem:acquire()
-- do work that needs the slot
permit:release()
```


## noon.async.Permit {#noon-async-Permit}

One slot in a semaphore, obtained from `Semaphore:acquire()`.

The slot is held until you call `:release()` or until the permit is
garbage collected. Releasing early lets other tasks acquire sooner.

---

### `Permit:release()` {#Permit-release}

```lua
Permit:release()
```

Give the permit back to the semaphore so another task can acquire it.
Throws if you already released this permit.


## noon.base64 {#noon-base64}

Base64 encoding and decoding, modelled after `vim.base64`.

Both functions accept strings and Luau buffers, so you can round-trip
binary data read with `noon.fs.read_bytes`.

```lua
local encoded = noon.base64.encode("hello")
local decoded = noon.base64.decode(encoded)
```

---

### `noon.base64.encode()` {#noon-base64-encode}

```lua
noon.base64.encode({data})
```

Encode {data} to standard Base64. Like `vim.base64.encode`.
Accepts both strings and Luau buffers.

**Parameters:**

- `{data}` (`string|buffer`) Data to encode.

**Returns:** (`string`) Base64-encoded string.

**Example:**

```lua
noon.base64.encode("hello") -- "aGVsbG8="
```

---

### `noon.base64.decode()` {#noon-base64-decode}

```lua
noon.base64.decode({str})
```

Decode a Base64-encoded {str} back to its original bytes. Like `vim.base64.decode`.
Throws if {str} is not valid Base64.

**Parameters:**

- `{str}` (`string|buffer`) Base64-encoded text.

**Returns:** (`string`) Decoded bytes as a string.

**Example:**

```lua
noon.base64.decode("aGVsbG8=") -- "hello"
```


## noon.env {#noon-env}

Paths to noon's own directories (config, state, logs, legacy).

Use these to locate config files or persistent state without hard-coding paths.

```lua
local cfg = noon.env.config_dir()
```

---

### `noon.env.state_dir()` {#noon-env-state_dir}

```lua
noon.env.state_dir()
```

Return the directory where noon stores runtime state (sessions, auth tokens, etc.).
Typically something like `~/.local/state/noon`.

**Returns:** (`string?`) State directory path, or nil if it cannot be determined.

**Example:**

```lua
local dir = noon.env.state_dir()
```

---

### `noon.env.config_dir()` {#noon-env-config_dir}

```lua
noon.env.config_dir()
```

Return the directory where noon looks for user configuration files.
Typically something like `~/.config/noon`.

**Returns:** (`string?`) Config directory path, or nil if it cannot be determined.

**Example:**

```lua
local dir = noon.env.config_dir()
```

---

### `noon.env.logs_dir()` {#noon-env-logs_dir}

```lua
noon.env.logs_dir()
```

Return the directory where noon writes its log files (`noon.log`).
Typically something like `~/.local/logs/noon`.

**Returns:** (`string?`) Logs directory path, or nil if it cannot be determined.

**Example:**

```lua
local dir = noon.env.logs_dir()
```

---

### `noon.env.legacy_dir()` {#noon-env-legacy_dir}

```lua
noon.env.legacy_dir()
```

Return the legacy config path (`~/.noon`), if it exists on disk.
Useful for migration logic. Returns nil when there is no legacy directory.

**Returns:** (`string?`) Legacy directory path, or nil if not present.


## noon.fn {#noon-fn}

Process and environment helpers, modeled after Neovim's `vim.fn` job
control. Use these to run shell commands, wait for output, and check
whether programs are installed.

Job functions need the `run` permission. `executable` needs the `env`
permission.

```lua
local id = noon.fn.jobstart("git status", {
  on_exit = function(code) print("done: " .. code) end,
})
```

---

### `noon.fn.jobstart()` {#noon-fn-jobstart}

```lua
noon.fn.jobstart({cmd}, {opts?})
```

Run a shell command in the background. The command runs through
`bash -c` on Unix or `cmd /C` on Windows. You get back a job id
that you can pass to `jobstop` or `jobwait` to control the process.

**Parameters:**

- `{cmd}` (`string`) Shell command to run.
- `{opts?}` (`table?`) Optional settings:
  - `cwd` (`string?`) working directory (tilde is expanded).
  - `env` (`table?`) extra environment variables, `{ VAR = "value" }`.
  - `on_stdout` (`function?`) called with `(job_id, line)` for each stdout line.
  - `on_stderr` (`function?`) called with `(job_id, line)` for each stderr line.
  - `on_exit` (`function?`) called with `(job_id, code)` when the process finishes.

**Returns:** (`integer`) Job id.

**Example:**

```lua
local id = noon.fn.jobstart("ls -la", {
  cwd = "~/projects",
  on_stdout = function(_, line) print(line) end,
  on_exit = function(_, code) print("exit: " .. code) end,
})
```

---

### `noon.fn.jobstop()` {#noon-fn-jobstop}

```lua
noon.fn.jobstop({job_id})
```

Kill a running job immediately (SIGKILL on Unix). Safe to call on
jobs that already exited or on unknown ids.

**Parameters:**

- `{job_id}` (`integer`) Job id returned by `jobstart`.

**Example:**

```lua
noon.fn.jobstop(id)
```

---

### `noon.fn.jobwait()` {#noon-fn-jobwait}

```lua
noon.fn.jobwait({job_id}, {timeout_ms?})
```

Wait for a job to finish and collect its output. Returns a result
table with `stdout`, `stderr`, and `exit_code`. Returns `nil` if the
job does not finish before the timeout.

While waiting, the job's `on_stdout`, `on_stderr`, and `on_exit`
callbacks fire as events arrive (like Neovim), so you can stream
output into a buffer while parked here.

**Parameters:**

- `{job_id}` (`integer`) Job id returned by `jobstart`.
- `{timeout_ms?}` (`integer?`) Maximum wait in milliseconds (default 30000).

**Returns:** (`table?`) `{ stdout, stderr, exit_code }`, or nil on timeout.

**Example:**

```lua
local id = noon.fn.jobstart("echo hello")
local result = noon.fn.jobwait(id, 5000)
if result then
  print(result.stdout)
end
```

---

### `noon.fn.executable()` {#noon-fn-executable}

```lua
noon.fn.executable({name})
```

Check whether {name} can be found on `$PATH` or is an absolute path
to a file. Returns 1 when found, 0 otherwise (matches Neovim's
`vim.fn.executable`).

**Parameters:**

- `{name}` (`string`) Program name (e.g. `"git"`) or absolute path.

**Returns:** (`integer`) `1` if found, `0` otherwise.

**Example:**

```lua
if noon.fn.executable("rg") == 1 then
  -- use ripgrep
end
```


## noon.fs {#noon-fs}

File-system utilities, modelled after `vim.fs` and `vim.uv`.

Fallible operations return `(value, err)` pairs and never throw.
Paths support `~/` expansion. Relative paths resolve from the current working directory.

```lua
local text, err = noon.fs.read("init.lua")
if err then return end
```

---

### `noon.fs.read()` {#noon-fs-read}

```lua
noon.fs.read({path})
```

Read the entire file at {path} as a UTF-8 string.
If the file contains bytes that are not valid UTF-8, this function throws.
Use `read_bytes` for binary files.

**Parameters:**

- `{path}` (`string`) Absolute or relative file path. `~/` is expanded to the home directory.

**Returns:** (`string?`, `string?`) File contents, or nil plus an error message.

**Example:**

```lua
local text, err = noon.fs.read("config.toml")
if err then
  noon.log.warn("could not read config: " .. err)
  return
end
```

---

### `noon.fs.read_bytes()` {#noon-fs-read_bytes}

```lua
noon.fs.read_bytes({path})
```

Read the entire file at {path} as raw bytes, returned as a Luau buffer.
Useful for binary files or when you need to pass the data to `noon.base64.encode`.

**Parameters:**

- `{path}` (`string`) Absolute or relative file path. `~/` is expanded to the home directory.

**Returns:** (`buffer?`, `string?`) File bytes as a Luau buffer, or nil plus an error message.

**Example:**

```lua
local buf, err = noon.fs.read_bytes("image.png")
if err then return end
local encoded = noon.base64.encode(buf)
```

---

### `noon.fs.metadata()` {#noon-fs-metadata}

```lua
noon.fs.metadata({path})
```

Get metadata for the file or directory at {path}.
Returns a table with `size` (integer), `is_file` (boolean), and `is_dir` (boolean).
If {path} does not exist, returns nil with no error.

**Parameters:**

- `{path}` (`string`) Absolute or relative path.

**Returns:** (`table?`, `string?`) Metadata table, nil if missing, or nil plus an error message.

**Example:**

```lua
local meta = noon.fs.metadata("src/main.rs")
if meta and meta.is_file then
  print("size: " .. meta.size)
end
```

---

### `noon.fs.dirname()` {#noon-fs-dirname}

```lua
noon.fs.dirname({path})
```

Return the parent directory of {path}. Like `vim.fs.dirname`.

**Parameters:**

- `{path}` (`string`) File path.

**Returns:** (`string?`) Parent directory, or nil if {path} has no parent.

**Example:**

```lua
noon.fs.dirname("/home/user/init.lua") -- "/home/user"
```

---

### `noon.fs.basename()` {#noon-fs-basename}

```lua
noon.fs.basename({path})
```

Return the final component (the file name) of {path}. Like `vim.fs.basename`.

**Parameters:**

- `{path}` (`string`) File path.

**Returns:** (`string?`) File name, or nil for paths like `/`.

**Example:**

```lua
noon.fs.basename("/home/user/init.lua") -- "init.lua"
```

---

### `noon.fs.joinpath()` {#noon-fs-joinpath}

```lua
noon.fs.joinpath({...})
```

Join one or more path segments into a single path. Like `vim.fs.joinpath`.

**Parameters:**

- `{...}` (`string`) One or more path segments to join.

**Returns:** (`string`) The joined path.

**Example:**

```lua
noon.fs.joinpath("src", "api", "fs.rs") -- "src/api/fs.rs"
```

---

### `noon.fs.normalize()` {#noon-fs-normalize}

```lua
noon.fs.normalize({path})
```

Clean up `.` and `..` segments and make {path} absolute. Like `vim.fs.normalize`.
This is purely string-based and does not touch the filesystem.

**Parameters:**

- `{path}` (`string`) Path to normalize. `~/` is expanded.

**Returns:** (`string`) Normalized absolute path.

**Example:**

```lua
noon.fs.normalize("src/../src/api") -- "/home/user/project/src/api"
```

---

### `noon.fs.abspath()` {#noon-fs-abspath}

```lua
noon.fs.abspath({path})
```

Make {path} absolute by prepending the current working directory when needed.
Unlike `normalize`, this does not resolve `.` or `..` segments.

**Parameters:**

- `{path}` (`string`) Relative or absolute path. `~/` is expanded.

**Returns:** (`string`) Absolute path.

**Example:**

```lua
noon.fs.abspath("src/main.rs") -- "/home/user/project/src/main.rs"
```

---

### `noon.fs.parents()` {#noon-fs-parents}

```lua
noon.fs.parents({path})
```

Return all ancestor directories of {path}, from the immediate parent up to the root.
Handy for walking up a directory tree.

**Parameters:**

- `{path}` (`string`) File or directory path.

**Returns:** (`string[]`) Array of ancestor directory paths.

**Example:**

```lua
local dirs = noon.fs.parents("/home/user/project/src")
-- { "/home/user/project", "/home/user", "/home", "/" }
```

---

### `noon.fs.root()` {#noon-fs-root}

```lua
noon.fs.root({source}, {marker})
```

Walk upward from {source} looking for a directory that contains one of the
{marker} files or directories. Like `vim.fs.root`. Useful for finding the
project root.

**Parameters:**

- `{source}` (`string`) Starting file or directory path.
- `{marker}` (`string|string[]`) Marker filename(s) to look for, e.g. `".git"` or `{"package.json", ".git"}`.

**Returns:** (`string?`, `string?`) Root directory path, or nil when not found.

**Example:**

```lua
local root = noon.fs.root("src/main.rs", { ".git", "Cargo.toml" })
if root then print("project root: " .. root) end
```

---

### `noon.fs.relpath()` {#noon-fs-relpath}

```lua
noon.fs.relpath({base}, {target})
```

Compute a relative path from {base} to {target}.

**Parameters:**

- `{base}` (`string`) Base directory path.
- `{target}` (`string`) Target path.

**Returns:** (`string`) Relative path from {base} to {target}.

**Example:**

```lua
noon.fs.relpath("/home/user", "/home/user/project/src") -- "project/src"
```

---

### `noon.fs.ext()` {#noon-fs-ext}

```lua
noon.fs.ext({path})
```

Return the file extension of {path}, without the leading dot.

**Parameters:**

- `{path}` (`string`) File path.

**Returns:** (`string?`) Extension, or nil if the path has no extension.

**Example:**

```lua
noon.fs.ext("main.rs")   -- "rs"
noon.fs.ext("Makefile")  -- nil
```

---

### `noon.fs.dir()` {#noon-fs-dir}

```lua
noon.fs.dir({path}, {opts?})
```

List the contents of the directory at {path}.
Each entry is a two-element array `{name, type}` where type is one of
`"file"`, `"directory"`, `"link"`, or `"unknown"`. Follows symlinks.

**Parameters:**

- `{path}` (`string`) Directory path.
- `{opts?}` (`table?`) `depth` (integer, default 1): how many levels deep to recurse.

**Returns:** (`table?`, `string?`) Array of `{name, type}` entries, or nil plus an error message.

**Example:**

```lua
local entries, err = noon.fs.dir("src", { depth = 2 })
if err then return end
for _, e in ipairs(entries) do
  print(e[1], e[2]) -- "main.rs"  "file"
end
```

---

### `noon.fs.write()` {#noon-fs-write}

```lua
noon.fs.write({path}, {content})
```

Write {content} to the file at {path}, creating it if it does not exist
or overwriting it if it does.

**Parameters:**

- `{path}` (`string`) Destination file path. `~/` is expanded.
- `{content}` (`string`) Text to write.

**Returns:** (`true?`, `string?`) `true` on success, or nil plus an error message.

**Example:**

```lua
local ok, err = noon.fs.write("out.txt", "hello world")
if err then print("write failed: " .. err) end
```

---

### `noon.fs.rm()` {#noon-fs-rm}

```lua
noon.fs.rm({path})
```

Delete the file at {path}. Does not remove directories.

**Parameters:**

- `{path}` (`string`) Path to the file to remove.

**Returns:** (`true?`, `string?`) `true` on success, or nil plus an error message.

**Example:**

```lua
local ok, err = noon.fs.rm("temp.txt")
if err then print("rm failed: " .. err) end
```

---

### `noon.fs.mkdir()` {#noon-fs-mkdir}

```lua
noon.fs.mkdir({path}, {opts?})
```

Create the directory at {path}. Set `parents = true` to create
intermediate directories, like `mkdir -p`.

**Parameters:**

- `{path}` (`string`) Directory path to create.
- `{opts?}` (`table?`) `parents` (boolean, default false): create intermediate parent directories.

**Returns:** (`true?`, `string?`) `true` on success, or nil plus an error message.

**Example:**

```lua
noon.fs.mkdir("a/b/c", { parents = true })
```

---

### `noon.fs.glob()` {#noon-fs-glob}

```lua
noon.fs.glob({pattern}, {opts?})
```

Find files matching one or more glob patterns.
Respects `.gitignore` by default. Pass `sort = "mtime"` to get the most
recently modified files first.

**Parameters:**

- `{pattern}` (`string|string[]`) Glob pattern or array of patterns.
- `{opts?}` (`table?`) `path` (string): search root. `limit` (integer): max results. `gitignore` (boolean, default true): respect .gitignore. `sort` (string): `"mtime"` sorts newest first.

**Returns:** (`string[]?`, `string?`) Array of absolute file paths, or nil plus an error message.

**Example:**

```lua
local files, err = noon.fs.glob("**/*.lua", { path = "plugins", limit = 10 })
if err then return end
for _, f in ipairs(files) do print(f) end
```

---

### `noon.fs.grep()` {#noon-fs-grep}

```lua
noon.fs.grep({pattern}, {opts?})
```

Search file contents for a regex {pattern}. Returns structured matches
grouped by file, similar to ripgrep output.

Each result entry has a `path` and a list of `groups`. Each group contains
`lines`, where every line has `line_nr`, `text`, and `is_match`.

**Parameters:**

- `{pattern}` (`string`) Regular expression to search for.
- `{opts?}` (`table?`) `path` (string): search root. `include` (string): file glob filter (e.g. `"*.rs"`). `context_before` / `context_after` (integer): context lines around matches. `limit` (integer): max match groups. `max_line_bytes` (integer): skip lines longer than this.

**Returns:** (`table?`, `string?`) Array of `{path, groups}` tables, or nil plus an error message.

**Example:**

```lua
local hits, err = noon.fs.grep("TODO", { path = "src", include = "*.rs", limit = 5 })
if err then return end
for _, file in ipairs(hits) do
  for _, g in ipairs(file.groups) do
    for _, line in ipairs(g.lines) do
      if line.is_match then print(file.path .. ":" .. line.line_nr) end
    end
  end
end
```


## noon.image {#noon-image}

Small building blocks for working with images: probe metadata, decode
pixels, resize, and encode back to bytes. Plugins compose these freely.

Decoding is guarded against pixel-bomb attacks (50 MP limit).

```lua
local img = noon.image.decode(raw_bytes)
local small = img:resize(1024, 768)
local png = small:encode("png")
```

---

### `noon.image.probe()` {#noon-image-probe}

```lua
noon.image.probe({data})
```

Read image metadata (format, dimensions) from raw bytes without fully
decoding the pixels. Much faster than `decode` when you only need to
check the size or format.

Returns a table with `format` (string), `width` (integer), `height`
(integer), or `(nil, err)` if the bytes are not a recognized image.

**Parameters:**

- `{data}` (`string|buffer`) Raw image bytes.

**Returns:** (`table?`, `string?`) Info table, or `(nil, err)` on failure.

**Example:**

```lua
local info, err = noon.image.probe(raw_bytes)
if err then error(err) end
print(info.format, info.width, info.height)
```

---

### `noon.image.decode()` {#noon-image-decode}

```lua
noon.image.decode({data})
```

Decode raw image bytes into an Image handle you can resize and re-encode.
Images larger than 50 megapixels are rejected to prevent memory bombs.

**Parameters:**

- `{data}` (`string|buffer`) Raw image bytes.

**Returns:** ([`noon.image.Image?`](#noon-image-Image), `string?`) Decoded image, or `(nil, err)` on failure.

**Example:**

```lua
local img, err = noon.image.decode(raw_bytes)
if err then error(err) end
print(img:width() .. "x" .. img:height())
```


## noon.image.Image {#noon-image-Image}

A decoded image you can inspect, resize, and re-encode.

Get one from `noon.image.decode()`. The image data lives in memory
until the handle is garbage collected.

---

### `Image:width()` {#Image-width}

```lua
Image:width()
```

Get the width of the image in pixels.

**Returns:** (`integer`) Width in pixels.

---

### `Image:height()` {#Image-height}

```lua
Image:height()
```

Get the height of the image in pixels.

**Returns:** (`integer`) Height in pixels.

---

### `Image:resize()` {#Image-resize}

```lua
Image:resize({max_w}, {max_h})
```

Shrink the image to fit inside {max_w} x {max_h}, keeping the aspect
ratio. If the image already fits, it is returned as-is. Never upscales.

**Parameters:**

- `{max_w}` (`integer`) Maximum width in pixels. Must be positive.
- `{max_h}` (`integer`) Maximum height in pixels. Must be positive.

**Returns:** ([`noon.image.Image`](#noon-image-Image)) A new image handle (or the same one if no resize was needed).

**Example:**

```lua
local img = noon.image.decode(raw_bytes)
local small = img:resize(800, 600)
local encoded = small:encode("jpeg")
```

---

### `Image:encode()` {#Image-encode}

```lua
Image:encode({format})
```

Encode the image into raw bytes in the given format. Use this to prepare
images for sending over the network or writing to disk.

**Parameters:**

- `{format}` (`string`) Output format: `"png"`, `"jpeg"`, or `"jpg"`.

**Returns:** (`string`) Encoded image bytes.

**Example:**

```lua
local bytes = img:encode("png")
-- bytes is a Lua string containing the raw PNG data
```


## noon.interpreter {#noon-interpreter}

Run Python code in a memory-safe, time-limited sandbox.

The sandbox uses the monty interpreter. Python code can call back into
Lua-defined tools, and stdout is streamed line by line. Requires the
`run` permission.

```lua
local r, err = noon.interpreter.run("print('hello')", {
  timeout = 10,
  max_memory_mb = 128,
  on_output = function(line) print(line) end,
})
```

---

### `noon.interpreter.run()` {#noon-interpreter-run}

```lua
noon.interpreter.run({code}, {opts})
```

Run Python code in a sandboxed interpreter with memory and time limits.
Stdout lines are streamed to your {on_output} callback as they are produced.
If the Python code calls tools, those calls are dispatched to the Lua
functions you provide in {opts}.tools.

The result table has optional fields: `stdout` (string, trimmed combined
output) and `output` (string, the final expression value). On error, the
table is empty and the second return value is the error message.

**Parameters:**

- `{code}` (`string`) Python source code to execute.
- `{opts}` (`table`) Required fields:
  - `timeout` (`integer`) execution time limit in seconds.
  - `max_memory_mb` (`integer`) memory limit in megabytes.
  - `on_output` (`function`) called with each stdout line (string) as it is
    produced. Must not yield.

  Optional fields:

  - `tools` (`table?`) map of `name -> function` for tools the sandbox may call.
    Each function receives the tool input table and must return `(string)` or
    `(nil, err)`. Tool calls are batched and dispatched concurrently.

**Returns:** (`table`, `string?`) Result table, plus an error string on failure.

**Example:**

```lua
local result, err = noon.interpreter.run("print(2 + 2)", {
  timeout = 30,
  max_memory_mb = 256,
  on_output = function(line) print("py: " .. line) end,
})
if err then error(err) end
if result.stdout then print(result.stdout) end
```


## noon.json {#noon-json}

JSON encoding, decoding, and schema validation. Encode Lua
tables to JSON strings, decode JSON back into tables, and
optionally validate data against a JSON Schema.

```lua
local s = noon.json.encode({ ok = true })
local t = noon.json.decode(s)
```

---

### `noon.json.encode()` {#noon-json-encode}

```lua
noon.json.encode({value})
```

Turn a Lua value into a JSON string. Tables, strings, numbers,
booleans, and nil all work. Functions and userdata cannot be
serialized.

**Parameters:**

- `{value}` (`any`) Lua value to encode.

**Returns:** (`string?`, `string?`) JSON string, or nil plus an error.

**Example:**

```lua
local s, err = noon.json.encode({ name = "noon", version = 1 })
print(s) -- {"name":"noon","version":1}
```

---

### `noon.json.decode()` {#noon-json-decode}

```lua
noon.json.decode({str})
```

Parse a JSON string into a Lua value. Objects become tables and
arrays become 1-indexed sequences.

**Parameters:**

- `{str}` (`string`) JSON string to decode.

**Returns:** (`any?`, `string?`) Decoded value, or nil plus an error.

**Example:**

```lua
local t, err = noon.json.decode('{"x": 42}')
print(t.x) -- 42
```

---

### `noon.json.schema_validator()` {#noon-json-schema_validator}

```lua
noon.json.schema_validator({schema})
```

Compile a JSON Schema into a reusable validator object. Supports
draft-07, 2019-09, and 2020-12. Schema errors show up right away so
you catch mistakes before doing any real work.

**Parameters:**

- `{schema}` (`table`) JSON Schema as a Lua table.

**Returns:** ([`noon.json.SchemaValidator?`](#noon-json-SchemaValidator), `string?`) Validator, or nil plus an error.

**Example:**

```lua
local v, err = noon.json.schema_validator({
  type = "object",
  properties = { name = { type = "string" } },
  required = { "name" },
})
local errs = v:validate({ name = "noon" })
assert(errs == nil)
```


## noon.json.SchemaValidator {#noon-json-SchemaValidator}

A compiled JSON Schema validator. Create one with `noon.json.schema_validator()` and reuse it to validate many values without recompiling the schema each time.

---

### `SchemaValidator:validate()` {#SchemaValidator-validate}

```lua
SchemaValidator:validate({value})
```

Check {value} against the compiled schema. Returns nil when the value is valid. When validation fails, returns a list of human-readable error strings.

**Parameters:**

- `{value}` (`any`) The Lua value to validate.

**Returns:** (`table?`) Array of error strings, or nil if valid.

**Example:**

```lua
local errs = validator:validate({ name = 123 })
if errs then
for _, msg in ipairs(errs) do print(msg) end
end
```


## noon.keymap {#noon-keymap}

Key mappings, modeled after `vim.keymap`. If you have written a
Neovim keymap plugin before, this will feel familiar.

```lua
noon.keymap.set("n", "<C-t>", function()
  print("hello")
end, { desc = "Say hello" })
```

---

### `noon.keymap.set()` {#noon-keymap-set}

```lua
noon.keymap.set({mode}, {lhs}, {rhs}, {opts?})
```

Bind a key to a Lua function, just like `vim.keymap.set`. Only
normal mode (`"n"`) is supported right now. If {lhs} is already
mapped, the old binding is replaced and a warning is logged.

**Parameters:**

- `{mode}` (`string`) Mode letter. Currently only `"n"` is accepted.
- `{lhs}` (`string`) Key in Vim notation, e.g. `"<C-t>"`, `"<Space>"`, `"a"`.
- `{rhs}` (`function`) Called when the key is pressed.
- `{opts?}` (`table?`) Options:
  - `desc` (`string`) short description shown in the keymap list.

**Example:**

```lua
noon.keymap.set("n", "<C-t>", function()
  print("toggle!")
end, { desc = "Toggle panel" })
```

---

### `noon.keymap.del()` {#noon-keymap-del}

```lua
noon.keymap.del({mode}, {lhs})
```

Remove the mapping for {lhs} in {mode}. Does nothing if no mapping
exists for that key.

**Parameters:**

- `{mode}` (`string`) Mode letter (reserved for future modes).
- `{lhs}` (`string`) Key to unmap, in Vim notation.

**Example:**

```lua
noon.keymap.del("n", "<C-t>")
```


## noon.log {#noon-log}

Structured logging for plugins.

Each call emits a tracing event tagged with the calling plugin's name.
Messages show up in noon's log output, which you can view with `noon --log`.

```lua
noon.log.info("ready")
noon.log.warn("something looks off")
```

---

### `noon.log.debug()` {#noon-log-debug}

```lua
noon.log.debug({msg})
```

Emit a DEBUG-level log message. Useful for development and troubleshooting.
The message is tagged with the plugin name automatically.

**Parameters:**

- `{msg}` (`string`) Message to log.

**Example:**

```lua
noon.log.debug("loaded " .. #items .. " items")
```

---

### `noon.log.info()` {#noon-log-info}

```lua
noon.log.info({msg})
```

Emit an INFO-level log message. Good for normal operational events.

**Parameters:**

- `{msg}` (`string`) Message to log.

**Example:**

```lua
noon.log.info("plugin initialized")
```

---

### `noon.log.warn()` {#noon-log-warn}

```lua
noon.log.warn({msg})
```

Emit a WARN-level log message. Use for recoverable problems.

**Parameters:**

- `{msg}` (`string`) Message to log.

**Example:**

```lua
noon.log.warn("config file missing, using defaults")
```

---

### `noon.log.error()` {#noon-log-error}

```lua
noon.log.error({msg})
```

Emit an ERROR-level log message. Use for failures that need attention.

**Parameters:**

- `{msg}` (`string`) Message to log.

**Example:**

```lua
noon.log.error("failed to connect to API")
```


## noon.net {#noon-net}

HTTP client for fetching web content. All traffic goes over HTTPS
(plain HTTP is upgraded). Private and metadata IP addresses are
blocked to prevent SSRF. Failed requests (5xx) are retried
automatically.

```lua
local res, err = noon.net.request("https://example.com")
if res then print(res.body) end
```

---

### `noon.net.request()` {#noon-net-request}

```lua
noon.net.request({url}, {opts?})
```

Make an HTTP request and return the response body. Plain `http://`
URLs are automatically upgraded to `https://`. Requests to private
or metadata IP addresses are blocked for safety.

{opts} fields:
  `method` (string) HTTP verb (default `"GET"`).
  `headers` (table) Header name/value pairs.
  `body` (string) Request body.
  `timeout` (integer) Timeout in seconds, max 120 (default 30).
  `max_bytes` (integer) Max response size in bytes (default 5 MB).
  `retry` (integer) Retries on 5xx errors (default 3).

The response table has three fields: `body` (string), `status`
(integer), and `content_type` (string).

**Parameters:**

- `{url}` (`string`) URL starting with `http://` or `https://`.
- `{opts?}` (`table?`) Request options (see above).

**Returns:** (`table?`, `string?`) Response table, or nil plus an error string.

**Example:**

```lua
local res, err = noon.net.request("https://httpbin.org/get")
if err then
  print("failed: " .. err)
else
  print(res.status, res.body)
end
```


## noon.session {#noon-session}

Host session primitives. The interactive UI can run several sessions
at once; these functions let plugins list, create, focus, rename, and
delete them. Every call round-trips to the UI event loop and returns
the pair `(value, err)`. Without an interactive UI attached, every
call returns `nil, "no interactive UI attached"`.

---

### `noon.session.list()` {#noon-session-list}

```lua
noon.session.list()
```

Lists sessions stored for the current project. Answered from a
background scan, so a slow disk never blocks the UI.

**Returns:** (`table|nil`, `string|nil`) Array of `{id, title, updated_at}`, or nil and an error.

**Example:**

```lua
local stored, err = noon.session.list()
```

---

### `noon.session.live()` {#noon-session-live}

```lua
noon.session.live()
```

Lists the sessions currently running in this UI. Status is "working",
"needs_input", or "idle".

**Returns:** (`table|nil`, `string|nil`) Array of `{id, title, status, updated_at, focused}`, or nil and an error.

**Example:**

```lua
local live, err = noon.session.live()
```

---

### `noon.session.current()` {#noon-session-current}

```lua
noon.session.current()
```

Returns the id of the currently focused session.

**Returns:** (`string|nil`, `string|nil`) Session id, or nil and an error.

**Example:**

```lua
local id = noon.session.current()
```

---

### `noon.session.focus()` {#noon-session-focus}

```lua
noon.session.focus({id})
```

Switches the UI to the session with {id}. The session must be live.

**Parameters:**

- `{id}` (`string`) Session id, as returned by `list()` or `live()`.

**Returns:** (`boolean|nil`, `string|nil`) true on success, or nil and an error.

**Example:**

```lua
local _, err = noon.session.focus(id)
```

---

### `noon.session.delete()` {#noon-session-delete}

```lua
noon.session.delete({id})
```

Deletes a session and its stored history, cancelling it first if it
is running. The focused session cannot be deleted.

**Parameters:**

- `{id}` (`string`) Session id to delete.

**Returns:** (`boolean|nil`, `string|nil`) true on success, or nil and an error.

**Example:**

```lua
local _, err = noon.session.delete(id)
```

---

### `noon.session.new()` {#noon-session-new}

```lua
noon.session.new({opts?})
```

Starts a new session in the current project.

**Parameters:**

- `{opts?}` (`table?`) Optional fields: prompt (string) first user message

  to submit right away; focus (boolean) switch the UI to the new session.


**Returns:** (`string|nil`, `string|nil`) New session id, or nil and an error.

**Example:**

```lua
local id, err = noon.session.new({ prompt = "fix the tests", focus = true })
```

---

### `noon.session.prompt()` {#noon-session-prompt}

```lua
noon.session.prompt({text}, {opts?})
```

Sends {text} as a regular user prompt to a live session. The text is
never interpreted: slash commands, `exit`, and `!` shell prefixes are
all sent to the model verbatim. If the session is currently streaming,
the prompt is queued and picked up when the agent reaches it.

**Parameters:**

- `{text}` (`string`) The prompt to send. Must not be blank.
- `{opts?}` (`table?`) Optional fields: session (string) id of a live

  session; defaults to the focused one.


**Returns:** (`string|nil`, `string|nil`) "started" or "queued", or nil and an error.

**Example:**

```lua
local state, err = noon.session.prompt("run the tests", { session = id })
```

---

### `noon.session.set_title()` {#noon-session-set_title}

```lua
noon.session.set_title({opts})
```

Renames a session, live or stored.

**Parameters:**

- `{opts}` (`table`) Required fields: id (string) session to rename;
  - `title` (`string`) the new title.

**Returns:** (`boolean|nil`, `string|nil`) true on success, or nil and an error.

**Example:**

```lua
local _, err = noon.session.set_title({ id = id, title = "refactor" })
```


## noon.text {#noon-text}

Text transformation utilities.

Helper functions for converting between text formats.

```lua
local md = noon.text.html_to_markdown(html)
```

---

### `noon.text.html_to_markdown()` {#noon-text-html_to_markdown}

```lua
noon.text.html_to_markdown({html})
```

Convert an HTML string to Markdown.
Useful for cleaning up web content fetched with `noon.webfetch`.

**Parameters:**

- `{html}` (`string`) HTML source text.

**Returns:** (`string?`, `string?`) Markdown text on success, or nil plus an error message.

**Example:**

```lua
local md, err = noon.text.html_to_markdown("<h1>Hello</h1><p>world</p>")
if err then return end
print(md) -- "# Hello\n\nworld"
```


## noon.treesitter {#noon-treesitter}

Tree-sitter parsing and query API.

Mirrors `vim.treesitter` from Neovim, so plugins can be shared between the two.
Start with `get_parser()` to parse source code, then use `get_node_text()` and
the `query` sub-module to extract information from the syntax tree.

```lua
local parser, err = noon.treesitter.get_parser(source, "lua")
local trees = parser:parse()
local root = trees[1]:root()
```

---

### `noon.treesitter.get_parser()` {#noon-treesitter-get_parser}

```lua
noon.treesitter.get_parser({source}, {lang})
```

Creates a `LanguageTree` for {source} using the grammar named {lang}.
This is the main entry point for parsing source code with tree-sitter.
Signature matches `vim.treesitter.get_parser()`, so Neovim plugins can be copy-pasted.

**Parameters:**

- `{source}` (`string`) Source text to parse.
- `{lang}` (`string`) Language name, e.g. `"rust"` or `"lua"`.

**Returns:** ([`LanguageTree|nil`](#noon-treesitter-LanguageTree), `string|nil`) Parser, or nil and an error message.

**Example:**

```lua
local parser, err = noon.treesitter.get_parser(src, "lua")
if err then print("error: " .. err) end
```

---

### `noon.treesitter.get_string_parser()` {#noon-treesitter-get_string_parser}

```lua
noon.treesitter.get_string_parser({source}, {lang})
```

Alias for `get_parser`. Use whichever name you prefer.

**Parameters:**

- `{source}` (`string`) Source text to parse.
- `{lang}` (`string`) Language name.

**Returns:** ([`LanguageTree|nil`](#noon-treesitter-LanguageTree), `string|nil`) Parser, or nil and an error message.

---

### `noon.treesitter.get_node_text()` {#noon-treesitter-get_node_text}

```lua
noon.treesitter.get_node_text({node}, {source})
```

Gets the text that {node} covers in {source}.
Useful when you have a captured node and need the actual source substring.

**Parameters:**

- `{node}` ([`Node`](#noon-treesitter-Node)) The node whose text you want.
- `{source}` (`string`) Original source text the tree was parsed from.

**Returns:** (`string`) Substring covered by the node.

**Example:**

```lua
local text = noon.treesitter.get_node_text(node, source)
print(text)
```

---

### `noon.treesitter.get_node_range()` {#noon-treesitter-get_node_range}

```lua
noon.treesitter.get_node_range({node})
```

Returns the range of {node} as four 0-based integers: start_row, start_col, end_row, end_col.

**Parameters:**

- `{node}` ([`Node`](#noon-treesitter-Node)) The node to query.

**Returns:** (`integer`, `integer`, `integer`, `integer`) start_row, start_col, end_row, end_col.

**Example:**

```lua
local sr, sc, er, ec = noon.treesitter.get_node_range(node)
```

---

### `noon.treesitter.get_range()` {#noon-treesitter-get_range}

```lua
noon.treesitter.get_range({node})
```

Returns a six-element table for {node}: `{start_row, start_col, start_byte, end_row, end_col, end_byte}`.
This gives you byte offsets in addition to row/column positions.

**Parameters:**

- `{node}` ([`Node`](#noon-treesitter-Node)) The node to query.

**Returns:** (`table`) Six-element array: start_row, start_col, start_byte, end_row, end_col, end_byte.

**Example:**

```lua
local r = noon.treesitter.get_range(node)
print("bytes: " .. r[3] .. "-" .. r[6])
```

---

### `noon.treesitter.is_ancestor()` {#noon-treesitter-is_ancestor}

```lua
noon.treesitter.is_ancestor({dest}, {source})
```

Checks whether {dest} is an ancestor of {source} (or the same node).
Walks up from {source} toward the root looking for {dest}.

**Parameters:**

- `{dest}` ([`Node`](#noon-treesitter-Node)) Potential ancestor node.
- `{source}` ([`Node`](#noon-treesitter-Node)) Node to check ancestry for.

**Returns:** (`boolean`)

---

### `noon.treesitter.is_in_node_range()` {#noon-treesitter-is_in_node_range}

```lua
noon.treesitter.is_in_node_range({node}, {line}, {col})
```

Checks whether the 0-based position ({line}, {col}) falls inside {node}.
Handy for cursor-position checks.

**Parameters:**

- `{node}` ([`Node`](#noon-treesitter-Node)) Node to test against.
- `{line}` (`integer`) 0-based line number.
- `{col}` (`integer`) 0-based column number.

**Returns:** (`boolean`)

---

### `noon.treesitter.node_contains()` {#noon-treesitter-node_contains}

```lua
noon.treesitter.node_contains({node}, {range})
```

Checks whether {node} fully contains the given {range}.

**Parameters:**

- `{node}` ([`Node`](#noon-treesitter-Node)) Node to test.
- `{range}` (`table`) Four-element array `{start_row, start_col, end_row, end_col}`.

**Returns:** (`boolean`)

---

### `noon.treesitter.get_node()` {#noon-treesitter-get_node}

```lua
noon.treesitter.get_node({opts?})
```

Placeholder for cursor-based node lookup (not yet implemented, always returns nil).

**Parameters:**

- `{opts?}` (`table?`) Options (currently unused).

**Returns:** ([`Node|nil`](#noon-treesitter-Node)) Always nil.


## noon.treesitter.language {#noon-treesitter-language}

Language registry for tree-sitter grammars.

Mirrors `vim.treesitter.language`. Use these functions to register grammars,
map filetypes to languages, and inspect available node types.

```lua
noon.treesitter.language.add("lua")
noon.treesitter.language.register("lua", "luau")
```

---

### `noon.treesitter.language.add()` {#noon-treesitter-language-add}

```lua
noon.treesitter.language.add({lang}, {opts?})
```

Registers {lang} for use with tree-sitter.
Call this to confirm a language grammar is available. Throws if {lang} is unknown.
Custom grammar paths are not yet supported.

**Parameters:**

- `{lang}` (`string`) Language name, e.g. `"rust"`.
- `{opts?}` (`table?`) Options table (the `path` key is not yet supported).

**Example:**

```lua
noon.treesitter.language.add("lua")
```

---

### `noon.treesitter.language.register()` {#noon-treesitter-language-register}

```lua
noon.treesitter.language.register({lang}, {filetype})
```

Associates {lang} with one or more filetypes, so you can look up the right
parser language for a given filetype later with `get_lang()`.

**Parameters:**

- `{lang}` (`string`) Language name.
- `{filetype}` (`string|table`) A single filetype string or an array of filetype strings.

**Example:**

```lua
noon.treesitter.language.register("typescript", { "ts", "tsx" })
```

---

### `noon.treesitter.language.get_lang()` {#noon-treesitter-language-get_lang}

```lua
noon.treesitter.language.get_lang({filetype})
```

Looks up the tree-sitter language name for {filetype}.
Returns the registered language, or falls back to {filetype} itself if
a grammar with that name exists. Returns nil when nothing matches.

**Parameters:**

- `{filetype}` (`string`) Filetype to look up, e.g. `"ts"`.

**Returns:** (`string|nil`) Language name, or nil.

**Example:**

```lua
local lang = noon.treesitter.language.get_lang("tsx")
if lang then print(lang) end -- "typescript"
```

---

### `noon.treesitter.language.get_filetypes()` {#noon-treesitter-language-get_filetypes}

```lua
noon.treesitter.language.get_filetypes({lang})
```

Returns all filetypes that have been registered for {lang}.

**Parameters:**

- `{lang}` (`string`) Language name.

**Returns:** (`table`) Array of filetype strings.

**Example:**

```lua
local fts = noon.treesitter.language.get_filetypes("typescript")
-- { "ts", "tsx" }
```

---

### `noon.treesitter.language.inspect()` {#noon-treesitter-language-inspect}

```lua
noon.treesitter.language.inspect({lang})
```

Returns metadata about the grammar for {lang}.
Useful for debugging or discovering which node types and fields a grammar defines.

**Parameters:**

- `{lang}` (`string`) Language name.

**Returns:** (`table`) Table with keys `abi_version` (integer), `node_types` (string[]), `fields` (string[]).

**Example:**

```lua
local info = noon.treesitter.language.inspect("lua")
print("ABI: " .. info.abi_version)
for _, nt in ipairs(info.node_types) do print(nt) end
```


## noon.treesitter.query {#noon-treesitter-query}

Query compilation and lookup.

Mirrors `vim.treesitter.query`. Use `parse()` to compile a tree-sitter
query string into a `Query` object you can run against parsed trees.

```lua
local q = noon.treesitter.query.parse("lua", "(string) @str")
```

---

### `noon.treesitter.query.parse()` {#noon-treesitter-query-parse}

```lua
noon.treesitter.query.parse({lang}, {query})
```

Compiles a tree-sitter query string for {lang}.
Throws if the language is unknown or the query has a syntax error.

**Parameters:**

- `{lang}` (`string`) Language name, e.g. `"lua"`.
- `{query}` (`string`) Tree-sitter S-expression query.

**Returns:** ([`Query`](#noon-treesitter-Query)) Compiled query object.

**Example:**

```lua
local q = noon.treesitter.query.parse("lua", "(identifier) @id")
```

---

### `noon.treesitter.query.get()` {#noon-treesitter-query-get}

```lua
noon.treesitter.query.get({lang}, {name})
```

Looks up a named built-in query for {lang} (not yet implemented, always returns nil).

**Parameters:**

- `{lang}` (`string`) Language name.
- `{name}` (`string`) Query name, e.g. `"highlights"`.

**Returns:** ([`Query|nil`](#noon-treesitter-Query)) Query object, or nil if not found.


## noon.treesitter.Query {#noon-treesitter-Query}

A compiled tree-sitter query.

Get one by calling `noon.treesitter.query.parse(lang, query_string)`.
Then use `:iter_captures()` or `:iter_matches()` to run it against a syntax tree.

```lua
local q = noon.treesitter.query.parse("lua", "(identifier) @id")
for idx, node, meta in q:iter_captures(root, source) do
  print(node:type())
end
```

---

### `Query:iter_captures()` {#Query-iter_captures}

```lua
Query:iter_captures({node}, {source}, {start_row?}, {stop_row?})
```

Iterates over every capture matched by this query. Each call to the returned iterator yields `(capture_index, node, metadata, match, active)`. Use this when you care about individual captures rather than whole pattern matches.

**Parameters:**

- `{node}` ([`Node`](#noon-treesitter-Node)) Root node to search within.
- `{source}` (`string`) Source text the tree was parsed from.
- `{start_row?}` (`integer`) Only match rows >= this value (0-based).
- `{stop_row?}` (`integer`) Only match rows < this value (0-based).

**Returns:** (`function`) Iterator yielding (integer, Node, table, table, integer).

**Example:**

```lua
local q = noon.treesitter.query.parse("lua", "(identifier) @id")
for idx, node, meta in q:iter_captures(root, source) do
  print(idx, node:type())
end
```

---

### `Query:iter_matches()` {#Query-iter_matches}

```lua
Query:iter_matches({node}, {source}, {start_row?}, {stop_row?})
```

Iterates over every full pattern match in this query. Each call to the returned iterator yields `(pattern_index, captures, metadata, active)` where captures is a table keyed by capture index. Use this when you need all captures for a pattern together.

**Parameters:**

- `{node}` ([`Node`](#noon-treesitter-Node)) Root node to search within.
- `{source}` (`string`) Source text the tree was parsed from.
- `{start_row?}` (`integer`) Only match rows >= this value (0-based).
- `{stop_row?}` (`integer`) Only match rows < this value (0-based).

**Returns:** (`function`) Iterator yielding (integer, table, table, integer).

**Example:**

```lua
local q = noon.treesitter.query.parse("lua", "(function_declaration name: (identifier) @name)"
)
for pat, captures, meta in q:iter_matches(root, source) do
  for cap_idx, nodes in pairs(captures) do
    print(nodes[1]:type())
  end
end
```


## noon.treesitter.Tree {#noon-treesitter-Tree}

A parsed syntax tree.

Obtained from `LanguageTree:parse()` or `LanguageTree:trees()`.
Call `:root()` to get the root node and start traversing.

```lua
local trees = parser:parse()
local root = trees[1]:root()
```

---

### `Tree:root()` {#Tree-root}

```lua
Tree:root()
```

Returns the root node of this tree. This is where you start walking
the syntax tree or running queries.

**Returns:** ([`Node`](#noon-treesitter-Node)) Root node.

**Example:**

```lua
local root = tree:root()
print(root:type()) -- e.g. "chunk" for Lua
```

---

### `Tree:copy()` {#Tree-copy}

```lua
Tree:copy()
```

Returns an independent copy of this tree.
Edits to the copy will not affect the original.

**Returns:** ([`Tree`](#noon-treesitter-Tree)) A new Tree with the same content.


## noon.treesitter.Node {#noon-treesitter-Node}

A single node in a parsed syntax tree.

Nodes are obtained from `Tree:root()`, navigation methods like `:child()`,
or from query captures. Each node knows its type, range, and children.

```lua
local root = tree:root()
print(root:type(), root:child_count())
for child, field in root:iter_children() do
  print(child:type(), field)
end
```

---

### `Node:type()` {#Node-type}

```lua
Node:type()
```

Returns the grammar type name for this node, like `"function_definition"` or `"identifier"`.

**Returns:** (`string`) Grammar type name.

---

### `Node:symbol()` {#Node-symbol}

```lua
Node:symbol()
```

Returns the numeric symbol id for this node's grammar type.
Two nodes with the same type always share the same symbol id.

**Returns:** (`integer`) Symbol id.

---

### `Node:id()` {#Node-id}

```lua
Node:id()
```

Returns a unique string identifier for this specific node in the tree.
Useful for deduplication or as a table key.

**Returns:** (`string`) Node identity string.

---

### `Node:range()` {#Node-range}

```lua
Node:range({include_bytes?})
```

Returns the range of this node as multiple return values.
Without {include_bytes}: `start_row, start_col, end_row, end_col`.
With {include_bytes} set to true: `start_row, start_col, start_byte, end_row, end_col, end_byte`.

**Parameters:**

- `{include_bytes?}` (`boolean`) When true, byte offsets are included in the return values.

**Returns:** (`integer`, `integer`, `integer`, `integer`) Four values, or six when include_bytes is true.

**Example:**

```lua
local sr, sc, er, ec = node:range()
local sr, sc, sb, er, ec, eb = node:range(true)
```

---

### `Node:start()` {#Node-start}

```lua
Node:start()
```

Returns the start position of this node: row, column, and byte offset (all 0-based).

**Returns:** (`integer`, `integer`, `integer`) start_row, start_col, start_byte.

---

### `Node:end_()` {#Node-end_}

```lua
Node:end_()
```

Returns the end position of this node: row, column, and byte offset (all 0-based).

**Returns:** (`integer`, `integer`, `integer`) end_row, end_col, end_byte.

---

### `Node:byte_length()` {#Node-byte_length}

```lua
Node:byte_length()
```

Returns how many bytes this node spans in the source text.

**Returns:** (`integer`) Byte length.

---

### `Node:child()` {#Node-child}

```lua
Node:child({index})
```

Returns the child at position {index} (0-based), including anonymous nodes like punctuation.
Returns nil if {index} is out of bounds.

**Parameters:**

- `{index}` (`integer`) 0-based child index.

**Returns:** ([`Node|nil`](#noon-treesitter-Node)) Child node, or nil.

---

### `Node:named_child()` {#Node-named_child}

```lua
Node:named_child({index})
```

Returns the named child at position {index} (0-based), skipping anonymous nodes.
Returns nil if {index} is out of bounds.

**Parameters:**

- `{index}` (`integer`) 0-based named child index.

**Returns:** ([`Node|nil`](#noon-treesitter-Node)) Named child node, or nil.

---

### `Node:child_count()` {#Node-child_count}

```lua
Node:child_count()
```

Returns the total number of children, including anonymous nodes.

**Returns:** (`integer`) Child count.

---

### `Node:named_child_count()` {#Node-named_child_count}

```lua
Node:named_child_count()
```

Returns the number of named children (skipping anonymous punctuation nodes).

**Returns:** (`integer`) Named child count.

---

### `Node:children()` {#Node-children}

```lua
Node:children()
```

Returns all children (named and anonymous) as a Lua table.

**Returns:** (`table`) Array of Node.

**Example:**

```lua
for _, child in ipairs(node:children()) do
  print(child:type())
end
```

---

### `Node:named_children()` {#Node-named_children}

```lua
Node:named_children()
```

Returns all named children as a Lua table, skipping anonymous nodes.

**Returns:** (`table`) Array of Node.

---

### `Node:iter_children()` {#Node-iter_children}

```lua
Node:iter_children()
```

Returns an iterator function that yields `(child, field_name)` for every child.
The field name is nil for children that are not assigned to a grammar field.

**Returns:** (`function`) Iterator yielding (Node, string|nil).

**Example:**

```lua
for child, field in node:iter_children() do
  if field then print(field .. ": " .. child:type()) end
end
```

---

### `Node:field()` {#Node-field}

```lua
Node:field({name})
```

Returns all children assigned to the grammar field {name} as a table.
For example, a function node might have a `"name"` or `"body"` field.

**Parameters:**

- `{name}` (`string`) Field name defined in the grammar.

**Returns:** (`table`) Array of Node.

**Example:**

```lua
local bodies = node:field("body")
```

---

### `Node:parent()` {#Node-parent}

```lua
Node:parent()
```

Returns the parent of this node, or nil if this is the root.

**Returns:** ([`Node|nil`](#noon-treesitter-Node)) Parent node.

---

### `Node:next_sibling()` {#Node-next_sibling}

```lua
Node:next_sibling()
```

Returns the next sibling (named or anonymous), or nil if this is the last child.

**Returns:** ([`Node|nil`](#noon-treesitter-Node)) Next sibling.

---

### `Node:prev_sibling()` {#Node-prev_sibling}

```lua
Node:prev_sibling()
```

Returns the previous sibling (named or anonymous), or nil if this is the first child.

**Returns:** ([`Node|nil`](#noon-treesitter-Node)) Previous sibling.

---

### `Node:next_named_sibling()` {#Node-next_named_sibling}

```lua
Node:next_named_sibling()
```

Returns the next named sibling, skipping anonymous nodes. Returns nil at the end.

**Returns:** ([`Node|nil`](#noon-treesitter-Node)) Next named sibling.

---

### `Node:prev_named_sibling()` {#Node-prev_named_sibling}

```lua
Node:prev_named_sibling()
```

Returns the previous named sibling, skipping anonymous nodes. Returns nil at the start.

**Returns:** ([`Node|nil`](#noon-treesitter-Node)) Previous named sibling.

---

### `Node:child_with_descendant()` {#Node-child_with_descendant}

```lua
Node:child_with_descendant({descendant})
```

Finds the direct child of this node that contains {descendant}.
Returns nil if {descendant} is not actually inside this node.

**Parameters:**

- `{descendant}` ([`Node`](#noon-treesitter-Node)) A node that may be a descendant.

**Returns:** ([`Node|nil`](#noon-treesitter-Node)) Direct child containing the descendant.

---

### `Node:descendant_for_range()` {#Node-descendant_for_range}

```lua
Node:descendant_for_range({start_row}, {start_col}, {end_row}, {end_col})
```

Finds the smallest node inside this node that spans the given point range.
Includes both named and anonymous nodes.

**Parameters:**

- `{start_row}` (`integer`) Start row (0-based).
- `{start_col}` (`integer`) Start column (0-based).
- `{end_row}` (`integer`) End row (0-based).
- `{end_col}` (`integer`) End column (0-based).

**Returns:** ([`Node|nil`](#noon-treesitter-Node)) Smallest node covering the range, or nil.

---

### `Node:named_descendant_for_range()` {#Node-named_descendant_for_range}

```lua
Node:named_descendant_for_range({start_row}, {start_col}, {end_row}, {end_col})
```

Like `descendant_for_range`, but only considers named nodes.

**Parameters:**

- `{start_row}` (`integer`) Start row (0-based).
- `{start_col}` (`integer`) Start column (0-based).
- `{end_row}` (`integer`) End row (0-based).
- `{end_col}` (`integer`) End column (0-based).

**Returns:** ([`Node|nil`](#noon-treesitter-Node)) Smallest named node covering the range, or nil.

---

### `Node:named()` {#Node-named}

```lua
Node:named()
```

Returns true if this is a named node (not anonymous punctuation like `,` or `(`).

**Returns:** (`boolean`)

---

### `Node:extra()` {#Node-extra}

```lua
Node:extra()
```

Returns true if this node is an "extra" (like a comment) that can appear anywhere in the grammar.

**Returns:** (`boolean`)

---

### `Node:missing()` {#Node-missing}

```lua
Node:missing()
```

Returns true if this node is "missing", meaning it was inserted by the parser during error recovery.

**Returns:** (`boolean`)

---

### `Node:has_error()` {#Node-has_error}

```lua
Node:has_error()
```

Returns true if this node or any of its descendants contain a syntax error.

**Returns:** (`boolean`)

---

### `Node:has_changes()` {#Node-has_changes}

```lua
Node:has_changes()
```

Returns true if this node has been marked as changed since the last parse.

**Returns:** (`boolean`)

---

### `Node:equal()` {#Node-equal}

```lua
Node:equal({other})
```

Returns true if this node and {other} are the same node in the tree.

**Parameters:**

- `{other}` ([`Node`](#noon-treesitter-Node)) Node to compare against.

**Returns:** (`boolean`)

---

### `Node:sexpr()` {#Node-sexpr}

```lua
Node:sexpr()
```

Returns the S-expression (lisp-like) string for this node and its children.
Handy for debugging the tree structure.

**Returns:** (`string`) S-expression.

**Example:**

```lua
print(node:sexpr()) -- e.g. "(identifier)"
```

---

### `Node:tree()` {#Node-tree}

```lua
Node:tree()
```

Returns the Tree that this node belongs to.

**Returns:** ([`Tree`](#noon-treesitter-Tree)) The owning tree.


## noon.treesitter.LanguageTree {#noon-treesitter-LanguageTree}

Manages parsing of a source string for a single language.

Obtained from `noon.treesitter.get_parser()` or `noon.treesitter.get_string_parser()`.
Call `:parse()` to get the syntax tree, then use `:root()` on the tree to start walking nodes.

```lua
local parser, err = noon.treesitter.get_parser(source, "lua")
if not err then
  local trees = parser:parse()
  local root = trees[1]:root()
end
```

---

### `LanguageTree:parse()` {#LanguageTree-parse}

```lua
LanguageTree:parse({range?})
```

Parses the source and returns a table containing the resulting Tree.
The tree is cached, so calling this again is cheap.

**Parameters:**

- `{range?}` (`table`) Unused. Accepted for API compatibility.

**Returns:** (`table`) Array with one Tree element.

**Example:**

```lua
local trees = parser:parse()
local root = trees[1]:root()
```

---

### `LanguageTree:lang()` {#LanguageTree-lang}

```lua
LanguageTree:lang()
```

Returns the language name this parser was created with.

**Returns:** (`string`) Language name, e.g. `"lua"`.

---

### `LanguageTree:children()` {#LanguageTree-children}

```lua
LanguageTree:children()
```

Returns child LanguageTrees for injected languages.
Not yet implemented, always returns an empty table.

**Returns:** (`table`) Empty table.

---

### `LanguageTree:trees()` {#LanguageTree-trees}

```lua
LanguageTree:trees()
```

Returns all parsed trees as a table (at most one for now).
Returns an empty table if `parse()` has not been called yet.

**Returns:** (`table`) Array of Tree.

---

### `LanguageTree:source()` {#LanguageTree-source}

```lua
LanguageTree:source()
```

Returns the source string this parser was created with.

**Returns:** (`string`) The original source text.

---

### `LanguageTree:is_valid()` {#LanguageTree-is_valid}

```lua
LanguageTree:is_valid({exclude_children?}, {range?})
```

Checks whether the parse tree is still valid.
Not yet implemented, always returns true.

**Parameters:**

- `{exclude_children?}` (`boolean`) Unused.
- `{range?}` (`table`) Unused.

**Returns:** (`boolean`) Always true.

---

### `LanguageTree:for_each_tree()` {#LanguageTree-for_each_tree}

```lua
LanguageTree:for_each_tree({fn})
```

Calls {fn} with `(tree, nil)` for the parsed tree.
Triggers a parse if the tree has not been parsed yet.

**Parameters:**

- `{fn}` (`function`) Callback receiving `(Tree, nil)`.

**Example:**

```lua
parser:for_each_tree(function(tree, _)
  print(tree:root():type())
end)
```

---

### `LanguageTree:included_regions()` {#LanguageTree-included_regions}

```lua
LanguageTree:included_regions()
```

Returns the regions this parser covers.
Not yet implemented, always returns a table with one empty region.

**Returns:** (`table`) Array with one empty table.

---

### `LanguageTree:contains()` {#LanguageTree-contains}

```lua
LanguageTree:contains({range})
```

Checks whether this parser covers the given {range}.
Not yet implemented, always returns true.

**Parameters:**

- `{range}` (`table`) Range to check (currently unused).

**Returns:** (`boolean`) Always true.

---

### `LanguageTree:destroy()` {#LanguageTree-destroy}

```lua
LanguageTree:destroy()
```

Drops the cached parse tree and frees its memory.
After calling this, the next `parse()` will re-parse from scratch.


## noon.ui {#noon-ui}

Functions for building interactive UI. Create buffers to hold
content, open floating or split windows to display them, highlight
code, render markdown, and show status hints.

```lua
local buf = noon.ui.buf()
buf:line("hello from my plugin!")
local win = noon.ui.open_win(buf, { title = "Greeting", width = "50%", height = 5 })
```

---

### `noon.ui.buf()` {#noon-ui-buf}

```lua
noon.ui.buf()
```

Creates a new buffer for building UI content. The first buffer you
create in a task becomes the "live" buffer, streamed to the UI while
your tool runs. Create more buffers for secondary content like
floating windows.

**Returns:** ([`Buf`](#noon-ui-Buf)) Buffer handle.

**Example:**

```lua
local buf = noon.ui.buf()
buf:line("hello world")
```

---

### `noon.ui.theme_color()` {#noon-ui-theme_color}

```lua
noon.ui.theme_color({name})
```

Looks up a semantic color from the current theme. Use this to keep
your plugin's colors consistent with the rest of the UI.

**Parameters:**

- `{name}` (`string`) Semantic color name, e.g. "accent" or "background".

**Returns:** (`string|nil`) "#rrggbb" hex color, or nil if the name is unknown.

**Example:**

```lua
local accent = noon.ui.theme_color("accent")
if accent then
  buf:line({ { "note", { fg = accent, bold = true } } })
end
```

---

### `noon.ui.highlight()` {#noon-ui-highlight}

```lua
noon.ui.highlight({code}, {lang}, {opts?})
```

Syntax-highlights a chunk of source code. Returns a table of styled
lines that you can feed into a buffer. Each line is a list of
`{text, style}` spans where style is a `{fg, bold?, italic?, underline?}` table.

**Parameters:**

- `{code}` (`string`) Source text to highlight.
- `{lang}` (`string`) Language identifier, e.g. "rust", "python".
- `{opts?}` (`table?`) Options. Fields:
  - `independent` (`boolean`) highlight each line without cross-line context. Default false.
  - `prefix` (`string`) prepend to the source before highlighting (affects token context). Default "".

**Returns:** (`table`) Lines: `{ { {text, style}, ... }, ... }`. Each style is `{fg, bold?, italic?, underline?}`.

**Example:**

```lua
local lines = noon.ui.highlight("fn main() {}", "rust")
for _, spans in ipairs(lines) do
  buf:line(spans)
end
```

---

### `noon.ui.markdown()` {#noon-ui-markdown}

```lua
noon.ui.markdown({text}, {width})
```

Renders Markdown into styled lines ready to display in a buffer.
Each span's style is either a named string ("bold", "heading",
"inline_code", etc.) or a `{fg, bold?, italic?, underline?}` table
for syntax-highlighted code blocks.

**Parameters:**

- `{text}` (`string`) Markdown source.
- `{width}` (`integer`) Wrap width in columns.

**Returns:** (`table`) Lines: `{ { {text, style}, ... }, ... }`.

**Example:**

```lua
local size = noon.ui.terminal_size()
local lines = noon.ui.markdown("# Hello\n\nSome **bold** text.", size.cols)
for _, spans in ipairs(lines) do
  buf:line(spans)
end
```

---

### `noon.ui.humantime()` {#noon-ui-humantime}

```lua
noon.ui.humantime({secs})
```

Formats a number of seconds into a short, human-friendly string.
Useful for displaying elapsed time in status messages.

**Parameters:**

- `{secs}` (`integer`) Duration in seconds.

**Returns:** (`string`) Human-readable duration, e.g. "1m30s".

**Example:**

```lua
noon.ui.humantime(90)   -- "1m30s"
noon.ui.humantime(3661) -- "1h1m1s"
```

---

### `noon.ui.terminal_size()` {#noon-ui-terminal_size}

```lua
noon.ui.terminal_size()
```

Returns the current terminal size. Handy for sizing floating windows
or wrapping text to fit the screen.

**Returns:** (`table`) `{cols, rows}`, terminal width and height in characters.

**Example:**

```lua
local size = noon.ui.terminal_size()
local half_width = math.floor(size.cols / 2)
```

---

### `noon.ui.flash()` {#noon-ui-flash}

```lua
noon.ui.flash({msg})
```

Shows a brief message in the status bar. The message disappears
after a short time. Good for confirming an action like "copied!"
or showing a transient warning.

**Parameters:**

- `{msg}` (`string`) Message text.

**Example:**

```lua
noon.ui.flash("Copied to clipboard!")
```

---

### `noon.ui.open_editor()` {#noon-ui-open_editor}

```lua
noon.ui.open_editor({path})
```

Opens {path} in the user's `$EDITOR` (e.g. vim, nano) and waits for
it to close. This suspends the TUI while the editor is running.
Returns the editor's exit code so you can check if the user saved.

**Parameters:**

- `{path}` (`string`) File to open.

**Returns:** (`integer`) Editor exit code, or -1 if the action could not be dispatched.

**Example:**

```lua
local code = noon.ui.open_editor("/tmp/scratch.lua")
if code == 0 then
  noon.ui.flash("File saved")
end
```

---

### `noon.ui.open_win()` {#noon-ui-open_win}

```lua
noon.ui.open_win({buf}, {opts})
```

Opens a floating or split window that displays the contents of {buf}.
Returns a Win handle you can use to receive events, update layout,
and close the window when you are done.

**Parameters:**

- `{buf}` ([`Buf`](#noon-ui-Buf)) Buffer to display.
- `{opts}` (`table`) Float configuration. Fields:
  - `width` (`integer|string`) window width. Integer for absolute columns; "N%" for percent of terminal width. Default "60%".
  - `height` (`integer|string`) window height. Integer for absolute rows; "N%" for percent of terminal height. Default "70%".
  - `row` (`integer?`) row offset from the anchor corner. Negative values move up.
  - `col` (`integer?`) column offset from the anchor corner.
  - `anchor` (`string`) corner the (row, col) offset is relative to. One of "NW" (default), "NE", "SW", "SE".
  - `border` (`string`) border style. One of "rounded" (default), "single", "double", "none".
  - `title` (`string`) text shown in the top border. Default "".
  - `title_pos` (`string`) title alignment. One of "left" (default), "center", "right".
  - `footer` (`table`) key-hint pairs shown in the bottom border. Each entry is {key, label}.
  - `zindex` (`integer`) stacking order. Default 50.
  - `cursor_line` (`boolean`) highlight the focused row. Default false.
  - `reserved_top` (`integer`) rows reserved at the top of the content area. Default 0.
  - `reserved_bottom` (`integer`) rows reserved at the bottom of the content area. Default 0.
  - `split` (`string`) dock the window to an edge instead of floating. One of "above", "below", "left", "right", "panel", or "" (floating, default).
  - `order` (`integer`) paint order among split windows at the same edge. Default 50.
  - `focus` (`boolean`) whether the window takes keyboard focus on open. Default true.
  - `visible` (`boolean`) whether the window is initially visible. Default true.

**Returns:** ([`Win`](#noon-ui-Win)) Window handle.

**Example:**

```lua
local buf = noon.ui.buf()
buf:line("Pick an option:")
local win = noon.ui.open_win(buf, {
  title = "Menu",
  width = "50%",
  height = 10,
  cursor_line = true,
  footer = { { "q", "quit" }, { "Enter", "select" } },
})
```

---

### `noon.ui.set_status_hint()` {#noon-ui-set_status_hint}

```lua
noon.ui.set_status_hint({spans})
```

Shows key hints in the status bar for your plugin. Each hint is a {key, label} pair. Pass nil to clear your plugin's hints. Only your own hints are affected, other plugins keep theirs.

**Parameters:**

- `{spans}` (`table|nil`) Sequence of {key, label} pairs, e.g. `{{"q", "quit"}, {"j", "down"}}`. Pass nil to remove the plugin's hints.

**Example:**

```lua
noon.ui.set_status_hint({ {"q", "quit"}, {"j", "down"} })
-- later, clear them:
noon.ui.set_status_hint(nil)
```


## noon.ui.Win {#noon-ui-Win}

Handle to a floating or split window. You get one from
`noon.ui.open_win()`. Use `recv()` in a loop to handle keyboard
input, and call `close()` when done.

Fields: `width`, `height` (initial content dimensions in columns/rows),
`visible` (current visibility).

```lua
local win = noon.ui.open_win(buf, { title = "Demo" })
while true do
  local ev = win:recv()
  if not ev or ev.key == "q" then break end
end
win:close()
```

---

### `Win:recv()` {#Win-recv}

```lua
Win:recv({timeout_ms?})
```

Waits for the next event from this window. Call this in a loop to build an interactive UI. Returns nil once the window is closed or the channel disconnects. Pass {timeout_ms} to also get `{type="timeout"}` events so your plugin can animate while idle.

Event tables by type:
- `{type="key", key}` -- keypress. Key is a string like "q", "j", or "esc".
- `{type="resize", width, height}` -- terminal was resized.
- `{type="paste", text}` -- bracketed paste.
- `{type="close"}` -- window was closed externally.
- `{type="timeout"}` -- no event arrived within {timeout_ms}.

**Parameters:**

- `{timeout_ms?}` (`integer`) Max milliseconds to wait before a timeout event is returned.

**Returns:** (`table|nil`) Event table, or nil if the window has closed.

**Example:**

```lua
while true do
  local ev = win:recv()
  if not ev or ev.key == "q" then break end
  if ev.type == "key" and ev.key == "j" then
    -- move cursor down
  end
end
win:close()
```

---

### `Win:set_config()` {#Win-set_config}

```lua
Win:set_config({opts})
```

Updates the window layout on the fly. Only the fields you include in
{opts} are changed, everything else stays the same.

**Parameters:**

- `{opts}` (`table`) Partial float config. Accepted fields:
  - `title` (`string`) border title text.
  - `title_pos` (`string`) title alignment, "left", "center", or "right".
  - `footer` (`table`) key-hint pairs `{{key, label}, ...}` shown in the bottom border.
  - `border` (`string`) "rounded", "single", "double", or "none".
  - `anchor` (`string`) corner origin, "NW", "NE", "SW", or "SE".
  - `width` (`integer|string`) new width; integer or "N%".
  - `height` (`integer|string`) new height; integer or "N%".
  - `zindex` (`integer`) stacking order.
  - `cursor_line` (`boolean`) highlight the focused row.
  - `reserved_top` (`integer`) rows reserved at the top of the content area.
  - `split` (`string`) edge docking, "above", "below", "left", "right", "panel", or "".
  - `order` (`integer`) paint order among split windows.

**Example:**

```lua
win:set_config({ title = "Updated!", width = "80%" })
```

---

### `Win:set_cursor()` {#Win-set_cursor}

```lua
Win:set_cursor({row})
```

Moves the highlighted cursor line to {row} (1-indexed). Only has a
visible effect when the window was opened with `cursor_line = true`.

**Parameters:**

- `{row}` (`integer`) Target row, 1-indexed.

**Example:**

```lua
win:set_cursor(3) -- highlight the third line
```

---

### `Win:close()` {#Win-close}

```lua
Win:close()
```

Closes the window and frees its resources. Safe to call more than
once. The window also closes automatically when the handle is
garbage collected.

**Example:**

```lua
win:close()
```

---

### `Win:is_open()` {#Win-is_open}

```lua
Win:is_open()
```

Returns true if the window is still alive (not closed). Useful for
checking before sending commands.

**Returns:** (`boolean`) true if open.

**Example:**

```lua
if win:is_open() then
  win:set_config({ title = "still here" })
end
```

---

### `Win:show()` {#Win-show}

```lua
Win:show()
```

Makes the window visible again after it was hidden with `hide()`.

**Example:**

```lua
win:show()
```

---

### `Win:hide()` {#Win-hide}

```lua
Win:hide()
```

Hides the window without closing it. The window keeps its state
and buffer contents. Call `show()` to bring it back.

**Example:**

```lua
win:hide()
-- do some work...
win:show()
```

---

### `Win:is_visible()` {#Win-is_visible}

```lua
Win:is_visible()
```

Returns true if the window is both open and visible (not hidden).

**Returns:** (`boolean`) true if visible.


## noon.ui.Buf {#noon-ui-Buf}

A content buffer that holds styled lines of text. Create one with
`noon.ui.buf()` and pass it to `noon.ui.open_win()` to show it in
a floating or split window.

```lua
local buf = noon.ui.buf()
buf:line("hello")
buf:line({ { "world", "bold" } })
```

---

### `Buf:line()` {#Buf-line}

```lua
Buf:line({line})
```

Appends a single line to the end of the buffer. You can pass a
plain string for unstyled text, or a table of `{text, style?}` spans
for rich content. Style can be a named string like "bold" or
"keyword", or an inline table `{fg?, bg?, bold?, italic?, underline?, dim?, strikethrough?, reversed?}`
with "#rrggbb" color strings.

**Parameters:**

- `{line}` (`string|table`) Plain string, or a sequence of spans: `{ {text, style?}, ... }`.

**Example:**

```lua
buf:line("plain text")
buf:line({ { "ERROR", { fg = "#ff0000", bold = true } }, { " something broke" } })
```

---

### `Buf:lines()` {#Buf-lines}

```lua
Buf:lines({lines})
```

Appends several lines at once. Each entry uses the same format as
`buf:line()`, so you can mix plain strings and styled spans.

**Parameters:**

- `{lines}` (`table`) Sequence of line values, each the same format accepted by `buf:line`.

**Example:**

```lua
buf:lines({
  "first line",
  { { "styled ", "bold" }, { "second line" } },
  "third line",
})
```

---

### `Buf:set_lines()` {#Buf-set_lines}

```lua
Buf:set_lines({lines})
```

Replaces every line in the buffer with {lines}. Use this when you
want to redraw the whole buffer, for example after the user toggles
a view.

**Parameters:**

- `{lines}` (`table`) Sequence of line values, each the same format accepted by `buf:line`.

**Example:**

```lua
buf:set_lines({ "new content", "replaces everything" })
```

---

### `Buf:len()` {#Buf-len}

```lua
Buf:len()
```

Returns how many lines the buffer currently holds.

**Returns:** (`integer`) Line count.

**Example:**

```lua
if buf:len() == 0 then
  buf:line("(empty)")
end
```

---

### `Buf:get_lines()` {#Buf-get_lines}

```lua
Buf:get_lines()
```

Returns all lines in the buffer as a Lua table. Each line is a
sequence of `{text, style?}` spans, the same format `buf:line()`
accepts. Useful for reading back content, copying it to another
buffer, or round-tripping through `set_lines()`.

**Returns:** (`table`) Sequence of lines.

**Example:**

```lua
local lines = buf:get_lines()
buf:set_lines(lines) -- round-trip
```

---

### `Buf:on()` {#Buf-on}

```lua
Buf:on({event}, {callback})
```

Registers an event handler on the buffer.

Supported events:
- "click": fires when the user clicks a line. The handler receives
  a click-event table and may yield or mutate the buffer.
- "change": fires synchronously after every mutation (`line`,
  `lines`, `set_lines`). Must not yield.

Calling `on()` again for the same event replaces the previous handler.

**Parameters:**

- `{event}` (`string`) Event name: "click" or "change".
- `{callback}` (`function`) Handler function. For "click", receives a click-event table. For "change", receives no arguments.

**Example:**

```lua
buf:on("click", function(ev)
  noon.ui.flash("Clicked row " .. ev.row)
end)
```

---

### `Buf:click()` {#Buf-click}

```lua
Buf:click({ev})
```

Programmatically fires the buffer's click handler with event {ev}.
Does nothing if no click handler is registered. Useful for testing
or simulating user interaction from code.

**Parameters:**

- `{ev}` (`table`) Click event table passed to the handler.

**Example:**

```lua
buf:click({ row = 1 })
```

---

### `Buf:blit()` {#Buf-blit}

```lua
Buf:blit({fb}, {width}, {height}, {opts?})
```

Replaces the whole buffer with a pixel frame drawn as `"▀"` cells.
Each cell's foreground is the top pixel and its background the
bottom one, so one text line fits two pixel rows. When {height} is
odd the last line leaves its background unset and the terminal
default shows through.

{fb} is a Luau `buffer` of raw pixel bytes in row-major order,
top-left origin. Its size must be exactly
`width * height * bytes_per_pixel` for the chosen format, otherwise
the call throws. A mismatch usually means a wrong width or format,
and an early error beats hunting down a garbled frame.

Formats: "rgb" is the default at 3 bytes per pixel. "rgba" and
"bgra" take 4 bytes per pixel and ignore the 4th byte. "bgra" is
what a little-endian `uint32` holding `0xRRGGBB` looks like in
memory, the layout doomgeneric uses for its framebuffer.

`char` swaps the `"▀"` glyph for another one column wide string,
e.g. `"█"` when only the foreground color should show. The
foreground still comes from the top pixel and the background from
the bottom one, whatever the glyph.

**Parameters:**

- `{fb}` (`buffer`) Raw pixel bytes.
- `{width}` (`integer`) Frame width in pixels, > 0.
- `{height}` (`integer`) Frame height in pixels, > 0.
- `{opts?}` (`table|nil`) Options: `format` = "rgb"|"rgba"|"bgra", `char` = one column wide string.

**Example:**

```lua
local fb = buffer.create(160 * 100 * 3)
buffer.writeu8(fb, (y * 160 + x) * 3, 255) -- red channel
buf:blit(fb, 160, 100)
buf:blit(fb32, 160, 100, { format = "bgra", char = "█" })
```


## noon.uv {#noon-uv}

System and environment utilities, modelled after `vim.uv`.

Provides access to the working directory, home directory, and environment
variables. None of these functions throw.

```lua
local home = noon.uv.os_homedir()
```

---

### `noon.uv.cwd()` {#noon-uv-cwd}

```lua
noon.uv.cwd()
```

Return the current working directory as an absolute path. Like `vim.uv.cwd`.

**Returns:** (`string?`) Current working directory, or nil if it cannot be determined.

**Example:**

```lua
local cwd = noon.uv.cwd()
if cwd then print("working in: " .. cwd) end
```

---

### `noon.uv.os_homedir()` {#noon-uv-os_homedir}

```lua
noon.uv.os_homedir()
```

Return the current user's home directory. Like `vim.uv.os_homedir`.

**Returns:** (`string?`) Home directory path, or nil if it cannot be determined.

**Example:**

```lua
local home = noon.uv.os_homedir() -- e.g. "/home/user"
```

---

### `noon.uv.os_getenv()` {#noon-uv-os_getenv}

```lua
noon.uv.os_getenv({name})
```

Look up the environment variable {name}. Like `vim.uv.os_getenv`.
Returns nil when the variable is not set.

**Parameters:**

- `{name}` (`string`) Name of the environment variable.

**Returns:** (`string?`) Variable value, or nil if not set.

**Example:**

```lua
local editor = noon.uv.os_getenv("EDITOR") or "vi"
```


## noon.yaml {#noon-yaml}

YAML encoding and decoding. Works the same way as `noon.json`,
but for YAML formatted strings.

```lua
local t = noon.yaml.decode("greeting: hello")
print(t.greeting)
```

---

### `noon.yaml.encode()` {#noon-yaml-encode}

```lua
noon.yaml.encode({value})
```

Turn a Lua value into a YAML string. Most Lua types work, but
circular references will return an error.

**Parameters:**

- `{value}` (`any`) Lua value to encode.

**Returns:** (`string?`, `string?`) YAML string, or nil plus an error.

**Example:**

```lua
local s, err = noon.yaml.encode({ name = "noon", tags = { "ai", "agent" } })
print(s)
```

---

### `noon.yaml.decode()` {#noon-yaml-decode}

```lua
noon.yaml.decode({str})
```

Parse a YAML string into a Lua value. Mappings become tables and
sequences become 1-indexed arrays.

**Parameters:**

- `{str}` (`string`) YAML string to decode.

**Returns:** (`any?`, `string?`) Decoded value, or nil plus an error.

**Example:**

```lua
local t, err = noon.yaml.decode("name: noon\nversion: 1")
print(t.name) -- noon
```


## Shared helper modules

These ship inside noon; `require` them from any plugin. Small modules are
shown as full source, larger ones as their public interface.

### `require("noon.color")`

```lua
local M = {}

function M.lerp(from, to, t)
  local fr, fg, fb = from:match("#(%x%x)(%x%x)(%x%x)")
  local tr, tg, tb = to:match("#(%x%x)(%x%x)(%x%x)")
  if not fr or not tr then
    return from
  end
  fr, fg, fb = tonumber(fr, 16), tonumber(fg, 16), tonumber(fb, 16)
  tr, tg, tb = tonumber(tr, 16), tonumber(tg, 16), tonumber(tb, 16)
  local r = math.floor(fr + (tr - fr) * t + 0.5)
  local g = math.floor(fg + (tg - fg) * t + 0.5)
  local b = math.floor(fb + (tb - fb) * t + 0.5)
  return string.format("#%02x%02x%02x", r, g, b)
end

function M.dim(color, factor)
  local bg = noon.ui.theme_color("background") or "#000000"
  return M.lerp(color, bg, factor)
end

return M
```

### `require("noon.fuzzy_replace")`

```lua
M.NO_MATCH = "old_string not found in file"
M.MULTIPLE_MATCHES = "old_string matches multiple locations; add surrounding context to make it unique"
M.EMPTY_OLD_STRING = "old_string must not be empty"

-- Replace {old_string} with {new_string} in {content}, tolerating small
-- whitespace and indentation drift. Returns the new content, or nil plus
-- one of the error constants above.
function M.replace(content, old_string, new_string, replace_all)
```

### `require("noon.list_picker")`

```lua
-- Open a fuzzy-filter picker in a floating window and block until the user
-- decides. {items} is a list of strings or { label, detail? } tables. {opts}:
-- title, footer, cursor (initial index), submit_keys (extra submit keys
-- besides enter). Returns { type = "choice"|"delete", index } or
-- { type = "close" }.
function ListPicker.open(items, opts)
ListPicker.split_words = split_words
ListPicker.matches = matches
ListPicker.highlight_spans = highlight_spans
```

### `require("noon.output_limits")`

```lua
-- Shared per-tool output limit options, so the tools that support them
-- cannot drift apart.

local DEFAULT_MAX_OUTPUT_LINES = 2000
local DEFAULT_MAX_OUTPUT_BYTES = 50 * 1024

local M = {}

M.specs = {
  max_output_lines = { type = "integer", desc = "Override `agent.max_output_lines` for this tool." },
  max_output_bytes = { type = "integer", desc = "Override `agent.max_output_bytes` for this tool." },
}

function M.extend(spec)
  for name, s in pairs(M.specs) do
    spec[name] = s
  end
  return spec
end

--- Returns max_lines, max_bytes: tool override when set, agent-wide otherwise.
function M.resolve(opts, ctx)
  return opts.max_output_lines or ctx:config("max_output_lines", DEFAULT_MAX_OUTPUT_LINES),
    opts.max_output_bytes or ctx:config("max_output_bytes", DEFAULT_MAX_OUTPUT_BYTES)
end

return M
```

### `require("noon.shorten_path")`

```lua
local function normalize_sep(s)
  return s:gsub("\\", "/")
end

local function shorten_path(path)
  local p = normalize_sep(path)
  local cwd = noon.uv.cwd()
  if cwd then
    cwd = normalize_sep(cwd)
    if p:sub(1, #cwd + 1) == cwd .. "/" then
      local rel = p:sub(#cwd + 2)
      return rel == "" and "." or rel
    end
  end
  local home = noon.uv.os_homedir()
  if home then
    home = normalize_sep(home)
    if p:sub(1, #home + 1) == home .. "/" then
      local rel = p:sub(#home + 2)
      return rel == "" and "~" or "~/" .. rel
    end
  end
  return path
end

return shorten_path
```

### `require("noon.text_input")`

```lua
-- TextInput: multi-line editable buffer with a byte-offset cursor.
--
-- Invariants enforced everywhere:
--   * `line` is 1-based and indexes a line that always exists.
--   * `col` is a byte offset inside `lines[line]`, always on a UTF-8 codepoint
--     boundary, so `lines[line]:sub(1, col)` is a complete UTF-8 prefix.
--   * No line ever contains a literal newline; newlines split into rows.
--
-- Parents OWN their keys. `handle_key` returns one of R.IGNORED / R.MOVED /
-- R.CHANGED. Parent dispatchers must filter their own keys (esc, ctrl+c,
-- submit keys, etc.) BEFORE forwarding, because `handle_key` claims any key
-- it can interpret. `ctrl+a` is bound to move-home; if a parent wants it for
-- "select all" it must intercept first.
--
-- IGNORED is returned when the buffer literally cannot act (backspace at
-- (1, 0), right at end of buffer, etc.). Parents can use that signal to fall
-- through to their own logic.
--
-- Parity cases live in plugins/lib/tests/spec.lua (TRACE_CASES). Add one
-- whenever you change handle_key semantics.
TextInput.Result = R
function TextInput.new()
function TextInput:value()
function TextInput:is_empty()
function TextInput:line_count()
function TextInput:clear()

-- Returns the codepoint right before the cursor as a string, or nil at the
-- start of a line. Lets callers peek backwards (e.g. "is the previous char
-- a backslash?") without touching internal indices.
function TextInput:char_before_cursor()
function TextInput:insert_text(text)
function TextInput:insert_char(c)
function TextInput:insert_space()
function TextInput:split_line()
function TextInput:remove_char()
function TextInput:delete_char()
function TextInput:remove_word_before()
function TextInput:delete_word_after()
function TextInput:kill_to_end_of_line()
function TextInput:move_left()
function TextInput:move_right()
function TextInput:move_up()
function TextInput:move_down()
function TextInput:move_home()
function TextInput:move_end()
function TextInput:move_word_left()
function TextInput:move_word_right()
function TextInput:handle_key(key)

-- Wrap lines to {width} with {prefix} before the first row. Returns
-- { lines = styled lines, cursor_row = 1-based row holding the cursor }.
function TextInput:render(prefix, prefix_width, width)
```

### `require("noon.tool_view")`

```lua
-- The shared truncate/expand body that tool plugins render through.
--
-- Click handlers get `ev.row`, a 1-based line in this buf; 0 means the
-- click landed outside it (the header). The handler lives on the buf
-- itself, so any wrapper of the same buf (a batch child's foreign handle)
-- reaches the same toggle. Expansion is never stored: the UI records
-- clicked rows and replays them through `restore` in order, so `toggle`
-- stays a pure flag flip + re-render, deterministic across replays.
-- Async highlighting goes through `noon.async.run`; during restore the
-- runtime runs those tasks inline before snapshotting.

-- opts: max_lines (default 80) shown while collapsed, keep "head"|"tail"
-- (default "tail"), max_expand_lines (default 2000) kept for expansion.
function ToolView.new(buf, opts)
function ToolView:set_header(lines)
function ToolView:clear()
function ToolView:append(line)
function ToolView:append_text(text)

-- Append {content} with line numbers, then syntax-highlight it for {ext}
-- asynchronously. Returns false when {content} is empty.
function ToolView:set_highlight(content, ext)
function ToolView:toggle()
function ToolView:flush()
function ToolView:update_line(all_idx, line)

-- Call once after the last append so the collapsed notice renders.
function ToolView:finish()
function ToolView.restore_lines(lines, opts)

-- Rebuild a collapsed view from a tool's saved llm_output, click-to-toggle
-- wired. For `restore` hooks.
function ToolView.restore(output, opts)
```

### `require("noon.truncate")`

```lua
local function truncate(text, max_lines, max_bytes)
  if #text <= max_bytes then
    local n = 0
    for _ in text:gmatch("\n") do
      n = n + 1
    end
    if n + 1 <= max_lines then
      return text
    end
  end
  local out = {}
  local bytes = 0
  local lines = 0
  for line in text:gmatch("([^\n]*)\n?") do
    lines = lines + 1
    if lines > max_lines then
      break
    end
    local new_bytes = bytes + #line + 1
    if new_bytes > max_bytes then
      break
    end
    out[#out + 1] = line
    bytes = new_bytes
  end
  local result = table.concat(out, "\n")
  if #result < #text then
    result = result .. "\n\n[truncated " .. (#text - #result) .. " bytes]"
  end
  return result
end

return truncate
```

