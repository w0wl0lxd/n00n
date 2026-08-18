local ExploreResult = require("n00n.explore_result")
local truncate = require("n00n.truncate")
local output_limits = require("n00n.output_limits")
local smell = n00n.smell

local cwd = n00n.uv.cwd() or "."

local opts = n00n.api.register_options(output_limits.extend({}))

local function resolve_repo(input)
  return input.repo or cwd
end

n00n.api.register_tool({
  name = "smell",
  defer_loading = true,
  namespace = "exploration",
  kind = "read",
  description = "Code-smell index. index, search.",

  schema = {
    type = "object",
    required = { "command" },
    properties = {
      command = {
        type = "string",
        enum = { "index", "search" },
        description = "Smell command to run.",
      },
      query = {
        type = "string",
        description = "Keyword or phrase to search for (required for search).",
      },
      repo = {
        type = "string",
        description = "Path to the project root. Defaults to the current working directory.",
      },
      kind = {
        type = "string",
        enum = { "todo", "fixme", "hack", "placeholder" },
        description = "Optional smell kind filter (for search).",
      },
      top_k = {
        type = "integer",
        description = "Maximum number of search results (default 5).",
      },
    },
  },

  header = function(input)
    local label = input.command or "search"
    if input.command == "search" then
      label = input.query or label
    end
    return ExploreResult.header(label, resolve_repo(input))
  end,

  restore = function(_input, output, _is_error, ctx)
    return ExploreResult.restore(output, ctx)
  end,

  handler = function(input, ctx)
    local repo = resolve_repo(input)
    local command = input.command
    if not command then
      return { llm_output = "error: command is required", is_error = true }
    end

    local max_lines, max_bytes = output_limits.resolve_capped(opts, ctx, 12 * 1024)
    local card, live_err = ExploreResult.live(ctx)
    if not card then
      return { llm_output = "error: failed to publish smell results: " .. tostring(live_err), is_error = true }
    end

    local ok, output, err
    if command == "index" then
      ok, err = smell.index(repo)
      output = ok and ("smell index rebuilt for " .. repo) or err
    elseif command == "search" then
      if not input.query or input.query:match("^%s*$") then
        return { llm_output = "error: query is required for search", is_error = true }
      end
      output, err = smell.search(repo, input.query, input.kind, input.top_k or 5)
      ok = output ~= nil
    else
      return { llm_output = "error: unsupported command: " .. tostring(command), is_error = true }
    end

    if not ok then
      return { llm_output = "error: smell failed: " .. tostring(err), is_error = true }
    end

    output = (output or ""):gsub("\n+$", "")
    local llm_output = truncate(output, max_lines, max_bytes)
    card:update(output)
    return { llm_output = llm_output, body = card.buf }
  end,
})
