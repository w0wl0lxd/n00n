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
local telemetry = require("n00n.telemetry")
local structured_output = require("n00n.structured_output")
local guard = require("n00n.guard")
local subagent = require("n00n.subagent")

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
local DEFAULT_AGENTS_PER_RUN = 24
local DEFAULT_CONCURRENT_AGENTS = 4
local DEFAULT_CONCURRENT_WORKFLOWS = 2
local HARD_MAX_CONCURRENT_AGENTS = 8
local HARD_MAX_CONCURRENT_WORKFLOWS = 4
local HARD_MAX_AGGREGATE_AGENTS = 12
local HARD_MAX_AGENTS_PER_RUN = 64
local MAX_PARALLEL_BRANCHES = 32
local MAX_PIPELINE_ITEMS = 32
local MAX_PIPELINE_STAGES = 16
local MAX_SCRIPT_BYTES = 64 * 1024
local MAX_INPUT_BYTES = 64 * 1024
local MAX_JOURNAL_BYTES = 1024 * 1024
local MAX_RESULT_BYTES = 32 * 1024
local RESULT_TRUNCATED_MARKER = "\n[truncated]"
local INVALID_RUN_ID_ERROR = "resume must be a run_id (hex letters/digits only, no path separators)"
local RUN_ID_PATTERN = "^[%x]+$"
local DEFAULT_TIMEOUT_SECS = 600
local ASYNC_RUNTIME_MIN_TIMEOUT_SECS = 60

local description = [[Run sandboxed Lua workflow for multi-stage agent orchestration.

Start with meta({ name, description, phases }). Globals: agent({ prompt, subagent_type?, model_tier?, label?, output_schema? }) returns agent result; parallel(fns, { concurrency? }) runs branches; pipeline(items, stages, { concurrency? }) runs stages per item; phase(name, fn), log(...), inputs.

`inputs` is `{}` when omitted. Lua tables have no `.map`; use `pipeline(items, stages)` or `ipairs`. No n00n, os, io, require, print, or load. Scripts must be deterministic for resume replay, must return the final string, and are capped by max_agents_per_run (default 24, hard maximum 64) with a runaway guard for repeated prompts and consecutive errors. Use task for one agent.]]

local schema = {
  type = "object",
  required = { "script" },
  additionalProperties = false,
  properties = {
    script = {
      type = "string",
      description = "Lua script. Start with meta({...}). Use agent/parallel/pipeline/phase/log. Return final string. Lua tables have no `.map`; use pipeline or ipairs.",
    },
    inputs = {
      description = "Free-form object exposed as global `inputs`; defaults to `{}` when omitted.",
    },
    resume = {
      type = "string",
      description = "Paused run_id. Replays journaled agent() calls.",
    },
    timeout_secs = {
      type = "integer",
      minimum = ASYNC_RUNTIME_MIN_TIMEOUT_SECS,
      description = "Wall-clock timeout for this run (minimum 60s). May shorten, but cannot exceed, the configured workflow timeout.",
    },
  },
}

local opts = n00n.api.register_options({
  max_agents_per_run = {
    default = DEFAULT_AGENTS_PER_RUN,
    min = 1,
    desc = "Agent-call budget per workflow (default 24, hard maximum 64).",
  },
  max_concurrent_agents = {
    default = DEFAULT_CONCURRENT_AGENTS,
    min = 1,
    desc = "Concurrency per parallel()/pipeline() (default 4, hard max 8).",
  },
  max_concurrent_workflows = {
    default = DEFAULT_CONCURRENT_WORKFLOWS,
    min = 1,
    desc = "Concurrent workflows (default 2, hard max 4).",
  },
  timeout_secs = {
    default = DEFAULT_TIMEOUT_SECS,
    min = ASYNC_RUNTIME_MIN_TIMEOUT_SECS,
    desc = "Maximum deadline for one workflow run; per-run timeout_secs may only shorten it.",
  },
})

local max_agents_per_run = math.min(opts.max_agents_per_run or DEFAULT_AGENTS_PER_RUN, HARD_MAX_AGENTS_PER_RUN)
local max_concurrent_agents = math.min(opts.max_concurrent_agents, HARD_MAX_CONCURRENT_AGENTS)
local max_concurrent_workflows = math.min(opts.max_concurrent_workflows, HARD_MAX_CONCURRENT_WORKFLOWS)
local workflow_semaphore = n00n.async.semaphore(max_concurrent_workflows)
local aggregate_agent_semaphore = n00n.async.semaphore(HARD_MAX_AGGREGATE_AGENTS)

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
    thinking = aopts.thinking,
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

