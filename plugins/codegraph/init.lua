local ExploreResult = require("n00n.explore_result")
local truncate = require("n00n.truncate")
local output_limits = require("n00n.output_limits")

local cwd = n00n.uv.cwd() or "."
local CG_TIMEOUT_SECS = 30
local cg_available

local function shell_quote(s)
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

local function check_codegraph()
  if cg_available ~= nil then
    return cg_available
  end
  local id = n00n.fn.jobstart("codegraph --version")
  local result = n00n.fn.jobwait(id, 3000)
  if result then
    cg_available = (result.exit_code == 0)
  else
    n00n.fn.jobstop(id)
    cg_available = false
  end
  return cg_available
end

local function check_index(project_path)
  local meta = n00n.fs.metadata(project_path .. "/.codegraph")
  return meta and meta.is_dir
end

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

    if not check_codegraph() then
      return {
        llm_output = "error: codegraph CLI not found. Install it from https://github.com/colbymchenry/codegraph",
        is_error = true,
      }
    end

    local project_path = input.projectPath or cwd

    if not check_index(project_path) then
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

    local id = n00n.fn.jobstart("codegraph explore -- " .. shell_quote(input.query) .. " " .. shell_quote(project_path))
    local result = n00n.fn.jobwait(id, CG_TIMEOUT_SECS * 1000)

    if not result then
      n00n.fn.jobstop(id)
      return { llm_output = "error: codegraph explore timed out after " .. CG_TIMEOUT_SECS .. "s", is_error = true }
    end

    if result.exit_code ~= 0 then
      local err_msg = (result.stderr or ""):gsub("^%s*(.-)%s*$", "%1")
      if err_msg == "" then
        err_msg = (result.stdout or ""):gsub("^%s*(.-)%s*$", "%1")
      end
      if err_msg == "" then
        err_msg = "exit code " .. result.exit_code
      end
      return { llm_output = "error: codegraph explore failed: " .. err_msg, is_error = true }
    end

    local output = (result.stdout or ""):gsub("\n+$", "")
    local llm_output = truncate(output, max_lines, max_bytes)
    card:update(output)

    return { llm_output = llm_output, body = card.buf }
  end,
})
