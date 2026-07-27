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
  description = [[Query a pre-indexed semantic codegraph for cross-file structural analysis. Returns verbatim source code grouped by file, plus a dependency impact "blast radius" summary with caller counts and test coverage info. Typically uses fewer tokens than broad grep + read for the same cross-file question.

Best for:
- Understanding how a system works end-to-end ("how does X work")
- Finding call paths ("what calls Y", "call path from A to B")
- Checking blast radius before editing ("what depends on Z")
- Cross-file symbol resolution

Prefer **index** for single-file structure, then **read** for specific sections. codegraph excels at multi-file exploration and impact analysis.

Requires the codegraph CLI and a .codegraph/ index in the project root.]],

  schema = {
    type = "object",
    required = { "query" },
    properties = {
      query = {
        type = "string",
        description = "Natural language question or symbol/file names to explore (e.g. 'AuthService login', 'GraphTraverser BFS impact')",
      },
      projectPath = { type = "string", description = "Absolute path to the project (defaults to current workspace)" },
    },
  },

  header = function(input)
    return ExploreResult.header(input.query, input.projectPath)
  end,

  restore = function(_input, output, _is_error, ctx)
    return ExploreResult.restore(output, ctx)
  end,

  handler = function(input, ctx)
    if not input.query then
      return { llm_output = "error: query is required", is_error = true }
    end

    if not n00n_codegraph.available() then
      return {
        llm_output = "error: codegraph CLI not found. Install it from https://github.com/colbymchenry/codegraph",
        is_error = true,
      }
    end

    local project_path = input.projectPath or cwd

    if not n00n_codegraph.has_index(project_path) then
      return {
        llm_output = "error: no .codegraph/ index found in "
          .. project_path
          .. ". Run `codegraph init` first to index the project.",
        is_error = true,
      }
    end

    local max_lines, max_bytes = output_limits.resolve(opts, ctx)
    local card, live_err = ExploreResult.live(ctx)
    if not card then
      return { llm_output = "error: failed to publish codegraph results: " .. tostring(live_err), is_error = true }
    end

    local ok, output = pcall(n00n_codegraph.explore, input.query, project_path, CG_TIMEOUT_SECS)
    if not ok then
      return { llm_output = "error: codegraph explore failed: " .. tostring(output), is_error = true }
    end

    output = (output or ""):gsub("\n+$", "")
    local llm_output = truncate(output, max_lines, max_bytes)
    card:update(output)

    return { llm_output = llm_output, body = card.buf }
  end,
})
