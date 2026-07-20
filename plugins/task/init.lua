-- Structured-output story: the subagent gets a session-local structured_output
-- tool whose handler validates and captures the result as closure upvalues.
-- Invalid input is an inline tool error the model can fix in the same run.
-- This plugin owns structured output and subagent concurrency; Rust exposes
-- primitives only (`n00n.agent.session`, `n00n.json.schema_validator`,
-- `n00n.async.semaphore`).

local ToolView = require("n00n.tool_view")
local route_tier = require("n00n.route_tier").route_tier

local STRUCTURED_OUTPUT_NAME = "structured_output"
local STRUCTURED_OUTPUT_DESCRIPTION = "Report your final result. Call it exactly once when your task is complete."
local STRUCTURED_OUTPUT_ACK = "Output recorded."
local STRUCTURED_OUTPUT_PROMPT_SUFFIX = "\n\nWhen finished, call the structured_output tool with your final result."
local MAX_SCHEMA_ERRORS = 3
local MAX_STRUCTURED_RETRIES = 2
local SCHEMA_ROOT_ERROR = "output_schema must have type object"
local SCHEMA_COMPILE_ERROR = "invalid output_schema"
local STRUCTURED_MISSING_ERROR = "subagent finished without calling structured_output"
local STRUCTURED_INVALID_ERROR = "subagent result does not match output_schema"
local INVALID_INPUT_PREFIX =
  "Input does not match the required schema. Fix the errors and call structured_output again:\n"
local NUDGE_MISSING =
  "You did not call the structured_output tool. Call it now with your final result matching its input schema."
local BODY_INDENT_COLS = 4
local MIN_MD_WIDTH = 20
local DEFAULT_OUTPUT_LINES = 5
local DEFAULT_MAX_LINE_BYTES = 500

local description = [[Launch an autonomous subagent to perform tasks independently. Best combined with batch.

Subagent types (set via `subagent_type`):
- `research` (default): Read-only tools. For codebase exploration or gathering context.
- `general`: Full tool access. For delegating implementation work.

Notes:
1. Launch multiple tasks concurrently when possible.
2. The agent's result is not visible to the user. Summarize it in your response.
3. Each invocation starts fresh - inline any needed context into the prompt.
4. Tell it to return concise summaries with file:line refs, not full file contents.
5. With `auto_tier`, the model tier is picked from the prompt (cheap work -> weak,
   hard work -> strong). This is opt-in and off by default.
6. Set background=true to start a non-blocking agent and receive an agent_id.
   Use agent_control to inspect, steer, or stop it.
]]

local schema = {
  type = "object",
  required = { "description", "prompt" },
  additionalProperties = false,
  properties = {
    description = {
      type = "string",
      description = "Short (3-5 words) description of the task",
    },
    prompt = {
      type = "string",
      description = "Detailed task prompt for the agent",
    },
    subagent_type = {
      type = "string",
      description = 'Subagent type: "research" (read-only, default) or "general" (can modify files)',
    },
    model = {
      type = "string",
      description = "Exact model spec (optional, e.g. openai/gpt-5.6-luna). Overrides model_tier.",
    },
    model_tier = {
      type = "string",
      description = 'Model tier (optional, omit to use current model, capped at current tier):\n- "strong" (e.g. Opus): Deep reasoning, complex architecture, subtle bugs, most critical sections. ~5x cost of medium.\n- "medium" (e.g. Sonnet): Balanced. Refactors, features, multi-file changes.\n- "weak" (e.g. Haiku): Fast/cheap. Search, summarize, boilerplate, simple edits.',
    },
    thinking = {
      type = { "string", "integer" },
      description = 'Thinking mode: "off", "adaptive", "minimal", "low", "medium", "high", "xhigh", "max", or a token budget. Omit to inherit the user setting.',
    },
    auto_tier = {
      type = "boolean",
      description = "Pick model_tier from the prompt automatically (opt-in). Overrides model_tier when set.",
    },
    background = {
      type = "boolean",
      description = "Start this task in a separate background session and return its agent_id immediately.",
    },
    output_schema = {
      description = "JSON Schema (object) the subagent's final result must match. When set, the result is returned as a validated JSON string.",
    },
  },
}