local function inspect_file(path, label, missing_error)
  local metadata, metadata_err = n00n.fs.metadata(path)
  if not metadata then
    if metadata_err then
      return nil, "failed to inspect workflow " .. label .. ": " .. tostring(metadata_err), false
    end
    return nil, missing_error, true
  end
  if not metadata.is_file then
    return nil, "workflow " .. label .. " path is not a file", false
  end
  return true, nil, false
end

local function load_journal(run_id, required)
  local cache = {}
  local dir = run_dir(run_id)
  if not dir then
    return nil, nil, nil, "cannot resolve workflow run directory"
  end
  local path = n00n.fs.joinpath(dir, JOURNAL_FILENAME)
  local exists, inspect_err, missing = inspect_file(path, "journal", "resume workflow journal not found")
  if not exists then
    if required or not missing then
      return nil, path, nil, inspect_err
    end
    return cache, path, ""
  end
  local metadata, metadata_err = n00n.fs.metadata(path)
  if not metadata then
    return nil, path, nil, "failed to inspect workflow journal: " .. tostring(metadata_err)
  end
  if metadata.size and metadata.size > MAX_JOURNAL_BYTES then
    return nil, path, nil, "workflow journal exceeds the " .. MAX_JOURNAL_BYTES .. " byte limit"
  end
  local text, read_err = n00n.fs.read(path)
  if type(text) ~= "string" then
    return nil, path, nil, "failed to read workflow journal: " .. tostring(read_err)
  end
  if text == "" then
    return cache, path, ""
  end
  for line in string.gmatch(text, "[^\n]+") do
    local ok, row, decode_err = pcall(n00n.json.decode, line)
    if not ok then
      return nil, path, nil, "invalid workflow journal JSON: " .. tostring(row)
    end
    if type(row) ~= "table" or type(row.k) ~= "string" or type(row.v) ~= "string" then
      return nil, path, nil, "invalid workflow journal JSON: " .. tostring(decode_err or "invalid journal row")
    end
    cache[row.k] = row.v
  end
  return cache, path, text
end

local function load_run_meta(run_id, script_hash)
  local dir = run_dir(run_id)
  if not dir then
    return nil, "cannot resolve workflow run directory"
  end
  local dir_metadata, dir_err = n00n.fs.metadata(dir)
  if not dir_metadata then
    if dir_err then
      return nil, "failed to inspect workflow run: " .. tostring(dir_err)
    end
    return nil, "resume run_id not found: " .. run_id
  end
  if not dir_metadata.is_dir then
    return nil, "workflow run path is not a directory"
  end

  local path = n00n.fs.joinpath(dir, META_FILENAME)
  local exists, inspect_err = inspect_file(path, "metadata", "resume workflow metadata not found")
  if not exists then
    return nil, inspect_err
  end
  local content, read_err = n00n.fs.read(path)
  if type(content) ~= "string" then
    return nil, "failed to read workflow metadata: " .. tostring(read_err)
  end
  local ok, meta, decode_err = pcall(n00n.json.decode, content)
  if not ok then
    return nil, "invalid workflow metadata JSON: " .. tostring(meta)
  end
  if type(meta) ~= "table" then
    return nil, "invalid workflow metadata JSON: " .. tostring(decode_err or "metadata must be an object")
  end
  if meta.run_id ~= run_id then
    return nil, "workflow metadata run_id mismatch"
  end
  if type(meta.script_hash) ~= "string" then
    return nil, "workflow metadata is missing script identity"
  end
  if meta.script_hash ~= script_hash then
    return nil, "workflow resume script mismatch"
  end
  return meta
end

