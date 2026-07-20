-- Script-runtime workflow orchestrator.
--
-- A workflow moves the plan into code: the script holds the loop, branching,
-- and intermediate results, so the caller's context holds only the final
-- answer. The script itself consumes zero tokens; only agent() calls cost
-- tokens. Mirrors Claude Code dynamic workflows, but in Lua on n00n's
-- existing primitives (n00n.agent.session / n00n.async.*), so no new JS runtime.
--
-- Policy lives here; Rust exposes primitives only (same split as the task
-- plugin). The script runs sandboxed: it sees only the injected globals
-- (meta, agent, parallel, pipeline, phase, log, inputs, plus a whitelisted
-- string/table/math), never n00n, os, io, require, or print. os and
-- math.random stay out so the script stays deterministic, which resume
-- depends on.
--
-- Resume: every agent() result is journaled under state_dir/workflows/{run_id}.
-- Re-running the same script with resume = run_id replays journal hits and
-- only re-spends tokens on uncached agent() calls.

local ToolView = require("n00n.tool_view")

local STRUCTURED_OUTPUT_NAME = "structured_output"
local STRUCTURED_OUTPUT_DESCRIPTION = "Report your final result. Call it exactly once when your task is complete."
local STRUCTURED_OUTPUT_ACK = "Output recorded."
local STRUCTURED_OUTPUT_SUFFIX = "\n\nWhen finished, call the structured_output tool with your final result."
local MAX_STRUCTURED_RETRIES = 2
local MAX_SCHEMA_ERRORS = 3
local SCHEMA_ROOT_ERROR = "output_schema must have type object"
local SCHEMA_COMPILE_ERROR = "invalid output_schema"
local STRUCTURED_MISSING_ERROR = "subagent finished without calling structured_output"
local STRUCTURED_INVALID_ERROR = "subagent result does not match output_schema"
local NUDGE_MISSING =
  "You did not call the structured_output tool. Call it now with your final result matching its input schema."
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
local JOURNAL_DIRNAME = "workflows"
local JOURNAL_FILENAME = "journal.jsonl"
local META_FILENAME = "meta.json"
local MAX_AGENTS_PER_RUN = 1000
local AGENT_LIMIT_ERROR = "workflow exceeded max agent() calls (" .. MAX_AGENTS_PER_RUN .. ")"
local INVALID_RUN_ID_ERROR = "resume must be a run_id (hex letters/digits only, no path separators)"
local RUN_ID_PATTERN = "^[%x]+$"
local DEFAULT_TIMEOUT_SECS = 600

local description = [[Run a workflow: a team of agents led by a supervisor using the sandboxed runtime.

A workflow moves the plan into code: the script holds the loop, branching, and intermediate results, so your context holds only the final answer. The script itself consumes zero tokens; only agent() calls cost tokens. Use it for codebase-wide audits, large migrations, cross-checked research, or any task needing more agents than one conversation can coordinate.

The script is Lua. Its first statement declares metadata:

  meta({ name = "audit", description = "Review changed files, then verify", phases = { { title = "Review" }, { title = "Verify" } } })

The runtime injects these globals into the script (nothing else is available - no n00n, os, io, require, print, or load):

- agent(opts) -> string | validated JSON string. Spawns an isolated subagent and returns its final result. opts: { prompt (required), subagent_type? ("general" default, or "research" for read-only), model_tier? ("strong"/"medium"/"weak", capped at your model), label?, output_schema? (JSON Schema; result returned as a validated JSON string) }
- parallel(fns, opts?) -> array. Runs zero-arg fns concurrently, returns results in input order. A branch failure fails the whole parallel. opts: { concurrency = 8 }
- pipeline(items, stages, opts?) -> array. Each item flows independently through every stage (no cross-item barrier between stages). Items run concurrently under the same concurrency cap as parallel.
- phase(name, fn) -> fn's result. Labels work for the progress UI.
- log(...) -> records a message in the workflow log.

Reach for a workflow when a task has multiple stages that feed into each other, some stages can run in parallel, or you want reproducible orchestration. For a single subagent doing one thing, use the task tool instead.

The script must be deterministic: do not rely on wall-clock time or randomness, so a resumed run reproduces the same orchestration path. Pass resume = <run_id> to replay journaled agent() results from a prior run.]]

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
    resume = {
      type = "string",
      description = "Prior run_id. Replays journaled agent() results and only spends tokens on new calls.",
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
  return pipeline(reviews, {
    function(review) return agent({ prompt = "Confirm or reject each finding: " .. review, subagent_type = "research" }) end,
  })
end)
return "Reviews:\n" .. table.concat(reviews, "\n---\n") .. "\n\nVerified:\n" .. table.concat(verified, "\n---\n")]],
  },
}

