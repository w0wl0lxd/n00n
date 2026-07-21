local n00n_arbor = n00n.arbor

local function format_list(items)
  local lines = {}
  for _, item in ipairs(items) do
    local loc = item.line and (":" .. item.line) or ""
    table.insert(lines, "  " .. item.name .. " (" .. item.kind .. ") " .. item.path .. loc)
  end
  return table.concat(lines, "\n")
end

local function dispatch(input, ctx)
  local command = input.command
  local project = input.project or "."
  local symbol = input.symbol

  if command == "callers" or command == "callees" then
    if not symbol then
      return { llm_output = "error: symbol required for " .. command, is_error = true }
    end
    local results
    if command == "callers" then
      results = n00n_arbor.callers(symbol, project)
    else
      results = n00n_arbor.callees(symbol, project)
    end
    if #results == 0 then
      return { llm_output = "No " .. command .. " found for symbol '" .. symbol .. "'" }
    end
    return { llm_output = command .. " of " .. symbol .. "\n" .. format_list(results) }
  end

  if command == "map" then
    local entries = n00n_arbor.map(project, input.token_budget)
    local lines = {}
    for _, entry in ipairs(entries) do
      table.insert(lines, entry.file)
      for _, sym in ipairs(entry.symbols) do
        local rank = sym.centrality and ("[" .. string.format("%.2f", sym.centrality) .. "]") or ""
        table.insert(lines, "  " .. rank .. sym.name)
      end
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "diff" then
    local impact = n00n_arbor.diff(project)
    local lines = {
      "Blast Radius Impact",
      "  Direct callers: " .. impact.direct_callers,
      "  Indirect callers: " .. impact.indirect_callers,
      "  Blast radius nodes: " .. impact.blast_radius_nodes,
      "  API entrypoints affected: " .. impact.api_entrypoints_affected,
      "  Files likely requiring updates: " .. impact.files_likely_require_updates,
    }
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "query" then
    if not symbol then
      return { llm_output = "error: query string required (use 'symbol' field)", is_error = true }
    end
    return { llm_output = n00n_arbor.query(symbol, project) }
  end

  if command == "status" then
    return { llm_output = n00n_arbor.status(project) }
  end

  return { llm_output = "error: unknown command: " .. tostring(command), is_error = true }
end

n00n.api.register_tool({
  name = "arbor",
  kind = "read",
  description = [[
Graph-based code analysis using Arbor.

Commands:
- callers <symbol>: Who calls this function/class? Returns name, kind, file, and line.
- callees <symbol>: What does this function/class call?
- map: Ranked project skeleton with entry points, centrality scores, and symbol coverage.
- diff: Blast radius of unpushed git changes — shows direct/indirect callers, entry points affected.
- query <text>: Free-text search of the code graph.
- status: Index status (node count, edge count, file count).

Use this to understand call relationships, find affected code, and get a
structured overview of a codebase. Complements codegraph — Arbor shows the
full set of callers/callees, while codegraph traces the call path between
two symbols.]],
  schema = {
    type = "object",
    properties = {
      command = {
        type = "string",
        enum = { "callers", "callees", "map", "diff", "query", "status" },
        required = true,
      },
      symbol = { type = "string" },
      project = { type = "string" },
      token_budget = { type = "integer", default = 1024 },
    },
  },
  handler = function(input, ctx)
    local ok, err = pcall(n00n_arbor.check_binary)
    if not ok or not err then
      return { llm_output = "Arbor CLI not found. Install it with: cargo install arbor-graph-cli", is_error = true }
    end
    return dispatch(input, ctx)
  end,
})
