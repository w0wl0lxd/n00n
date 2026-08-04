local ExploreResult = require("n00n.explore_result")
local truncate = require("n00n.truncate")
local output_limits = require("n00n.output_limits")
local smell = n00n.smell

local cwd = n00n.uv.cwd() or "."

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Use **smell** to search a persistent index of conflict markers, TODO/FIXME/HACK comments, and placeholder phrases. Run `index` before searching on a new repo.",
})

local opts = n00n.api.register_options(output_limits.extend({}))

local function resolve_repo(input)
  return input.repo or cwd
end

n00n.api.register_tool({
  name = "smell",
  kind = "read",
  description = "Code-smell index. index, search.",

  schema = {
    type = "object",
    required = { "command" },
    properties = {
      command = { type = "string" },
      query = { type = "string" },
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

    local max_lines, max_bytes = output_limits.resolve(opts, ctx)
    local card, live_err = ExploreResult.live(ctx)
    if not card then
      return { llm_output = "error: failed to publish smell results: " .. tostring(live_err), is_error = true }
    end

    local ok, output
    if command == "index" then
      ok = pcall(smell.index, repo)
      if ok then
        output = "smell index rebuilt for " .. repo
      end
    elseif command == "search" then
      if not input.query or input.query:match("^%s*$") then
        return { llm_output = "error: query is required for search", is_error = true }
      end
      ok, output = pcall(smell.search, repo, input.query, input.kind, input.top_k or 5)
    else
      return { llm_output = "error: unsupported command: " .. tostring(command), is_error = true }
    end

    if not ok then
      return { llm_output = "error: smell failed: " .. tostring(output), is_error = true }
    end

    output = (output or ""):gsub("\n+$", "")
    local llm_output = truncate(output, max_lines, max_bytes)
    card:update(output)
    return { llm_output = llm_output, body = card.buf }
  end,
})
