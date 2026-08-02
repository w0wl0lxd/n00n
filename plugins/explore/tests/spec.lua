local router = require("router")

local json = (n00n and n00n.json) or require("n00n.json")

local function try_read_one(path)
  if n00n and n00n.fs and n00n.fs.read then
    local content, err = n00n.fs.read(path)
    if not err then
      return content
    end
  else
    local f, err = io.open(path, "r")
    if f then
      local content = f:read("*a")
      f:close()
      return content
    end
  end
  return nil
end

local function read_file(name)
  local candidates = {
    name,
    "../" .. name,
    "../../" .. name,
  }
  for _, path in ipairs(candidates) do
    local content = try_read_one(path)
    if content then
      return content
    end
  end
  error("could not read " .. name)
end

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

case("file_path_selects_index", function()
  eq(router.normalize_intent({ query = "how does auth work", path = "src/main.rs" }), "file")
end)

case("caller_query_selects_relations", function()
  eq(router.normalize_intent({ query = "callers of restore_item" }), "relations")
end)

case("nl_query_defaults_to_cross_file", function()
  eq(router.normalize_intent({ query = "how does session restore work" }), "cross_file")
end)

case("explicit_command_selects_relations", function()
  eq(router.normalize_intent({ query = "foo", command = "map" }), "relations")
end)

case("explicit_command_beats_path_heuristic", function()
  eq(router.normalize_intent({ path = "src/lib.rs", command = "callers" }), "relations")
end)

case("build_index_input", function()
  local backend, input = router.build_backend_input({ query = "src/lib.rs", intent = "file" }, "file")
  eq(backend, "index")
  eq(input.path, "src/lib.rs")
end)

case("build_arbor_callers_input", function()
  local backend, input = router.build_backend_input({
    query = "callers of restore_item",
    project = "/tmp/project",
    intent = "relations",
  }, "relations")
  eq(backend, "arbor")
  eq(input.command, "callers")
  eq(input.symbol, "restore_item")
  eq(input.project, "/tmp/project")
end)

case("build_codegraph_input", function()
  local backend, input = router.build_backend_input({
    query = "how does auth work",
    project = "/tmp/project",
    intent = "cross_file",
  }, "cross_file")
  eq(backend, "codegraph")
  eq(input.query, "how does auth work")
  eq(input.projectPath, "/tmp/project")
end)

case("trace_path_symbols", function()
  local from_symbol, to_symbol = router.extract_trace_symbols("call path from foo to bar")
  eq(from_symbol, "foo")
  eq(to_symbol, "bar")
end)

case("query_path_selects_index", function()
  eq(router.normalize_intent({ query = "src/main.rs" }), "file")
end)

case("nl_query_mentioning_file_stays_cross_file", function()
  eq(router.normalize_intent({ query = "how does auth work in main.rs" }), "cross_file")
  eq(router.normalize_intent({ query = "impact of changing parse_config.py" }), "cross_file")
end)

case("what_calls_routes_to_callers", function()
  eq(router.parse_arbor_command({ query = "what calls restore_item" }), "callers")
  eq(router.extract_symbol("what calls restore_item", "callers"), "restore_item")
end)

case("what_does_call_routes_to_callees", function()
  eq(router.parse_arbor_command({ query = "what does restore_item call" }), "callees")
  eq(router.extract_symbol("what does restore_item call", "callees"), "restore_item")
end)

case("impact_query_auto_detects", function()
  eq(router.normalize_intent({ query = "impact of changing restore_item" }), "impact")
end)

case("symbol_case_preserved", function()
  eq(router.extract_symbol("Callers of AuthService", "callers"), "AuthService")
  local from_symbol, to_symbol = router.extract_trace_symbols("call path from Foo to Bar")
  eq(from_symbol, "Foo")
  eq(to_symbol, "Bar")
end)

case("cache_key_is_stable", function()
  local key_a = router.cache_key("arbor", { command = "map", project = "." })
  local key_b = router.cache_key("arbor", { project = ".", command = "map" })
  eq(key_a, key_b)
end)

case("cache_key_is_injective", function()
  local key_a = router.cache_key("codegraph", { query = "foo", projectPath = "bar" })
  local key_b = router.cache_key("codegraph", { query = "bar", projectPath = "foo" })
  assert(key_a ~= key_b, "swapped query/projectPath must produce different cache keys")
end)

-- New intent tests (T008-T012)
case("search_intent_routes_to_semblem", function()
  eq(router.normalize_intent({ query = "agent loop", intent = "search" }), "search")
  local backend, input = router.build_backend_input({ query = "agent loop", intent = "search" }, "search")
  eq(backend, "semblem")
  eq(input.command, "search")
  eq(input.query, "agent loop")
end)

case("skeleton_intent_routes_to_index", function()
  eq(router.normalize_intent({ query = "src/main.rs", intent = "skeleton" }), "skeleton")
  local backend, input = router.build_backend_input({ query = "src/main.rs", intent = "skeleton" }, "skeleton")
  eq(backend, "index")
  eq(input.path, "src/main.rs")
end)

case("symbol_intent_routes_to_codegraph", function()
  eq(router.normalize_intent({ query = "AuthService", intent = "symbol" }), "symbol")
  local backend, input = router.build_backend_input({ query = "AuthService", intent = "symbol" }, "symbol")
  eq(backend, "codegraph")
  eq(input.command, "node")
  eq(input.name, "AuthService")
end)

case("impact_intent_routes_to_codegraph", function()
  eq(router.normalize_intent({ query = "impact of changing restore_item", intent = "impact" }), "impact")
  local backend, input =
    router.build_backend_input({ query = "impact of changing restore_item", intent = "impact" }, "impact")
  eq(backend, "codegraph")
  eq(input.command, "impact")
  eq(input.symbol, "restore_item")
  eq(input.projectPath, ".")
end)

case("trace_intent_routes_to_arbor", function()
  eq(router.normalize_intent({ query = "call path from foo to bar", intent = "trace" }), "trace")
  local backend, input = router.build_backend_input({ query = "call path from foo to bar", intent = "trace" }, "trace")
  eq(backend, "arbor")
  eq(input.command, "trace_path")
  eq(input.from_symbol, "foo")
  eq(input.to_symbol, "bar")
end)

-- T020: Router classification accuracy test (SC-001)
case("router_classification_accuracy", function()
  local content = read_file("tests/fixtures/explore-queries.json")

  local queries = json.decode(content)

  local correct = 0
  for _, q in ipairs(queries) do
    local inferred = router.normalize_intent({ query = q.query })
    if inferred == q.intent then
      correct = correct + 1
    end
  end

  local accuracy = (correct / #queries) * 100
  assert(accuracy >= 90, "router classification accuracy " .. accuracy .. "% is below 90% threshold")
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