local function write_run_meta(run_id, meta, journal_path)
  local dir = run_dir(run_id)
  if not dir or not journal_path then
    return nil, "cannot resolve workflow run paths"
  end
  local mkdir_ok, mkdir_err = n00n.fs.mkdir(dir, { parents = true })
  if not mkdir_ok then
    return nil, "failed to create workflow run directory: " .. tostring(mkdir_err)
  end

  local meta_path = n00n.fs.joinpath(dir, META_FILENAME)
  local existing_meta, meta_inspect_err = n00n.fs.metadata(meta_path)
  if existing_meta then
    return nil, "workflow metadata already exists"
  end
  if meta_inspect_err then
    return nil, "failed to inspect workflow metadata: " .. tostring(meta_inspect_err)
  end
  local existing_journal, journal_inspect_err = n00n.fs.metadata(journal_path)
  if existing_journal then
    return nil, "workflow journal already exists"
  end
  if journal_inspect_err then
    return nil, "failed to inspect workflow journal: " .. tostring(journal_inspect_err)
  end

  local journal_ok, journal_err = n00n.fs.write(journal_path, "")
  if not journal_ok then
    return nil, "failed to create workflow journal: " .. tostring(journal_err)
  end
  local content, encode_err = n00n.json.encode(meta)
  if not content then
    return nil, "failed to encode workflow metadata: " .. tostring(encode_err)
  end
  local write_ok, write_err = n00n.fs.write(meta_path, content)
  if not write_ok then
    return nil, "failed to write workflow metadata: " .. tostring(write_err)
  end
  return true
end

local run_seq = 0
local function new_run_id(script)
  run_seq = run_seq + 1
  return n00n.workflow.hash(script .. "\0" .. tostring(os.time()) .. "\0" .. tostring(run_seq))
end

