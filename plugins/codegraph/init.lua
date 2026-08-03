local ExploreResult = require("n00n.explore_result")
local truncate = require("n00n.truncate")
local output_limits = require("n00n.output_limits")
local n00n_codegraph = n00n.codegraph

local cwd = n00n.uv.cwd() or "."
local CG_TIMEOUT_SECS = 30

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Use **codegraph** for cross-file structural queries, call paths, and impact analysis before editing. Use **index** for single-file skeletons before read.",
})

local opts = n00n.api.register_options(output_limits.extend({}))

n00n.api.register_tool({
  name = "codegraph",
  kind = "read",
  defer_loading = true,
  namespace = "explore",
  description = [[Query a pre-indexed semantic codegraph for cross-file structural analysis. Returns verbatim source code grouped by file, plus a dependency impact "blast radius" summary with caller counts and test coverage info. Typically uses fewer tokens than broad grep + read for the same cross-file question.

Best for:
- Understanding how a system works end-to-end ("how does X work")
- Finding call paths ("what calls Y", "call path from A to B")
- Checking blast radius before editing ("what depends on Z")
- Cross-file symbol resolution

Prefer **index** for single-file structure, then **read** for specific sections. codegraph excels at multi-file exploration and impact analysis.

Requires a .codegraph/ index in the project root.]],

  schema = {
    type = "object",
    required = { "command" },
    properties = {
      command = {
        type = "string",
        enum = { "explore", "callers", "callees", "impact", "affected", "node", "query", "sync", "files" },
        description = "CodeGraph command to run",
      },
      query = {
        type = "string",
        description = "Natural language question or symbol/file names to explore (for explore/query commands)",
      },
      symbol = {
        type = "string",
        description = "Symbol name for callers/callees/impact/node commands",
      },
      name = {
        type = "string",
        description = "Symbol name for node command",
      },
      node_id = {
        type = "string",
        description = "Node ID for node command",
      },
      search = {
        type = "string",
        description = "Search query for query command",
      },
      files = {
        type = "array",
        items = { type = "string" },
        description = "Array of file paths for affected command",
      },
      projectPath = { type = "string", description = "Absolute path to the project (defaults to current workspace)" },
      timeout_secs = {
        type = "integer",
        description = "Timeout in seconds for CodeGraph operations (default 30)",
      },
    },
  },

  header = function(input)
    local subtitle = input.query or input.symbol or input.name or input.search or ""
    return ExploreResult.header(subtitle, input.projectPath)
  end,

  restore = function(_input, output, _is_error, ctx)
    return ExploreResult.restore(output, ctx)
  end,

  handler = function(input, ctx)
    if not input.command then
      return { llm_output = "error: command is required", is_error = true }
    end

    local project_path = input.projectPath or cwd
    local timeout = input.timeout_secs or CG_TIMEOUT_SECS

    if not n00n_codegraph.has_index(project_path) then
      return {
        llm_output = "error: no .codegraph/ index found in "
          .. project_path
          .. ". Run `codegraph init` first to index the project.",
        is_error = true,
      }
    end

    if not n00n_codegraph.has_database(project_path) and not n00n_codegraph.available() then
      return {
        llm_output = "error: codegraph CLI not found on PATH; install it to query legacy indexes",
        is_error = true,
      }
    end

    local max_lines, max_bytes = output_limits.resolve(opts, ctx)
    local card, live_err = ExploreResult.live(ctx)
    if not card then
      return { llm_output = "error: failed to publish codegraph results: " .. tostring(live_err), is_error = true }
    end

    local output, err
    local cmd = input.command

    if cmd == "explore" then
      if not input.query then
        return { llm_output = "error: query is required for explore command", is_error = true }
      end
      output, err = n00n_codegraph.explore(input.query, project_path, timeout)
    elseif cmd == "callers" then
      if not input.symbol then
        return { llm_output = "error: symbol is required for callers command", is_error = true }
      end
      output, err = n00n_codegraph.callers(input.symbol, project_path, timeout)
    elseif cmd == "callees" then
      if not input.symbol then
        return { llm_output = "error: symbol is required for callees command", is_error = true }
      end
      output, err = n00n_codegraph.callees(input.symbol, project_path, timeout)
    elseif cmd == "impact" then
      if not input.symbol then
        return { llm_output = "error: symbol is required for impact command", is_error = true }
      end
      output, err = n00n_codegraph.impact(input.symbol, project_path, timeout)
    elseif cmd == "affected" then
      if not input.files or #input.files == 0 then
        return { llm_output = "error: files array is required for affected command", is_error = true }
      end
      output, err = n00n_codegraph.affected(input.files, project_path, timeout)
    elseif cmd == "node" then
      local name = input.node_id or input.name or input.symbol
      if not name then
        return { llm_output = "error: node_id, name, or symbol is required for node command", is_error = true }
      end
      output, err = n00n_codegraph.node(name, project_path, timeout)
    elseif cmd == "query" then
      local search = input.search or input.query
      if not search then
        return { llm_output = "error: search or query is required for query command", is_error = true }
      end
      output, err = n00n_codegraph.query(search, project_path, timeout)
    elseif cmd == "sync" then
      output, err = n00n_codegraph.sync(project_path, timeout)
    elseif cmd == "files" then
      output, err = n00n_codegraph.files(project_path, timeout)
    else
      return { llm_output = "error: unknown command: " .. cmd, is_error = true }
    end

    if err then
      return { llm_output = "error: codegraph " .. cmd .. " failed: " .. tostring(err), is_error = true }
    end

    output = (output or ""):gsub("\n+$", "")
    local llm_output = truncate(output, max_lines, max_bytes)
    card:update(output)

    return { llm_output = llm_output, body = card.buf }
  end,
})
