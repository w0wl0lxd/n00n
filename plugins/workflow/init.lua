-- Script-runtime workflow orchestrator.
--
-- A workflow moves the plan into code: the script holds the loop, branching,
-- and intermediate results, so the caller's context holds only the final
-- answer. The script itself consumes zero tokens; only agent() calls cost
-- tokens. Mirrors Claude Code dynamic workflows, but in Lua on noon's
-- existing primitives (noon.agent.session / noon.async.*), so no new Rust.
--
-- Policy lives here; Rust exposes primitives only (same split as the task
-- plugin). The script runs sandboxed: it sees only the injected globals
-- (meta, agent, parallel, pipeline, phase, log, inputs, plus a whitelisted
-- string/table/math), never noon, os, io, require, or print. os and
-- math.random stay out so the script stays deterministic, which resume
-- depends on.

local ToolView = require("noon.tool_view")

local STRUCTURED_OUTPUT_NAME = "structured_output"
local STRUCTURED_OUTPUT_DESCRIPTION = "Report your final result. Call it exactly once when your task is complete."
local STRUCTURED_OUTPUT_ACK = "Output recorded."
local STRUCTURED_OUTPUT_SUFFIX = "\n\nWhen finished, call the structured_output tool with your final result."
local MAX_STRUCTURED_RETRIES = 2
local NUDGE_MISSING =
  "You did not call the structured_output tool. Call it now with your final result matching its input schema."
local MAX_SCHEMA_ERRORS = 3
local SCHEMA_ROOT_ERROR = "output_schema must have type object"
local SCHEMA_COMPILE_ERROR = "invalid output_schema"
local STRUCTURED_MISSING_ERROR = "subagent finished without calling structured_output"
local STRUCTURED_INVALID_ERROR = "subagent result does not match output_schema"
local INVALID_INPUT_PREFIX =
  "Input does not match the required schema. Fix the errors and call structured_output again:\n"
local SCRIPT_ERROR_PREFIX = "workflow script error: "
local NO_META_ERROR = "workflow script must call meta({ name = ... }) before doing any work"
local SCRIPT_REQUIRED_ERROR = "script (string) is required"
local NAME_LABEL_MAX = 40
local DEFAULT_OUTPUT_LINES = 8
local DEFAULT_MAX_LINE_BYTES = 500
local MIN_BODY_WIDTH = 20
local BODY_INDENT_COLS = 4
local GENERAL_AUDIENCE = "general_sub"
local RESEARCH_AUDIENCE = "research_sub"
local GENERAL_PROMPT = "general"
local RESEARCH_PROMPT = "research"

local description = [[Run a workflow script that orchestrates many subagents at scale.

A workflow moves the plan into code: the script holds the loop, branching, and intermediate results, so your context holds only the final answer. The script itself consumes zero tokens; only agent() calls cost tokens. Use it for codebase-wide audits, large migrations, cross-checked research, or any task needing more agents than one conversation can coordinate.

The script is Lua. Its first statement declares metadata:

  meta({ name = "audit", description = "Review changed files, then verify", phases = { { title = "Review" }, { title = "Verify" } } })

The runtime injects these globals into the script (nothing else is available - no noon, os, io, require, print, or load):

- agent(opts) -> string | validated JSON string. Spawns an isolated subagent and returns its final result. opts: { prompt (required), subagent_type? ("general" default, or "research" for read-only), model_tier? ("strong"/"medium"/"weak", capped at your model), label?, output_schema? (JSON Schema; result returned as a validated JSON string) }
- parallel(fns, opts?) -> array. Runs zero-arg fns concurrently, returns results in input order. opts: { concurrency = 8 }
- pipeline(stages, initial) -> value. Runs stages in order, threading each stage's output into the next, returns the last stage's output.
- phase(name, fn) -> fn's result. Labels work for the progress UI.
- log(...) -> records a message in the workflow log.

Reach for a workflow when a task has multiple stages that feed into each other, some stages can run in parallel, or you want reproducible orchestration. For a single subagent doing one thing, use the task tool instead.

The script must be deterministic: do not rely on wall-clock time or randomness, so a resumed run reproduces the same orchestration path.]]

