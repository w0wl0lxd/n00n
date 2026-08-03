local ExploreResult = require("n00n.explore_result")
local n00n_arbor = n00n.arbor

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Use **arbor** for caller/callee relationships, project map, and blast-radius diff before broad grep or read.",
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
    return n00n_arbor.graph_callees(symbol, project)
  end)
  if not ok then
    return nil, tostring(results)
  end
  if type(results) ~= "table" then
    return nil, "native graph query returned an invalid result"
  end
  return results
end

local function missing_cli(command)
  return {
    llm_output = "error: Arbor CLI not found on PATH; install it to use '" .. command .. "'",
    is_error = true,
  }
end

local function native_index_ready(project)
  if not n00n_arbor.graph_index_available(project) then
    return false, nil
  end
  if not n00n_arbor.available() then
    return true, nil
  end
  local ok, err = pcall(n00n_arbor.ensure_fresh_index, project)
  if not ok then
    return false, "failed to refresh graph index: " .. tostring(err)
  end
  return true, nil
end

local function native_trace_path(from_symbol, to_symbol, project)
  if not n00n_arbor.graph_index_available(project) then
    return nil, "error: native graph index unavailable for trace_path; ensure .arbor/graph.json exists"
  end
  local ok, results = pcall(n00n_arbor.graph_trace_path, from_symbol, to_symbol, project)
  if not ok then
    return nil, "error: " .. tostring(results)
  end
  if type(results) ~= "table" then
    return nil, "error: native graph index unavailable for trace_path; ensure .arbor/graph.json exists"
  end
  return results
end

