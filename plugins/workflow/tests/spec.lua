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
    eq(type(n00n.agent[fn_name]), "function", "n00n.agent." .. fn_name .. " must be a function")
  end
  local expected_async = { "gather", "semaphore", "run" }
  for _, fn_name in ipairs(expected_async) do
    eq(type(n00n.async[fn_name]), "function", "n00n.async." .. fn_name .. " must be a function")
  end
  eq(type(n00n.json.schema_validator), "function", "n00n.json.schema_validator must be a function")
  eq(type(n00n.json.encode), "function", "n00n.json.encode must be a function")
  eq(type(n00n.workflow.compile), "function", "n00n.workflow.compile must be a function")
  eq(type(n00n.workflow.hash), "function", "n00n.workflow.hash must be a function")
end)

case("workflow_hash_is_stable_sha256", function()
  local a = n00n.workflow.hash("abc")
  local b = n00n.workflow.hash("abc")
  eq(a, b, "hash must be deterministic")
  eq(#a, 64, "sha256 hex is 64 chars")
  eq(a, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
  assert(a ~= n00n.workflow.hash("abd"), "different inputs must differ")
end)

case("workflow_compile_sandbox_blocks_globals", function()
  local env = { agent = function() end }
  local fn, err = n00n.workflow.compile("return n00n, os, io, require, print, math, coroutine, debug", env)
  eq(err, nil, "script must compile, got: " .. tostring(err))
  local n00n_v, os_v, io_v, require_v, print_v, math_v, coro_v, debug_v = fn()
  eq(n00n_v, nil, "n00n must not leak into the sandbox")
  eq(os_v, nil, "os must not leak into the sandbox")
  eq(io_v, nil, "io must not leak into the sandbox")
  eq(require_v, nil, "require must not leak into the sandbox")
  eq(print_v, nil, "print must not leak into the sandbox")
  eq(math_v, nil, "un-whitelisted math must not leak into the sandbox")
  eq(coro_v, nil, "coroutine must not leak into the sandbox")
  eq(debug_v, nil, "debug must not leak into the sandbox")
end)

case("workflow_compile_exposes_env_capabilities", function()
  local bare_string = { format = string.format }
  local safe_string = setmetatable({}, {
    __index = bare_string,
    __newindex = function()
      error("readonly", 0)
    end,
    __metatable = false,
  })
  local env = {
    math = { floor = math.floor, max = math.max },
    string = safe_string,
    table = { concat = table.concat },
  }
  local fn, err = n00n.workflow.compile("return math.random, math.floor, math.max, string.format, table.concat", env)
  eq(err, nil, "script must compile, got: " .. tostring(err))
  local rand, floor, max, sfmt, tconcat = fn()
  eq(rand, nil, "math.random must be blocked to keep scripts deterministic")
  assert(type(floor) == "function", "math.floor must be available")
  assert(type(max) == "function", "math.max must be available")
  assert(type(sfmt) == "function", "string.format must be available")
  assert(type(tconcat) == "function", "table.concat must be available")
end)

case("workflow_sandbox_stdlib_is_readonly", function()
  local bare = { format = string.format }
  local safe = setmetatable({}, {
    __index = bare,
    __newindex = function()
      error("readonly", 0)
    end,
    __metatable = false,
  })
  local host_format = string.format
  local env = { string = safe }
  local fn, err = n00n.workflow.compile("string.format = function() end return true", env)
  eq(err, nil, "script must compile, got: " .. tostring(err))
  local ok, run_err = pcall(fn)
  eq(ok, false, "mutating sandboxed string must fail")
  assert(tostring(run_err):find("readonly", 1, true), "must report readonly, got: " .. tostring(run_err))
  eq(string.format, host_format, "host string.format must be unchanged")
end)

case("workflow_compile_reports_syntax_error", function()
  local fn, err = n00n.workflow.compile("return (", {})
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
    n00n.workflow.compile('meta({ name = "audit", phases = { { title = "Review" } } }) return "done"', env)
  eq(err, nil, "script must compile, got: " .. tostring(err))
  eq(fn(), "done", "script return value must pass through")
  eq(captured.meta.name, "audit", "meta.name must be captured")
  eq(captured.meta.phases[1].title, "Review", "meta.phases must be captured")
end)

case("workflow_sandbox_primitives_compose", function()
  local env = { table = table, string = string, tostring = tostring, ipairs = ipairs }
  env.agent = function(o)
    return "agent:" .. o.prompt
  end
  env.pipeline = function(items, stages)
    local out = {}
    for i, item in ipairs(items) do
      local v = item
      for _, s in ipairs(stages) do
        v = s(v)
      end
      out[i] = v
    end
    return out
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
  local fn, err = n00n.workflow.compile(
    [[local a = parallel({ function() return agent({ prompt = "one" }) end, function() return agent({ prompt = "two" }) end })
      local r = pipeline({ "x", "y" }, { function(v) return v .. "a" end, function(v) return v .. "b" end })
      return table.concat(a, ",") .. "|" .. table.concat(r, ",")]],
    env
  )
  eq(err, nil, "script must compile, got: " .. tostring(err))
  eq(fn(), "agent:one,agent:two|xab,yab", "agent/parallel/pipeline must compose inside the sandbox")
end)

case("workflow_schema_validator_available", function()
  local validator, err = n00n.json.schema_validator({
    type = "object",
    properties = { findings = { type = "array", items = { type = "string" } } },
    required = { "findings" },
  })
  eq(err, nil, "valid schema must compile")
  eq(validator:validate({ findings = { "bug" } }), nil, "matching value must produce no errors")
  local errors = validator:validate({ findings = "nope" })
  assert(type(errors) == "table" and #errors > 0, "mismatch must produce an error list")
end)

case("workflow_stable_keying_is_order_insensitive_for_object_keys", function()
  local function stable_json(value)
    local t = type(value)
    if t == "nil" then
      return "null"
    elseif t == "boolean" then
      return value and "true" or "false"
    elseif t == "number" then
      return tostring(value)
    elseif t == "string" then
      return n00n.json.encode(value)
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
      if type(k) == "string" then
        keys[#keys + 1] = k
      end
    end
    table.sort(keys)
    local parts = {}
    for i, k in ipairs(keys) do
      parts[i] = n00n.json.encode(k) .. ":" .. stable_json(value[k])
    end
    return "{" .. table.concat(parts, ",") .. "}"
  end
  local a = n00n.workflow.hash(stable_json({ b = 1, a = 2 }))
  local b = n00n.workflow.hash(stable_json({ a = 2, b = 1 }))
  eq(a, b, "object key order must not change the journal key")
  eq(#a, 64, "journal key is full sha256 hex")
end)

case("workflow_run_id_allowlist_rejects_path_segments", function()
  local function is_safe_run_id(run_id)
    return type(run_id) == "string" and #run_id >= 8 and #run_id <= 128 and run_id:match("^[%x]+$") ~= nil
  end
  assert(is_safe_run_id("aabbccdd"), "plain hex must pass")
  assert(is_safe_run_id(string.rep("a", 64)), "sha256 hex must pass")
  eq(is_safe_run_id("../evil"), false, "path traversal must fail")
  eq(is_safe_run_id("/tmp/abs"), false, "absolute path must fail")
  eq(is_safe_run_id("a/../../b"), false, "nested traversal must fail")
  eq(is_safe_run_id("short"), false, "too short must fail")
  eq(is_safe_run_id(""), false, "empty must fail")
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