local opts = n00n.api.register_options({
  max_concurrent_agents = { default = 8, min = 1, desc = "Max subagents one parallel()/pipeline() call runs at once." },
  max_concurrent_workflows = { default = 4, min = 1, desc = "Max concurrently running workflows." },
  timeout_secs = {
    default = DEFAULT_TIMEOUT_SECS,
    min = 1,
    desc = "Hard deadline for one workflow run (cancels pure-Lua runaway loops via the VM watchdog).",
  },
})

local workflow_semaphore = n00n.async.semaphore(opts.max_concurrent_workflows)

local function bounded_errors(errors)
  local out = {}
  for i = 1, math.min(#errors, MAX_SCHEMA_ERRORS) do
    out[i] = errors[i]
  end
  return table.concat(out, "\n")
end

local function freeze_fns(src, names)
  local bare = {}
  for _, name in ipairs(names) do
    bare[name] = src[name]
  end
  -- Read-only proxy: scripts see the whitelist, but writes (string.format = …)
  -- cannot clobber the host tables or this copy.
  return setmetatable({}, {
    __index = bare,
    __newindex = function()
      error("workflow stdlib is read-only", 0)
    end,
    __metatable = false,
  })
end

-- Own frozen copies of stdlib so a workflow script cannot clobber host
-- string.*/table.* for every other plugin in the process.
local SAFE_STRING = freeze_fns(string, {
  "byte",
  "char",
  "find",
  "format",
  "gmatch",
  "gsub",
  "len",
  "lower",
  "match",
  "rep",
  "reverse",
  "sub",
  "upper",
  "pack",
  "packsize",
  "unpack",
})
local SAFE_TABLE = freeze_fns(table, {
  "concat",
  "insert",
  "move",
  "pack",
  "remove",
  "sort",
  "unpack",
})

local function stable_json(value)
  local t = type(value)
  if t == "nil" then
    return "null"
  elseif t == "boolean" then
    return value and "true" or "false"
  elseif t == "number" then
    return tostring(value)
  elseif t == "string" then
    local ok, enc = pcall(n00n.json.encode, value)
    return ok and enc or ('"' .. value .. '"')
  elseif t ~= "table" then
    return n00n.json.encode(tostring(value))
  end

  local n = #value
  local is_array = true
  local count = 0
  for k in pairs(value) do
    count = count + 1
    if type(k) ~= "number" or k < 1 or k > n or k % 1 ~= 0 then
      is_array = false
    end
  end
  if is_array and count == n then
    local parts = {}
    for i = 1, n do
      parts[i] = stable_json(value[i])
    end
    return "[" .. table.concat(parts, ",") .. "]"
  end

  local keys = {}
  for k in pairs(value) do
    keys[#keys + 1] = k
  end
  table.sort(keys, function(a, b)
    local ta, tb = type(a), type(b)
    if ta == tb then
      if ta == "number" then
        return a < b
      end
      return tostring(a) < tostring(b)
    end
    return ta < tb
  end)
  local parts = {}
  for i, k in ipairs(keys) do
    local key_json
    if type(k) == "string" then
      key_json = n00n.json.encode(k)
    else
      key_json = n00n.json.encode(tostring(k))
    end
    parts[i] = key_json .. ":" .. stable_json(value[k])
  end
  return "{" .. table.concat(parts, ",") .. "}"
end

local function journal_key(aopts)
  return n00n.workflow.hash(stable_json({
    prompt = aopts.prompt,
    subagent_type = aopts.subagent_type or "general",
    model_tier = aopts.model_tier,
    label = aopts.label,
    output_schema = aopts.output_schema,
  }))
end

local function is_safe_run_id(run_id)
  return type(run_id) == "string" and #run_id >= 8 and #run_id <= 128 and run_id:match(RUN_ID_PATTERN) ~= nil
end

local function workflows_root()
  local state = n00n.env.state_dir()
  if not state then
    return nil
  end
  return n00n.fs.joinpath(state, JOURNAL_DIRNAME)
end

local function run_dir(run_id)
  if not is_safe_run_id(run_id) then
    return nil
  end
  local root = workflows_root()
  if not root then
    return nil
  end
  return n00n.fs.joinpath(root, run_id)
end

local function load_journal(run_id)
  local cache = {}
  local dir = run_dir(run_id)
  if not dir then
    return cache, nil, ""
  end
  local path = n00n.fs.joinpath(dir, JOURNAL_FILENAME)
  local text = n00n.fs.read(path)
  if type(text) ~= "string" or text == "" then
    return cache, path, ""
  end
  for line in string.gmatch(text, "[^\n]+") do
    local ok, row = pcall(n00n.json.decode, line)
    if ok and type(row) == "table" and type(row.k) == "string" and type(row.v) == "string" then
      cache[row.k] = row.v
    end
  end
  return cache, path, text
end

local function write_run_meta(run_id, meta)
  local dir = run_dir(run_id)
  if not dir then
    return
  end
  n00n.fs.mkdir(dir, { parents = true })
  n00n.fs.write(n00n.fs.joinpath(dir, META_FILENAME), n00n.json.encode(meta))
end

local run_seq = 0
local function new_run_id(script)
  run_seq = run_seq + 1
  return n00n.workflow.hash(script .. "\0" .. tostring(os.time()) .. "\0" .. tostring(run_seq))
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
  local sem = n00n.async.semaphore(concurrency)
  local wrapped = {}
  for i, f in ipairs(fns) do
    if type(f) ~= "function" then
      error("parallel: fns[" .. i .. "] must be a function", 0)
    end
    wrapped[i] = function()
      local permit
      local ok, result = pcall(function()
        permit = sem:acquire()
        return f()
      end)
      if permit then
        permit:release()
      end
      if not ok then
        error(result, 0)
      end
      return result
    end
  end
  local results = n00n.async.gather(wrapped)
  local out = {}
  for i, r in ipairs(results) do
    if not r.ok then
      error("parallel: branch " .. i .. " failed: " .. tostring(r.err), 0)
    end
    out[i] = r.value
  end
  return out
end

-- Claude-parity pipeline: each item flows independently through stages, with
-- no cross-item barrier between stages. Concurrent item chains share the
-- parallel() concurrency cap.
local function pipeline(items, stages, popts)
  if type(items) ~= "table" then
    error("pipeline: items must be an array", 0)
  end
  if type(stages) ~= "table" then
    error("pipeline: stages must be an array of functions", 0)
  end
  for i, stage in ipairs(stages) do
    if type(stage) ~= "function" then
      error("pipeline: stages[" .. i .. "] must be a function", 0)
    end
  end
  local fns = {}
  for i, item in ipairs(items) do
    fns[i] = function()
      local value = item
      for _, stage in ipairs(stages) do
        value = stage(value)
      end
      return value
    end
  end
  return parallel(fns, popts)
end

local function make_agent(ctx, progress, journal)
  return function(aopts)
    aopts = aopts or {}
    if type(aopts.prompt) ~= "string" then
      error("agent: opts.prompt (string) is required", 0)
    end
    if not journal.meta_ready then
      error(NO_META_ERROR, 0)
    end

    if aopts.label and type(aopts.label) ~= "string" then
      error("agent: opts.label must be a string", 0)
    end
    local subagent_type = aopts.subagent_type or "general"
    if subagent_type ~= "general" and subagent_type ~= "research" then
      error("agent: unknown subagent_type: " .. tostring(subagent_type), 0)
    end
    local label = aopts.label or n00n.ui.truncate_text(aopts.prompt, NAME_LABEL_MAX).head
    local key = journal_key(aopts)

    -- Per-key single-flight: concurrent agent() calls with the same key wait
    -- on that key's lock, so only one spends tokens. Distinct keys stay free
    -- to run in parallel; journal I/O uses a separate mutex.
    local key_lock
    do
      local gate = journal.lock:acquire()
      local hit = journal.cache[key]
      if hit ~= nil then
        gate:release()
        progress.agent_cached(label)
        return hit
      end
      key_lock = journal.in_flight[key]
      if not key_lock then
        key_lock = n00n.async.semaphore(1)
        journal.in_flight[key] = key_lock
      end
      gate:release()
    end

    local key_permit = key_lock:acquire()
    local ok, result = pcall(function()
      do
        local gate = journal.lock:acquire()
        local hit = journal.cache[key]
        if hit ~= nil then
          gate:release()
          progress.agent_cached(label)
          return hit
        end
        journal.agent_count = journal.agent_count + 1
        if journal.agent_count > MAX_AGENTS_PER_RUN then
          gate:release()
          error(AGENT_LIMIT_ERROR, 0)
        end
        gate:release()
      end

      local validator
      if aopts.output_schema then
        if type(aopts.output_schema) ~= "table" or aopts.output_schema.type ~= "object" then
          error(SCHEMA_ROOT_ERROR, 0)
        end
        local compile_err
        validator, compile_err = n00n.json.schema_validator(aopts.output_schema)
        if compile_err then
          error(SCHEMA_COMPILE_ERROR .. ": " .. compile_err, 0)
        end
      end

      local model, model_err = n00n.agent.resolve_model(ctx, { tier = aopts.model_tier })
      if model_err then
        error(model_err, 0)
      end

      local audience = subagent_type == "research" and RESEARCH_AUDIENCE or GENERAL_AUDIENCE
      local prompt_id = subagent_type == "research" and RESEARCH_PROMPT or GENERAL_PROMPT
      local system, system_err = n00n.agent.system_prompt(ctx, { prompt_id = prompt_id, instructions = true })
      if system_err then
        error(system_err, 0)
      end

      local tool_defs, tools_err = n00n.agent.tools(ctx, {
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
      local sess, sess_err = n00n.agent.session(ctx, {
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
      local prompt_result, prompt_err = sess:prompt(message)
      local retries = 0
      while not prompt_err and validator and not captured and retries < MAX_STRUCTURED_RETRIES do
        retries = retries + 1
        prompt_result, prompt_err = sess:prompt(NUDGE_MISSING)
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
      local out = prompt_result.text
      if captured then
        local encoded, encode_err = n00n.json.encode(captured)
        if encode_err then
          error("failed to encode structured output: " .. tostring(encode_err), 0)
        end
        out = encoded
      end

      local gate = journal.lock:acquire()
      journal.cache[key] = out
      local io_ok, io_err = pcall(function()
        if not journal.path then
          return
        end
        local dir = n00n.fs.dirname(journal.path)
        if dir then
          n00n.fs.mkdir(dir, { parents = true })
        end
        local line = n00n.json.encode({ k = key, v = out }) .. "\n"
        journal.text = (journal.text or "") .. line
        n00n.fs.write(journal.path, journal.text)
      end)
      journal.in_flight[key] = nil
      gate:release()
      if not io_ok then
        error(io_err, 0)
      end
      return out
    end)
    key_permit:release()
    if not ok then
      local gate = journal.lock:acquire()
      if journal.in_flight[key] == key_lock then
        journal.in_flight[key] = nil
      end
      gate:release()
      error(result, 0)
    end
    return result
  end
end

local function make_progress(ctx)
  local tol = ctx:tool_output_lines()
  local max_lines = (tol and tol.workflow) or DEFAULT_OUTPUT_LINES
  local view = ToolView.new(n00n.ui.buf(), { max_lines = max_lines, keep = "tail" })
  local started_at = os.time()
  local state = { name = "workflow", phase = "starting", agents = 0, done = 0, cached = 0 }
  local lock = n00n.async.semaphore(1)

  local function refresh_header()
    local elapsed = math.max(os.time() - started_at, 0)
    local header = {
      { state.name .. " · " .. state.phase .. " · " .. n00n.ui.humantime(elapsed), "bold" },
    }
    if state.agents > 0 or state.cached > 0 then
      header[#header + 1] = {
        string.format("agents %d/%d cached %d", state.done, state.agents, state.cached),
        "dim",
      }
    end
    view:set_header(header)
  end

  local function with_lock(fn)
    local permit = lock:acquire()
    local ok, err = pcall(fn)
    permit:release()
    if not ok then
      error(err, 0)
    end
  end

  view.buf:on("click", function()
    view:toggle()
  end)
  refresh_header()

  return {
    buf = view.buf,
    set_name = function(name)
      with_lock(function()
        state.name = name
        refresh_header()
      end)
    end,
    set_phase = function(name)
      with_lock(function()
        state.phase = name
        refresh_header()
      end)
    end,
    log = function(msg)
      with_lock(function()
        view:append({ { msg, "dim" } })
      end)
    end,
    agent_started = function(label)
      with_lock(function()
        state.agents = state.agents + 1
        refresh_header()
        view:append({ { "> " .. label, "dim" } })
      end)
    end,
    agent_done = function(label)
      with_lock(function()
        state.done = state.done + 1
        refresh_header()
        view:append({ { "+ " .. label, "dim" } })
      end)
    end,
    agent_cached = function(label)
      with_lock(function()
        state.cached = state.cached + 1
        refresh_header()
        view:append({ { "= " .. label, "dim" } })
      end)
    end,
  }
end

local function build_env(ctx, progress, inputs, journal, captured)
  local env = {
    inputs = inputs,
    agent = make_agent(ctx, progress, journal),
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
    string = SAFE_STRING,
    table = SAFE_TABLE,
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
    journal.meta_ready = true
    progress.set_name(t.name)
  end
  env.phase = function(name, fn)
    if type(fn) ~= "function" then
      error("phase: fn must be a function", 0)
    end
    if not journal.meta_ready then
      error(NO_META_ERROR, 0)
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

  local syntax_fn, syntax_err = n00n.workflow.compile(input.script, {})
  if not syntax_fn then
    return { llm_output = SCRIPT_ERROR_PREFIX .. tostring(syntax_err), is_error = true }
  end

  local run_id = input.resume
  if type(run_id) == "string" and run_id ~= "" then
    if not is_safe_run_id(run_id) then
      return { llm_output = INVALID_RUN_ID_ERROR, is_error = true }
    end
  else
    run_id = new_run_id(input.script)
  end
  local cache, journal_path, journal_text = load_journal(run_id)
  local journal = {
    cache = cache,
    path = journal_path or (function()
      local dir = run_dir(run_id)
      if not dir then
        return nil
      end
      return n00n.fs.joinpath(dir, JOURNAL_FILENAME)
    end)(),
    text = journal_text or "",
    lock = n00n.async.semaphore(1),
    in_flight = {},
    meta_ready = false,
    agent_count = 0,
  }

  local progress = make_progress(ctx)
  progress.log("run_id " .. run_id)
  local captured = {}

  local function on_finish(err, result)
    if err then
      ctx:finish({
        llm_output = SCRIPT_ERROR_PREFIX .. tostring(err),
        is_error = true,
        body = progress.buf,
        state = { run_id = run_id },
      })
    else
      ctx:finish({
        llm_output = result,
        body = progress.buf,
        format = "markdown",
        state = { run_id = run_id },
      })
    end
  end

  -- Bound pure-Lua runaway loops (while true) via the VM watchdog deadline.
  ctx:set_deadline(opts.timeout_secs)

  n00n.async.run(function()
    local permit
    local ok, result = pcall(function()
      permit = workflow_semaphore:acquire()
      local env = build_env(ctx, progress, input.inputs or {}, journal, captured)
      local run_fn, load_err = n00n.workflow.compile(input.script, env)
      if not run_fn then
        error(tostring(load_err), 0)
      end
      local output = run_fn()
      if not captured.meta then
        error(NO_META_ERROR, 0)
      end
      write_run_meta(run_id, {
        name = captured.meta.name,
        description = captured.meta.description,
        run_id = run_id,
      })
      if type(output) ~= "string" then
        output = tostring(output)
      end
      return output .. "\n\n_run_id: `" .. run_id .. "` (pass as `resume` to continue)_"
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

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- For complex, multi-stage orchestration of many agents, use **workflow** (a team of agents led by a supervisor inside the sandboxed runtime).",
})

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
    local width = math.max(n00n.ui.terminal_size().cols - BODY_INDENT_COLS, MIN_BODY_WIDTH)
    local ok, md_lines = pcall(n00n.ui.markdown, output, width)
    if ok then
      return ToolView.restore_lines(md_lines, restore_opts)
    end
  end
  return ToolView.restore(output, restore_opts)
end

n00n.api.register_tool({
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