local schema = {
  type = "object",
  required = { "script" },
  additionalProperties = false,
  properties = {
    script = {
      type = "string",
      description = "Lua workflow script. First statement: meta({...}). Then orchestrate with agent/parallel/pipeline/phase/log. Must return the final answer as a string.",
    },
    inputs = {
      description = "Free-form object exposed to the script as the global `inputs`.",
    },
  },
}

local examples = {
  {
    description = "Review two files in parallel, then verify each finding",
    script = [[meta({ name = "audit", description = "Review then verify", phases = { { title = "Review" }, { title = "Verify" } } })
local reviews = phase("Review", function()
  return parallel({
    function() return agent({ prompt = "Review src/a.rs for real bugs.", output_schema = { type = "object", properties = { findings = { type = "array", items = { type = "string" } } }, required = { "findings" } } }) end,
    function() return agent({ prompt = "Review src/b.rs for real bugs.", output_schema = { type = "object", properties = { findings = { type = "array", items = { type = "string" } } }, required = { "findings" } } }) end,
  })
end)
local verified = phase("Verify", function()
  return parallel({
    function() return agent({ prompt = "Confirm or reject each finding: " .. reviews[1], subagent_type = "research" }) end,
    function() return agent({ prompt = "Confirm or reject each finding: " .. reviews[2], subagent_type = "research" }) end,
  })
end)
return "Reviews:\n" .. table.concat(reviews, "\n---\n") .. "\n\nVerified:\n" .. table.concat(verified, "\n---\n")]],
  },
}

local opts = noon.api.register_options({
  max_concurrent_agents = { default = 8, min = 1, desc = "Max subagents one parallel() call runs at once." },
  max_concurrent_workflows = { default = 4, min = 1, desc = "Max concurrently running workflows." },
})

-- Process-wide cap on concurrent workflows, separate from the per-parallel
-- agent cap inside parallel().
local workflow_semaphore = noon.async.semaphore(opts.max_concurrent_workflows)

