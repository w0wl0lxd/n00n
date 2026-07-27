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

case("cache_key_is_stable", function()
  local key_a = router.cache_key("arbor", { command = "map", project = "." })
  local key_b = router.cache_key("arbor", { project = ".", command = "map" })
  eq(key_a, key_b)
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
