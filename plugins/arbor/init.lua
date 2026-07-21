local n00n_arbor = n00n.arbor

local function dispatch(input, ctx)
  local command = input.command
  local project = input.project or "."
  local symbol = input.symbol

  if command == "callers" or command == "callees" then
    if not symbol then
      return { llm_output = "error: symbol required for " .. command, is_error = true }
    end
    if command == "callers" then
      return { llm_output = n00n_arbor.callers(symbol, project) }
    else
      return { llm_output = n00n_arbor.callees(symbol, project) }
    end
  end

  if command == "map" then
    local results = n00n_arbor.map(project, input.token_budget)
    local lines = {}
    for _, entry in ipairs(results) do
      local rank = entry.rank and (" [" .. string.format("%.2f", entry.rank) .. "]") or ""
      table.insert(lines, entry.path .. rank)
      for _, sym in ipairs(entry.symbols) do
        table.insert(lines, "  " .. sym)
      end
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "diff" then
    local results = n00n_arbor.diff(project)
    local lines = {}
    for _, item in ipairs(results) do
      table.insert(lines, item.name .. " (" .. item.path .. ") distance=" .. item.distance)
    end
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
- callers <symbol>: Who calls this function/class?
- callees <symbol>: What does this function/class call?
- map: Ranked project skeleton with symbols (supports token_budget)
- diff: Blast radius of unpushed git changes
- query <text>: Free-text search of the code graph
- status: Index status

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