local function bounded_errors(errors)
  local out = {}
  for i = 1, math.min(#errors, MAX_SCHEMA_ERRORS) do
    out[i] = errors[i]
  end
  return table.concat(out, "\n")
end

local function parallel(fns, popts)
  if type(fns) ~= "table" then
    error("parallel: fns must be an array of functions", 0)
  end
  popts = popts or {}
  local concurrency = opts.max_concurrent_agents
  if type(popts.concurrency) == "number" then
    concurrency = math.max(1, math.min(popts.concurrency, opts.max_concurrent_agents))
  end
  local sem = noon.async.semaphore(concurrency)
  local wrapped = {}
  for i, f in ipairs(fns) do
    if type(f) ~= "function" then
      error("parallel: fns[" .. i .. "] must be a function", 0)
    end
    wrapped[i] = function()
      local permit = sem:acquire()
      local ok, result = pcall(f)
      permit:release()
      if not ok then
        error(result, 0)
      end
      return result
    end
  end
  local results = noon.async.gather(wrapped)
  local out = {}
  for i, r in ipairs(results) do
    if not r.ok then
      error("parallel: branch " .. i .. " failed: " .. tostring(r.err), 0)
    end
    out[i] = r.value
  end
  return out
end

local function pipeline(stages, initial)
  if type(stages) ~= "table" then
    error("pipeline: stages must be an array of functions", 0)
  end
  local value = initial
  for i, stage in ipairs(stages) do
    if type(stage) ~= "function" then
      error("pipeline: stages[" .. i .. "] must be a function", 0)
    end
    value = stage(value)
  end
  return value
end

local function make_agent(ctx, progress)
  return function(aopts)
    aopts = aopts or {}
    if type(aopts.prompt) ~= "string" then
      error("agent: opts.prompt (string) is required", 0)
    end
    if aopts.label and type(aopts.label) ~= "string" then
      error("agent: opts.label must be a string", 0)
    end
    local subagent_type = aopts.subagent_type or "general"
    if subagent_type ~= "general" and subagent_type ~= "research" then
      error("agent: unknown subagent_type: " .. tostring(subagent_type), 0)
    end
    local label = aopts.label or noon.ui.truncate_text(aopts.prompt, NAME_LABEL_MAX).head

    -- Compile before spending tokens: a bad schema costs zero tokens.
    local validator
    if aopts.output_schema then
      if type(aopts.output_schema) ~= "table" or aopts.output_schema.type ~= "object" then
        error(SCHEMA_ROOT_ERROR, 0)
      end
      local compile_err
      validator, compile_err = noon.json.schema_validator(aopts.output_schema)
      if compile_err then
        error(SCHEMA_COMPILE_ERROR .. ": " .. compile_err, 0)
      end
    end

    local model, model_err = noon.agent.resolve_model(ctx, { tier = aopts.model_tier })
    if model_err then
      error(model_err, 0)
    end

    local audience = subagent_type == "research" and RESEARCH_AUDIENCE or GENERAL_AUDIENCE
    local prompt_id = subagent_type == "research" and RESEARCH_PROMPT or GENERAL_PROMPT
    local system, system_err = noon.agent.system_prompt(ctx, { prompt_id = prompt_id, instructions = true })
    if system_err then
      error(system_err, 0)
    end

    local tool_defs, tools_err = noon.agent.tools(ctx, {
      audience = audience,
      spec = model.spec,
      include_mcp = true,
    })
    if tools_err then
      error(tools_err, 0)
    end

    local captured, last_errors
    local local_tools
    if validator then
      local_tools = {
        [STRUCTURED_OUTPUT_NAME] = {
          description = STRUCTURED_OUTPUT_DESCRIPTION,
          input_schema = aopts.output_schema,
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

    progress.agent_started(label)
    local sess, sess_err = noon.agent.session(ctx, {
      model_spec = model.spec,
      system = system,
      tools = tool_defs,
      local_tools = local_tools,
      audience = audience,
      name = label,
    })
    if sess_err then
      error(sess_err, 0)
    end

    local message = aopts.prompt
    if validator then
      message = message .. STRUCTURED_OUTPUT_SUFFIX
    end
    local result, prompt_err = sess:prompt(message)
    local retries = 0
    while not prompt_err and validator and not captured and retries < MAX_STRUCTURED_RETRIES do
      retries = retries + 1
      result, prompt_err = sess:prompt(NUDGE_MISSING)
    end
    sess:close()
    progress.agent_done(label)

    if prompt_err then
      error("sub-agent error: " .. prompt_err, 0)
    end
    if validator and not captured then
      local msg = last_errors and (STRUCTURED_INVALID_ERROR .. ":\n" .. last_errors) or STRUCTURED_MISSING_ERROR
      error(msg, 0)
    end
    if captured then
      local encoded, encode_err = noon.json.encode(captured)
      if encode_err then
        error("failed to encode structured output: " .. tostring(encode_err), 0)
      end
      return encoded
    end
    return result.text
  end
end

local function make_progress(ctx)
  local tol = ctx:tool_output_lines()
  local max_lines = (tol and tol.workflow) or DEFAULT_OUTPUT_LINES
  local view = ToolView.new(noon.ui.buf(), { max_lines = max_lines, keep = "tail" })
  local started_at = os.time()
  local state = { name = "workflow", phase = "starting", agents = 0, done = 0 }

  local function refresh_header()
    local elapsed = math.max(os.time() - started_at, 0)
    local header = {
      { state.name .. " · " .. state.phase .. " · " .. noon.ui.humantime(elapsed), "bold" },
    }
    if state.agents > 0 then
      header[#header + 1] = { string.format("agents %d/%d", state.done, state.agents), "dim" }
    end
    view:set_header(header)
  end

  view.buf:on("click", function()
    view:toggle()
  end)
  refresh_header()

  return {
    buf = view.buf,
    set_name = function(name)
      state.name = name
      refresh_header()
    end,
    set_phase = function(name)
      state.phase = name
      refresh_header()
    end,
    log = function(msg)
      view:append({ msg, "dim" })
    end,
    agent_started = function(label)
      state.agents = state.agents + 1
      refresh_header()
      view:append({ "> " .. label, "dim" })
    end,
    agent_done = function(label)
      state.done = state.done + 1
      refresh_header()
      view:append({ "+ " .. label, "dim" })
    end,
  }
end

local function build_env(ctx, progress, inputs, captured)
  local env = {
    inputs = inputs,
    agent = make_agent(ctx, progress),
    parallel = parallel,
    pipeline = pipeline,
    tostring = tostring,
    tonumber = tonumber,
    type = type,
    error = error,
    assert = assert,
    pcall = pcall,
    select = select,
    next = next,
    ipairs = ipairs,
    pairs = pairs,
    unpack = unpack,
    string = string,
    table = table,
    math = {
      floor = math.floor,
      ceil = math.ceil,
      abs = math.abs,
      max = math.max,
      min = math.min,
      huge = math.huge,
      pi = math.pi,
      fmod = math.fmod,
      modf = math.modf,
      sqrt = math.sqrt,
      log = math.log,
      exp = math.exp,
      sin = math.sin,
      cos = math.cos,
      tan = math.tan,
      tointeger = math.tointeger,
    },
  }
  env.meta = function(t)
    if captured.meta then
      error("meta() must be called exactly once", 0)
    end
    if type(t) ~= "table" or type(t.name) ~= "string" then
      error("meta({...}) requires a `name` string", 0)
    end
    captured.meta = t
    progress.set_name(t.name)
  end
  env.phase = function(name, fn)
    if type(fn) ~= "function" then
      error("phase: fn must be a function", 0)
    end
    progress.set_phase(tostring(name))
    return fn()
  end
  env.log = function(...)
    local n = select("#", ...)
    local parts = {}
    for i = 1, n do
      parts[i] = tostring(select(i, ...))
    end
    progress.log(table.concat(parts, " "))
  end
  return env
end

local function handler(input, ctx)
  if type(input.script) ~= "string" or input.script == "" then
    return { llm_output = SCRIPT_REQUIRED_ERROR, is_error = true }
  end

  -- Surface syntax errors synchronously, before spending anything.
  local syntax_fn, syntax_err = noon.workflow.compile(input.script, {})
  if not syntax_fn then
    return { llm_output = SCRIPT_ERROR_PREFIX .. tostring(syntax_err), is_error = true }
  end

  local progress = make_progress(ctx)
  local captured = {}

  local function on_finish(err, result)
    if err then
      ctx:finish({ llm_output = SCRIPT_ERROR_PREFIX .. tostring(err), is_error = true, body = progress.buf })
    else
      ctx:finish({ llm_output = result, body = progress.buf, format = "markdown" })
    end
  end

  noon.async.run(function()
    local permit
    local ok, result = pcall(function()
      permit = workflow_semaphore:acquire()
      local env = build_env(ctx, progress, input.inputs or {}, captured)
      local run_fn, load_err = noon.workflow.compile(input.script, env)
      if not run_fn then
        error(tostring(load_err), 0)
      end
      local output = run_fn()
      if not captured.meta then
        error(NO_META_ERROR, 0)
      end
      if type(output) ~= "string" then
        return tostring(output)
      end
      return output
    end)
    if permit then
      permit:release()
    end
    if not ok then
      error(result, 0)
    end
    return result
  end, on_finish)

  return nil
end

local function header(input)
  if type(input.script) == "string" then
    local name = input.script:match('meta%s*%(%s*[%s%S]-name%s*=%s*"([^"]+)"')
      or input.script:match("meta%s*%(%s*[%s%S]-name%s*=%s*'([^']+)'")
    if name then
      return name
    end
  end
  return "workflow"
end

local function restore(_input, output, is_error, ctx)
  local tol = ctx:tool_output_lines()
  local restore_opts = {
    max_lines = (tol and tol.workflow) or DEFAULT_OUTPUT_LINES,
    keep = "head",
    max_line_bytes = DEFAULT_MAX_LINE_BYTES,
  }
  if not is_error then
    local width = math.max(noon.ui.terminal_size().cols - BODY_INDENT_COLS, MIN_BODY_WIDTH)
    local ok, md_lines = pcall(noon.ui.markdown, output, width)
    if ok then
      return ToolView.restore_lines(md_lines, restore_opts)
    end
  end
  return ToolView.restore(output, restore_opts)
end

noon.api.register_tool({
  name = "workflow",
  description = description,
  kind = "execute",
  audiences = { "main" },
  examples = examples,
  schema = schema,
  handler = handler,
  header = header,
  restore = restore,
})