local function dispatch(input)
  if not input.command or input.command == "" then
    return { llm_output = "error: command is required", is_error = true }
  end
  local command = input.command:gsub("-", "_")
  if command == "trace" then
    command = "trace_path"
  end
  local project = input.project or "."
  local symbol = input.symbol

  if command == "callers" or command == "callees" then
    if not symbol then
      return { llm_output = "error: symbol required for " .. command, is_error = true }
    end
    local native_results, native_err
    local ready, ready_err = native_index_ready(project)
    if ready then
      native_results, native_err = native_relations(command, symbol, project)
    else
      native_err = ready_err
    end
    if native_err then
      return { llm_output = "error: native graph query failed: " .. native_err, is_error = true }
    end
    local results = native_results
    if not results then
      if not n00n_arbor.available() then
        return missing_cli(command)
      end
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
    if n00n_arbor.available() then
      local fresh_ok, fresh_err = pcall(n00n_arbor.ensure_fresh_index, project)
      if not fresh_ok then
        return {
          llm_output = "error: failed to refresh graph index: " .. tostring(fresh_err),
          is_error = true,
        }
      end
    end
    local results, err = native_trace_path(input.from_symbol, input.to_symbol, project)
    if not results then
      return {
        llm_output = err,
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
    local ready, ready_err = native_index_ready(project)
    local entries
    if ready then
      local ok, result = pcall(n00n_arbor.graph_map, project, token_budget)
      if ok then
        entries = result
      else
        entries = nil
      end
    end
    if not entries then
      if not n00n_arbor.available() then
        return missing_cli(command)
      end
      if ready_err then
        return {
          llm_output = "error: native graph query failed: " .. ready_err,
          is_error = true,
        }
      end
      local fresh_ok, fresh_err = pcall(n00n_arbor.ensure_fresh_index, project)
      if not fresh_ok then
        return {
          llm_output = "error: failed to refresh graph index: " .. tostring(fresh_err),
          is_error = true,
        }
      end
      entries = n00n_arbor.map(project, token_budget)
    end
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
    if not n00n_arbor.available() then
      return missing_cli(command)
    end
    local fresh_ok, fresh_err = pcall(n00n_arbor.ensure_fresh_index, project)
    if not fresh_ok then
      return {
        llm_output = "error: failed to refresh graph index: " .. tostring(fresh_err),
        is_error = true,
      }
    end
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
    if not n00n_arbor.available() then
      return missing_cli(command)
    end
    local fresh_ok, fresh_err = pcall(n00n_arbor.ensure_fresh_index, project)
    if not fresh_ok then
      return {
        llm_output = "error: failed to refresh graph index: " .. tostring(fresh_err),
        is_error = true,
      }
    end
    local result, err = n00n_arbor.query(symbol, project)
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return { llm_output = result }
  end

  if command == "status" then
    if not n00n_arbor.available() then
      return missing_cli(command)
    end
    local output, err = n00n_arbor.status(project)
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return { llm_output = output }
  end

  -- T070: Add new commands that require CLI
  if command == "entry_points" then
    local ready, ready_err = native_index_ready(project)
    local results
    if ready then
      local ok, result = pcall(n00n_arbor.graph_entry_points, project)
      if ok then
        results = result
      else
        results = nil
      end
    end
    if not results then
      if not n00n_arbor.available() then
        return missing_cli(command)
      end
      if ready_err then
        return {
          llm_output = "error: native graph query failed: " .. ready_err,
          is_error = true,
        }
      end
      local fresh_ok, fresh_err = pcall(n00n_arbor.ensure_fresh_index, project)
      if not fresh_ok then
        return {
          llm_output = "error: failed to refresh graph index: " .. tostring(fresh_err),
          is_error = true,
        }
      end
      local output, err = n00n_arbor.entry_points(project)
      if err then
        return { llm_output = "error: " .. tostring(err), is_error = true }
      end
      return { llm_output = output }
    end
    if #results == 0 then
      return { llm_output = "No entry points found" }
    end
    return { llm_output = "Entry points\n" .. format_list(results) }
  end

  if command == "file_graph" then
    if not n00n_arbor.available() then
      return missing_cli(command)
    end
    local fresh_ok, fresh_err = pcall(n00n_arbor.ensure_fresh_index, project)
    if not fresh_ok then
      return {
        llm_output = "error: failed to refresh graph index: " .. tostring(fresh_err),
        is_error = true,
      }
    end
    local path = input.path
    local output, err = n00n_arbor.file_graph(project, path)
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return { llm_output = output }
  end

  if command == "inspect" then
    if not symbol then
      return { llm_output = "error: symbol required for inspect", is_error = true }
    end
    if not n00n_arbor.available() then
      return missing_cli(command)
    end
    local fresh_ok, fresh_err = pcall(n00n_arbor.ensure_fresh_index, project)
    if not fresh_ok then
      return {
        llm_output = "error: failed to refresh graph index: " .. tostring(fresh_err),
        is_error = true,
      }
    end
    local output, err = n00n_arbor.inspect(symbol, project)
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return { llm_output = output }
  end

  if command == "path" then
    if not input.from_symbol or not input.to_symbol then
      return { llm_output = "error: from_symbol and to_symbol required for path", is_error = true }
    end
    if not n00n_arbor.available() then
      return missing_cli(command)
    end
    local fresh_ok, fresh_err = pcall(n00n_arbor.ensure_fresh_index, project)
    if not fresh_ok then
      return {
        llm_output = "error: failed to refresh graph index: " .. tostring(fresh_err),
        is_error = true,
      }
    end
    local output, err = n00n_arbor.path(input.from_symbol, input.to_symbol, project)
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return { llm_output = output }
  end

  if command == "refactor" then
    if not input.operation then
      return { llm_output = "error: operation required for refactor", is_error = true }
    end
    if not n00n_arbor.available() then
      return missing_cli(command)
    end
    local fresh_ok, fresh_err = pcall(n00n_arbor.ensure_fresh_index, project)
    if not fresh_ok then
      return {
        llm_output = "error: failed to refresh graph index: " .. tostring(fresh_err),
        is_error = true,
      }
    end
    local output, err = n00n_arbor.refactor(input.operation, project)
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return { llm_output = output }
  end

  if command == "check" then
    if not n00n_arbor.available() then
      return missing_cli(command)
    end
    local fresh_ok, fresh_err = pcall(n00n_arbor.ensure_fresh_index, project)
    if not fresh_ok then
      return {
        llm_output = "error: failed to refresh graph index: " .. tostring(fresh_err),
        is_error = true,
      }
    end
    local output, err = n00n_arbor.check(project)
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return { llm_output = output }
  end

  if command == "summary" then
    if not n00n_arbor.available() then
      return missing_cli(command)
    end
    local fresh_ok, fresh_err = pcall(n00n_arbor.ensure_fresh_index, project)
    if not fresh_ok then
      return {
        llm_output = "error: failed to refresh graph index: " .. tostring(fresh_err),
        is_error = true,
      }
    end
    local output, err = n00n_arbor.summary(project)
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return { llm_output = output }
  end

  return { llm_output = "error: unknown command: " .. tostring(command), is_error = true }
end

n00n.api.register_tool({
  name = "arbor",
  kind = "read",
  defer_loading = true,
  namespace = "explore",
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
- entry_points: List API entry points and public symbols.
- file_graph: Show file-level dependency graph.
- inspect <symbol>: Detailed symbol information with context.
- path <from_symbol> <to_symbol>: Call path between two symbols (CLI version).
- refactor <operation>: Run refactoring operations (requires Arbor CLI).
- check: Run static analysis checks on the codebase.
- summary: High-level project summary and statistics.

Callers, callees, and trace_path can query .arbor/graph.json natively. Other
commands use the Arbor CLI when available.

Use this to understand call relationships, find affected code, and get a
structured overview of a codebase. Complements codegraph — Arbor shows the
full set of callers/callees, while codegraph traces the call path between
two symbols.]],
  schema = {
    type = "object",
    properties = {
      command = {
        type = "string",
        enum = {
          "callers",
          "callees",
          "trace_path",
          "map",
          "diff",
          "query",
          "status",
          "entry_points",
          "file_graph",
          "inspect",
          "path",
          "refactor",
          "check",
          "summary",
        },
        required = true,
      },
      symbol = { type = "string" },
      from_symbol = { type = "string" },
      to_symbol = { type = "string" },
      operation = { type = "string" },
      project = { type = "string" },
      path = { type = "string" },
      token_budget = { type = "integer", default = 1024 },
    },
  },
  header = function(input)
    local label = input.command or ""
    if input.command == "trace_path" then
      if input.from_symbol and input.to_symbol then
        label = label .. " " .. input.from_symbol .. " -> " .. input.to_symbol
      end
    elseif input.command == "path" then
      if input.from_symbol and input.to_symbol then
        label = label .. " " .. input.from_symbol .. " -> " .. input.to_symbol
      end
    elseif input.command == "inspect" and input.symbol then
      label = label .. " " .. input.symbol
    elseif input.command == "refactor" and input.operation then
      label = label .. " " .. input.operation
    elseif input.symbol then
      label = label .. " " .. input.symbol
    end
    return ExploreResult.header(label, input.project)
  end,
  restore = function(_input, output, _is_error, ctx)
    return ExploreResult.restore(output, ctx)
  end,
  handler = function(input, ctx)
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