local function bounded_text(text, limit)
  if #text <= limit then
    return text
  end
  if limit <= #RESULT_TRUNCATED_MARKER then
    return RESULT_TRUNCATED_MARKER:sub(1, limit)
  end
  return text:sub(1, limit - #RESULT_TRUNCATED_MARKER) .. RESULT_TRUNCATED_MARKER
end

local function parallel(fns, popts)
  if type(fns) ~= "table" then
    error("parallel: fns must be an array of functions", 0)
  end
  if #fns > MAX_PARALLEL_BRANCHES then
    error("parallel: branch count exceeds " .. MAX_PARALLEL_BRANCHES, 0)
  end
  popts = popts or {}
  local concurrency = max_concurrent_agents
  if type(popts.concurrency) == "number" then
    concurrency = math.max(1, math.min(popts.concurrency, max_concurrent_agents))
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
  if #items > MAX_PIPELINE_ITEMS then
    error("pipeline: item count exceeds " .. MAX_PIPELINE_ITEMS, 0)
  end
  if #stages > MAX_PIPELINE_STAGES then
    error("pipeline: stage count exceeds " .. MAX_PIPELINE_STAGES, 0)
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

local function make_agent(ctx, progress, journal, logger, run_guard)
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
    local aggregate_permit
    local ok, result = pcall(function()
      do
        local gate = journal.lock:acquire()
        local hit = journal.cache[key]
        if hit ~= nil then
          gate:release()
          progress.agent_cached(label)
          return hit
        end
        gate:release()
      end

      local guard_ok, guard_err = run_guard:check(aopts.prompt)
      if not guard_ok then
        error(guard_err, 0)
      end

      aggregate_permit = aggregate_agent_semaphore:acquire()
      progress.agent_started(label)
      if logger then
        logger.log("agent_started", { label = label, model_tier = aopts.model_tier, subagent_type = subagent_type })
      end

      local captured, launch_err = subagent.launch(ctx, {
        description = label,
        prompt = aopts.prompt,
        subagent_type = subagent_type,
        model_tier = aopts.model_tier,
        thinking = aopts.thinking,
        output_schema = aopts.output_schema,
        include_mcp = true,
      })

      local record_ok, record_err = run_guard:record(aopts.prompt, launch_err)
      if not record_ok then
        aggregate_permit:release()
        aggregate_permit = nil
        error(record_err, 0)
      end

      aggregate_permit:release()
      aggregate_permit = nil

      if launch_err then
        error("sub-agent error: " .. launch_err, 0)
      end

      progress.agent_done(label)
      if logger then
        logger.log("agent_done", { label = label, model_tier = aopts.model_tier, subagent_type = subagent_type })
      end

      local out
      if type(captured) == "string" then
        out = captured
      elseif captured then
        local encoded, encode_err = n00n.json.encode(captured)
        if encode_err then
          error("failed to encode structured output: " .. tostring(encode_err), 0)
        end
        out = encoded
      else
        out = ""
      end
      out = bounded_text(out, MAX_RESULT_BYTES)

      local gate = journal.lock:acquire()
      local io_ok, io_err = pcall(function()
        if not journal.path then
          error("cannot resolve workflow journal path", 0)
        end
        local dir = n00n.fs.dirname(journal.path)
        if dir then
          local mkdir_ok, mkdir_err = n00n.fs.mkdir(dir, { parents = true })
          if not mkdir_ok then
            error("failed to create workflow journal directory: " .. tostring(mkdir_err), 0)
          end
        end
        local line, encode_err = n00n.json.encode({ k = key, v = out })
        if not line then
          error("failed to encode workflow journal entry: " .. tostring(encode_err), 0)
        end
        local next_text = (journal.text or "") .. line .. "\n"
        if #next_text > MAX_JOURNAL_BYTES then
          error("workflow journal exceeds the " .. MAX_JOURNAL_BYTES .. " byte limit", 0)
        end
        local write_ok, write_err = n00n.fs.write(journal.path, next_text)
        if not write_ok then
          error("failed to write workflow journal: " .. tostring(write_err), 0)
        end
        journal.text = next_text
        journal.cache[key] = out
      end)
      journal.in_flight[key] = nil
      gate:release()
      if not io_ok then
        error(io_err, 0)
      end
      return out
    end)
    if aggregate_permit then
      aggregate_permit:release()
    end
    key_permit:release()
    if not ok then
      if logger then
        logger.log("agent_error", { label = label, error = tostring(result) })
      end
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
      { { state.name .. " · " .. state.phase .. " · " .. n00n.ui.humantime(elapsed), "bold" } },
    }
    if state.agents > 0 or state.cached > 0 then
      header[#header + 1] = {
        { string.format("agents %d/%d cached %d", state.done, state.agents, state.cached), "dim" },
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

local function build_env(ctx, progress, inputs, journal, captured, saga, logger, run_guard)
  local env = {
    inputs = inputs,
    agent = make_agent(ctx, progress, journal, logger, run_guard),
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
    if not journal.initialized then
      local meta_ok, meta_err = write_run_meta(journal.run_id, {
        name = t.name,
        description = t.description,
        run_id = journal.run_id,
        script_hash = journal.script_hash,
      }, journal.path)
      if not meta_ok then
        error(meta_err, 0)
      end
      journal.initialized = true
    end
    captured.meta = t
    journal.meta_ready = true
    progress.set_name(t.name)
    local sess_id, _ = n00n.session.current()
    if sess_id then
      pcall(function()
        n00n.session.set_title({ id = sess_id, title = "workflow: " .. t.name:sub(1, 60) })
      end)
    end
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
  env.compensate = function(fn)
    if type(fn) ~= "function" then
      error("compensate: fn must be a function", 0)
    end
    if not saga then
      error("compensate() is only available inside a workflow", 0)
    end
    table.insert(saga.compensations, fn)
  end
  env.on_error = function(fn)
    if type(fn) ~= "function" then
      error("on_error: fn must be a function", 0)
    end
    if not saga then
      error("on_error() is only available inside a workflow", 0)
    end
    table.insert(saga.error_handlers, fn)
  end
  return env
end

local function handler(input, ctx)
  if type(input.script) ~= "string" or input.script == "" then
    return { llm_output = SCRIPT_REQUIRED_ERROR, is_error = true }
  end
  if #input.script > MAX_SCRIPT_BYTES then
    return {
      llm_output = "workflow script exceeds the " .. MAX_SCRIPT_BYTES .. " byte limit",
      is_error = true,
    }
  end
  if input.inputs then
    local encoded_inputs, inputs_err = n00n.json.encode(input.inputs)
    if not encoded_inputs then
      return { llm_output = "workflow inputs are not serializable: " .. tostring(inputs_err), is_error = true }
    end
    if #encoded_inputs > MAX_INPUT_BYTES then
      return {
        llm_output = "workflow inputs exceed the " .. MAX_INPUT_BYTES .. " byte limit",
        is_error = true,
      }
    end
  end
  if input.timeout_secs and input.timeout_secs < ASYNC_RUNTIME_MIN_TIMEOUT_SECS then
    return {
      llm_output = "timeout_secs must be at least " .. ASYNC_RUNTIME_MIN_TIMEOUT_SECS,
      is_error = true,
    }
  end

  local syntax_fn, syntax_err = n00n.workflow.compile(input.script, {})
  if not syntax_fn then
    return { llm_output = SCRIPT_ERROR_PREFIX .. tostring(syntax_err), is_error = true }
  end

  local run_id = input.resume
  local is_resume = type(run_id) == "string" and run_id ~= ""
  local script_hash = n00n.workflow.hash(input.script)
  if is_resume then
    if not is_safe_run_id(run_id) then
      return { llm_output = INVALID_RUN_ID_ERROR, is_error = true }
    end
    local _, meta_err = load_run_meta(run_id, script_hash)
    if meta_err then
      return { llm_output = "failed to resume workflow: " .. meta_err, is_error = true }
    end
  else
    run_id = new_run_id(input.script)
  end

  local cache, journal_path, journal_text, journal_err = load_journal(run_id, is_resume)
  if journal_err then
    return { llm_output = "failed to resume workflow: " .. journal_err, is_error = true }
  end
  local journal = {
    cache = cache,
    path = journal_path,
    text = journal_text or "",
    lock = n00n.async.semaphore(1),
    in_flight = {},
    meta_ready = false,
    initialized = is_resume,
    run_id = run_id,
    script_hash = script_hash,
  }

  local progress = make_progress(ctx)
  ctx:live_buf(progress.buf)
  progress.log("run_id " .. run_id)
  local captured = {}
  local saga = { compensations = {}, error_handlers = {} }
  local workflow_dir = run_dir(run_id)
  local logger = workflow_dir and telemetry.open(n00n.fs.joinpath(workflow_dir, "events"), run_id)
  if logger then
    logger.log("run_started", { run_id = run_id })
  end

  local function run_compensations(err)
    if not saga or #saga.compensations == 0 then
      return err
    end
    local comp_failures = {}
    for i = #saga.compensations, 1, -1 do
      local cok, cerr = pcall(saga.compensations[i])
      if not cok then
        table.insert(comp_failures, tostring(cerr))
      end
    end
    for _, eh in ipairs(saga.error_handlers) do
      pcall(eh, err)
    end
    if #comp_failures > 0 then
      return tostring(err) .. "\ncompensation failures: " .. table.concat(comp_failures, "; ")
    end
    return err
  end

  local function on_finish(err, result)
    if err then
      local final_err = run_compensations(err)
      if logger then
        logger.log("run_error", { error = tostring(final_err) })
      end
      ctx:finish({
        llm_output = SCRIPT_ERROR_PREFIX .. tostring(final_err),
        is_error = true,
        body = progress.buf,
        state = { run_id = run_id },
      })
    else
      if logger then
        logger.log("run_done", { run_id = run_id })
      end
      ctx:finish({
        llm_output = result,
        body = progress.buf,
        format = "markdown",
        state = { run_id = run_id },
      })
    end
  end

  local timeout_secs = math.min(input.timeout_secs or opts.timeout_secs, opts.timeout_secs)

  -- Bound pure-Lua runaway loops (while true) via the VM watchdog deadline.
  ctx:set_deadline(timeout_secs)

  local run_guard = guard.new({ max_calls = max_agents_per_run, timeout_secs = timeout_secs })

  n00n.async.run(function()
    local permit
    local ok, result = pcall(function()
      permit = workflow_semaphore:acquire()
      local env = build_env(ctx, progress, input.inputs or {}, journal, captured, saga, logger, run_guard)
      local run_fn, load_err = n00n.workflow.compile(input.script, env)
      if not run_fn then
        error(tostring(load_err), 0)
      end
      local output = run_fn()
      if not captured.meta then
        error(NO_META_ERROR, 0)
      end
      if type(output) ~= "string" then
        output = tostring(output)
      end
      local run_suffix = "\n\n_run_id: `" .. run_id .. "` (pass as `resume` to continue)_"
      output = bounded_text(output, MAX_RESULT_BYTES - #run_suffix)
      return output .. run_suffix
    end)
    if permit then
      permit:release()
    end
    if not ok then
      -- Compensations run inside on_finish, but if on_finish itself errors we
      -- still want rollback for catastrophic errors. Wrap and re-raise.
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
  defer_loading = true,
  namespace = "agent",
  workload = "orchestrator",
  audiences = { "main" },
  schema = schema,
  handler = handler,
  header = header,
  restore = restore,
})
