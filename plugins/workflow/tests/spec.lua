local failures = {}

local function case(name, fn)
  local ok, err = pcall(fn)
  if not ok then
    table.insert(failures, name .. ": " .. tostring(err))
  end
end

local function eq(actual, expected, msg)
  if actual ~= expected then
    error((msg or "") .. "\nexpected: " .. tostring(expected) .. "\n  actual: " .. tostring(actual))
  end
end

case("workflow_host_api_surface", function()
  local expected_agent = { "resolve_model", "system_prompt", "tools", "call_tool", "session" }
  for _, fn_name in ipairs(expected_agent) do
    eq(type(noon.agent[fn_name]), "function", "noon.agent." .. fn_name .. " must be a function")
  end
  local expected_async = { "gather", "semaphore", "run" }
  for _, fn_name in ipairs(expected_async) do
    eq(type(noon.async[fn_name]), "function", "noon.async." .. fn_name .. " must be a function")
  end
  eq(type(noon.json.schema_validator), "function", "noon.json.schema_validator must be a function")
  eq(type(noon.json.encode), "function", "noon.json.encode must be a function")
  eq(type(noon.workflow.compile), "function", "noon.workflow.compile must be a function")
end)

case("workflow_compile_sandbox_blocks_globals", function()
  local env = { agent = function() end }
  local fn, err = noon.workflow.compile("return noon, os, io, require, print, math, coroutine, debug", env)
  eq(err, nil, "script must compile, got: " .. tostring(err))
  local noon_v, os_v, io_v, require_v, print_v, math_v, coro_v, debug_v = fn()
  eq(noon_v, nil, "noon must not leak into the sandbox")
  eq(os_v, nil, "os must not leak into the sandbox")
  eq(io_v, nil, "io must not leak into the sandbox")
  eq(require_v, nil, "require must not leak into the sandbox")
  eq(print_v, nil, "print must not leak into the sandbox")
  eq(math_v, nil, "un-whitelisted math must not leak into the sandbox")
  eq(coro_v, nil, "coroutine must not leak into the sandbox")
  eq(debug_v, nil, "debug must not leak into the sandbox")
end)

case("workflow_compile_exposes_env_capabilities", function()
  -- Mirrors build_env's math whitelist (no random), keeping scripts deterministic.
  local env = { math = { floor = math.floor, max = math.max }, string = string, table = table }
  local fn, err = noon.workflow.compile("return math.random, math.floor, math.max, string.format, table.concat", env)
  eq(err, nil, "script must compile, got: " .. tostring(err))
  local rand, floor, max, sfmt, tconcat = fn()
  eq(rand, nil, "math.random must be blocked to keep scripts deterministic")
  assert(type(floor) == "function", "math.floor must be available")
  assert(type(max) == "function", "math.max must be available")
  assert(type(sfmt) == "function", "string.format must be available")
  assert(type(tconcat) == "function", "table.concat must be available")
end)

case("workflow_compile_reports_syntax_error", function()
  local fn, err = noon.workflow.compile("return (", {})
  eq(fn, nil, "broken source must not compile")
  assert(err ~= nil, "broken source must report a compile error")
end)

case("workflow_meta_capture_pattern", function()
  local captured = {}
  local env = {}
  env.meta = function(t)
    if captured.meta then
      error("meta() must be called exactly once", 0)
    end
    captured.meta = t
  end
  local fn, err =
    noon.workflow.compile('meta({ name = "audit", phases = { { title = "Review" } } }) return "done"', env)
  eq(err, nil, "script must compile, got: " .. tostring(err))
  eq(fn(), "done", "script return value must pass through")
  eq(captured.meta.name, "audit", "meta.name must be captured")
  eq(captured.meta.phases[1].title, "Review", "meta.phases must be captured")
end)

case("workflow_sandbox_primitives_compose", function()
  -- Stub the primitives the way build_env injects them, then prove a script
  -- can pipeline and parallel over them without seeing noon.
  local env = { table = table, string = string, tostring = tostring, ipairs = ipairs }
  env.agent = function(o)
    return "agent:" .. o.prompt
  end
  env.pipeline = function(stages, init)
    local v = init
    for _, s in ipairs(stages) do
      v = s(v)
    end
    return v
  end
  env.parallel = function(fns)
    local out = {}
    for i, f in ipairs(fns) do
      out[i] = f()
    end
    return out
  end
  env.phase = function(_name, fn)
    return fn()
  end
  env.log = function() end
  local fn, err = noon.workflow.compile(
    [[local a = parallel({ function() return agent({ prompt = "one" }) end, function() return agent({ prompt = "two" }) end })
      local r = pipeline({ function(x) return x .. "a" end, function(x) return x .. "b" end }, "")
      return table.concat(a, ",") .. "|" .. r]],
    env
  )
  eq(err, nil, "script must compile, got: " .. tostring(err))
  eq(fn(), "agent:one,agent:two|ab", "agent/parallel/pipeline must compose inside the sandbox")
end)

case("workflow_schema_validator_available", function()
  local validator, err = noon.json.schema_validator({
    type = "object",
    properties = { findings = { type = "array", items = { type = "string" } } },
    required = { "findings" },
  })
  eq(err, nil, "valid schema must compile")
  eq(validator:validate({ findings = { "bug" } }), nil, "matching value must produce no errors")
  local errors = validator:validate({ findings = "nope" })
  assert(type(errors) == "table" and #errors > 0, "mismatch must produce an error list")
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
