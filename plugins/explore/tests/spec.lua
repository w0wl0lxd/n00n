local router = require("router")

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

case("impact_routes_to_cross_file", function()
  eq(router.normalize_intent({ query = "impact of changing restore_item" }), "cross_file")
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

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
