local ExploreResult = require("n00n.explore_result")
local truncate = require("n00n.truncate")
local output_limits = require("n00n.output_limits")
local semblem = n00n.semblem

local cwd = n00n.uv.cwd() or "."

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Use **semblem** for BM25 code search across the repo; use **explore** for structural graph questions.",
})

local opts = n00n.api.register_options(output_limits.extend({}))

local function resolve_repo(input)
  return input.repo or cwd
end

n00n.api.register_tool({
  name = "semblem",
  kind = "read",
  description = [[Search indexed source code with BM25 keyword ranking. Builds a `.n00n/search/` index on first use.

Commands:
- `search`: ranked snippets for a natural-language or keyword query
- `find_related`: related chunks for a file location
- `savings`: show token savings from using semantic search (requires semble CLI; no native fallback)

`mode` defaults to `bm25`. `hybrid` and `semantic` try the upstream semble CLI first and fall back to BM25 with an embedder nag if unavailable.]],
  strict = true,

  schema = {
    type = "object",
    additionalProperties = false,
    required = { "command", "repo", "query", "file_path", "line", "mode", "top_k", "content" },
    properties = {
      command = {
        type = "string",
        enum = { "search", "find_related", "savings" },
        required = true,
        description = "Semblem command: search, find_related, or savings",
      },
      repo = { type = { "string", "null" }, required = true, description = "Project root (defaults to cwd)" },
      query = { type = { "string", "null" }, required = true, description = "Search query (for search command)" },
      file_path = { type = { "string", "null" }, required = true, description = "File path (for find_related command)" },
      line = { type = { "integer", "null" }, required = true, description = "Line number (for find_related command)" },
      mode = {
        type = { "string", "null" },
        enum = { "bm25", "hybrid", "semantic" },
        required = true,
        description = "Search mode: bm25 (default), hybrid, or semantic",
      },
      top_k = { type = { "integer", "null" }, required = true, description = "Number of results to return (default 5)" },
      content = {
        type = { "string", "null" },
        enum = { "docs", "config", "code", "all" },
        required = true,
        description = "Content filter: docs, config, code, or all (default code)",
      },
    },
  },

  header = function(input)
    local label = input.command or "search"
    if input.command == "search" then
      label = input.query or label
    elseif input.command == "find_related" then
      label = string.format("%s:%s", input.file_path or "?", tostring(input.line or "?"))
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
      return { llm_output = "error: failed to publish semblem results: " .. tostring(live_err), is_error = true }
    end

    local ok, output, err
    if command == "search" then
      if not input.query or input.query:match("^%s*$") then
        return { llm_output = "error: query is required for search", is_error = true }
      end
      local content = input.content
      ok, output = pcall(semblem.search, repo, input.query, input.mode or "bm25", input.top_k or 5, content)
    elseif command == "find_related" then
      if not input.file_path or not input.line then
        return { llm_output = "error: file_path and line are required for find_related", is_error = true }
      end
      ok, output = pcall(semblem.find_related, repo, input.file_path, input.line, input.top_k or 5)
    elseif command == "savings" then
      ok, output, err = true, semblem.savings(repo)
    else
      return { llm_output = "error: unsupported command: " .. tostring(command), is_error = true }
    end

    if not ok then
      return { llm_output = "error: semblem failed: " .. tostring(output), is_error = true }
    end
    if err then
      return { llm_output = "error: semblem failed: " .. tostring(err), is_error = true }
    end

    output = (output or ""):gsub("\n+$", "")
    local llm_output = truncate(output, max_lines, max_bytes)
    card:update(output)
    return { llm_output = llm_output, body = card.buf }
  end,
})
