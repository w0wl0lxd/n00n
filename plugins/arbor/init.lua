local ExploreResult = require("n00n.explore_result")
local n00n_arbor = n00n.arbor

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Use **arbor** for caller/callee relationships, project map, and blast-radius diff before broad grep or read (requires the Arbor CLI).",
})

local function format_list(items)
  local lines = {}
  for _, item in ipairs(items) do
    local loc = item.line and (":" .. item.line) or ""
    table.insert(lines, "  " .. item.name .. " (" .. item.kind .. ") " .. item.path .. loc)
  end
  return table.concat(lines, "\n")
end

local function native_relations(command, symbol, project)
  if not n00n_arbor.graph_index_available(project) then
    return nil
  end
  local ok, results = pcall(function()
    if command == "callers" then
      return n00n_arbor.graph_callers(symbol, project)
    end
    if command == "callees" then
      return n00n_arbor.graph_callees(symbol, project)
    end
    return nil
  end)
  if not ok or type(results) ~= "table" then
    return nil
  end
  return results
end

local function native_trace_path(from_symbol, to_symbol, project)
  if not n00n_arbor.graph_index_available(project) then
    return nil
  end
  local ok, results = pcall(n00n_arbor.graph_trace_path, from_symbol, to_symbol, project)
  if not ok or type(results) ~= "table" then
    return nil
  end
  return results
end

local function dispatch(input)
  local command = input.command
  local project = input.project or "."
  local symbol = input.symbol

  if command == "callers" or command == "callees" then
    if not symbol then
      return { llm_output = "error: symbol required for " .. command, is_error = true }
    end
    local native_results = native_relations(command, symbol, project)
    local results = native_results
    if not results then
      if command == "callers" then
        results = n00n_arbor.callers(symbol, project)
      else
        results = n00n_arbor.callees(symbol, project)
      end
    end
    if #results == 0 then
      return { llm_output = "No " .. command .. " found for symbol '" .. symbol .. "'" }
    end
    return { llm_output = command .. " of " .. symbol .. "\n" .. format_list(results) }
  end

  if command == "trace_path" then
    if not input.from_symbol or not input.to_symbol then
      return {
        llm_output = "error: from_symbol and to_symbol required for trace_path",
        is_error = true,
      }
    end
    local native_results = native_trace_path(input.from_symbol, input.to_symbol, project)
    local results = native_results
    if not results then
      return {
        llm_output = "error: native graph index unavailable for trace_path; ensure .arbor/graph.json exists",
        is_error = true,
      }
    end
    if #results == 0 then
      return {
        llm_output = "No call path found from '" .. input.from_symbol .. "' to '" .. input.to_symbol .. "'",
      }
    end
    return {
      llm_output = "trace_path " .. input.from_symbol .. " -> " .. input.to_symbol .. "\n" .. format_list(results),
    }
  end

  if command == "map" then
    local token_budget = input.token_budget or 1024
    local entries = n00n_arbor.map(project, token_budget)
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
Graph-based code analysis using Arbor. Returns structured, compact
caller/callee/project maps; prefer it over broad grep or unfiltered reads
for relationship and impact questions.

Commands:
- callers <symbol>: Who calls this function/class? Returns name, kind, file, and line.
- callees <symbol>: What does this function/class call?
- trace_path <from_symbol> <to_symbol>: Shortest call path between two symbols (native graph index).
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
        enum = { "callers", "callees", "trace_path", "map", "diff", "query", "status" },
        required = true,
      },
      symbol = { type = "string" },
      from_symbol = { type = "string" },
      to_symbol = { type = "string" },
      project = { type = "string" },
      token_budget = { type = "integer", default = 1024 },
    },
  },
  header = function(input)
    local label = input.command or ""
    if input.command == "trace_path" then
      if input.from_symbol and input.to_symbol then
        label = label .. " " .. input.from_symbol .. " -> " .. input.to_symbol
      end
    elseif input.symbol then
      label = label .. " " .. input.symbol
    end
    return ExploreResult.header(label, input.project)
  end,
  restore = function(_input, output, _is_error, ctx)
    return ExploreResult.restore(output, ctx)
  end,
  handler = function(input, ctx)
    local ok, err = pcall(n00n_arbor.check_binary)
    if not ok then
      return {
        llm_output = "Arbor CLI not found. Install it with: cargo install arbor-graph-cli: " .. tostring(err),
        is_error = true,
      }
    end
    local card, live_err = ExploreResult.live(ctx)
    if not card then
      return { llm_output = "error: failed to publish Arbor results: " .. tostring(live_err), is_error = true }
    end
    local result = dispatch(input)
    card:update(result.llm_output)
    result.body = card.buf
    return result
  end,
})