local examples = {
  {
    description = "Find auth middleware",
    prompt = "Search the codebase for authentication middleware. Return file paths and a summary of how auth is implemented.",
    model_tier = "weak",
  },
}

local opts = n00n.api.register_options({
  max_concurrent = { default = 8, min = 1, desc = "Max concurrently running subagents." },
  auto_tier = { default = false, desc = "Route each subagent's model tier from its prompt (opt-in, off by default)." },
})

-- Process-wide cap on concurrent subagents.
local semaphore = n00n.async.semaphore(opts.max_concurrent)

local function bounded_errors(errors)
  local out = {}
  for i = 1, math.min(#errors, MAX_SCHEMA_ERRORS) do
    out[i] = errors[i]
  end
  return table.concat(out, "\n")
end

local function make_preview(ctx, description)
  local tol = ctx:tool_output_lines()
  local max_preview = (tol and tol.task) or DEFAULT_OUTPUT_LINES
  local view = ToolView.new(n00n.ui.buf(), { max_lines = max_preview, keep = "tail" })
  local last_completed = 0

  local function update(progress)
    if progress.completed_count > last_completed then
      local new_count = progress.completed_count - last_completed
      local recent = progress.recent_tools
      local start = new_count <= #recent and (#recent - new_count + 1) or 1
      for i = start, #recent do
        view:append({ { "✓ " .. recent[i], "dim" } })
      end
      last_completed = progress.completed_count
    end

    local elapsed = math.floor(progress.elapsed_ms / 1000)
    local elapsed_str = n00n.ui.humantime(elapsed)
    local header = { { description .. " · " .. elapsed_str, "bold" } }
    if progress.current_tool then
      header[#header + 1] = { { "▸ " .. progress.current_tool, "bold" } }
    elseif not progress.done then
      header[#header + 1] = { { "Starting...", "dim" } }
    end
    view:set_header(header)
  end

  view.buf:on("click", function()
    view:toggle()
  end)

  return { buf = view.buf, update = update }
end

local function handler(input, ctx)
  if input.background then
    local forwarded = {}
    for key, value in pairs(input) do
      forwarded[key] = value
    end
    forwarded.background = false
    local prompt = "Use the task tool now with background=false. Do not only describe this request.\n\n"
      .. n00n.json.encode(forwarded)
    local id, err = n00n.session.new({ prompt = prompt, focus = false })
    if not id then
      return { llm_output = err, is_error = true }
    end
    return n00n.json.encode({ agent_id = id, status = "started" })
  end

  local subagent_type = input.subagent_type or "research"
  if subagent_type ~= "research" and subagent_type ~= "general" then
    return { llm_output = "unknown subagent type: " .. subagent_type, is_error = true }
  end

  -- Compile early: a bad schema costs zero tokens.
  local validator
  if input.output_schema then
    if type(input.output_schema) ~= "table" or input.output_schema.type ~= "object" then
      return { llm_output = SCHEMA_ROOT_ERROR, is_error = true }
    end
    local compile_err
    validator, compile_err = n00n.json.schema_validator(input.output_schema)
    if compile_err then
      return { llm_output = SCHEMA_COMPILE_ERROR .. ": " .. compile_err, is_error = true }
    end
  end

  local model_tier = input.model_tier
  if not input.model and (input.auto_tier == true or (input.auto_tier == nil and opts.auto_tier)) then
    model_tier = route_tier(input.prompt)
  end

  local model, model_err = n00n.agent.resolve_model(ctx, {
    spec = input.model,
    tier = not input.model and model_tier or nil,
  })
  if model_err then
    return { llm_output = model_err, is_error = true }
  end

  local audience = subagent_type == "research" and "research_sub" or "general_sub"
  local prompt_id = subagent_type == "research" and "research" or "general"
  local system, system_err = n00n.agent.system_prompt(ctx, {
    prompt_id = prompt_id,
    instructions = true,
  })
  if system_err then
    return { llm_output = system_err, is_error = true }
  end

  local tool_defs, tools_err = n00n.agent.tools(ctx, {
    audience = audience,
    spec = model.spec,
    include_mcp = true,
  })
  if tools_err then
    return { llm_output = tools_err, is_error = true }
  end

  local captured, last_errors
  local local_tools
  if validator then
    local_tools = {
      [STRUCTURED_OUTPUT_NAME] = {
        description = STRUCTURED_OUTPUT_DESCRIPTION,
        input_schema = input.output_schema,
        handler = function(value)
          local errs = validator:validate(value)
          if errs then
            last_errors = bounded_errors(errs)
            return nil, INVALID_INPUT_PREFIX .. last_errors
          end
          captured = value
          return STRUCTURED_OUTPUT_ACK
        end,
      },
    }
  end

  local preview = make_preview(ctx, input.description or "task")

  n00n.async.run(function()
    local permit = semaphore:acquire()
    local ok, out = pcall(function()
      local sess, sess_err = n00n.agent.session(ctx, {
        model_spec = model.spec,
        system = system,
        tools = tool_defs,
        local_tools = local_tools,
        audience = audience,
        name = input.description,
        thinking = input.thinking,
      })
      if sess_err then
        return { llm_output = sess_err, is_error = true }
      end

      local function do_prompt()
        local message = input.prompt
        if validator then
          message = message .. STRUCTURED_OUTPUT_PROMPT_SUFFIX
        end
        local result, err = sess:prompt(message)
        local retries = 0
        while not err and validator and not captured and retries < MAX_STRUCTURED_RETRIES do
          retries = retries + 1
          result, err = sess:prompt(NUDGE_MISSING)
        end
        if err then
          return { llm_output = "sub-agent error: " .. err, is_error = true }
        end
        if validator and not captured then
          local msg = last_errors and (STRUCTURED_INVALID_ERROR .. ":\n" .. last_errors) or STRUCTURED_MISSING_ERROR
          return { llm_output = msg, is_error = true }
        end
        return { llm_output = captured and n00n.json.encode(captured) or result.text, format = "markdown" }
      end

      local function do_poll()
        while true do
          local progress, err = sess:get_progress()
          if not progress then
            return
          end
          preview:update(progress)
          if progress.done then
            return
          end
        end
      end

      local results = n00n.async.gather({ do_prompt, do_poll })
      sess:close()
      local prompt_res = results[1]
      if not prompt_res.ok then
        error(prompt_res.err, 0)
      end
      return prompt_res.value
    end)
    permit:release()
    if not ok then
      ctx:finish({ llm_output = "task failed: " .. tostring(out), is_error = true, body = preview.buf })
      return
    end
    ctx:finish({
      llm_output = out.llm_output,
      body = preview.buf,
      is_error = out.is_error,
      format = out.format,
    })
  end)

  return nil
end

local function header(input)
  return input.description
end

-- Standalone runs render markdown on the Rust side (format = "markdown");
-- this mirrors that for restore and batch children, which build the body here.
local function restore(_input, output, is_error, ctx)
  local tol = ctx:tool_output_lines()
  local opts = {
    max_lines = (tol and tol.task) or DEFAULT_OUTPUT_LINES,
    keep = "head",
    max_line_bytes = DEFAULT_MAX_LINE_BYTES,
  }
  if not is_error then
    local width = math.max(n00n.ui.terminal_size().cols - BODY_INDENT_COLS, MIN_MD_WIDTH)
    local ok, md_lines = pcall(n00n.ui.markdown, output, width)
    if ok then
      return ToolView.restore_lines(md_lines, opts)
    end
  end
  return ToolView.restore(output, opts)
end

n00n.api.register_tool({
  name = "task",
  description = description,
  kind = "execute",
  audiences = { "main", "workflow" },
  examples = examples,
  schema = schema,
  handler = handler,
  header = header,
  restore = restore,
})
