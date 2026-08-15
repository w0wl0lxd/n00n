local ExploreResult = require("n00n.explore_result")
local router = require("router")

local cwd = n00n.uv.cwd() or "."

local function trim(text)
  return (text or ""):match("^%s*(.-)%s*$") or ""
end

local session_cache = {}

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Use **explore** first for codebase questions; it routes to index (single-file skeleton) or codegraph (callers/callees, impact, cross-file structure).",
})

local FALLBACK_BACKEND = "semblem"

local function route_label(backend, intent)
  return string.format("%s via %s", intent, backend)
end

-- A backend that is missing its index, or whose CLI is absent, must not fail the
-- whole call: semblem needs no prebuilt graph, so it is the degraded route.
local function fallback_dispatch(input, ctx, failed_backend)
  if failed_backend == FALLBACK_BACKEND then
    return nil, nil, "no further fallback"
  end
  local backend_input = router.search_backend_input(input)
  if not backend_input.query then
    return nil, nil, "no query, symbol, or path to search for"
  end
  local output, err = n00n.agent.call_tool(ctx, FALLBACK_BACKEND, backend_input)
  if err then
    return nil, nil, err
  end
  return { llm_output = output or "" }, FALLBACK_BACKEND, nil
end

local function dispatch(input, ctx, use_cache)
  local intent = router.normalize_intent(input)
  local backend, backend_input = router.build_backend_input(input, intent)
  if backend_input and backend_input.is_error then
    return backend_input, route_label(backend or "unknown", intent), false
  end
  local cache_key = router.cache_key(backend, backend_input)

  if use_cache and session_cache[cache_key] then
    local cached = session_cache[cache_key]
    return cached, route_label(backend, intent), true
  end

  local output, err = n00n.agent.call_tool(ctx, backend, backend_input)
  if err then
    local degraded, degraded_backend, degraded_err = fallback_dispatch(input, ctx, backend)
    if degraded then
      return degraded,
        route_label(degraded_backend, intent) .. string.format(" (degraded from %s: %s)", backend, tostring(err)),
        false
    end
    return {
      llm_output = string.format(
        "error: explore dispatch to %s failed: %s; fallback to %s also failed: %s",
        backend,
        tostring(err),
        FALLBACK_BACKEND,
        tostring(degraded_err)
      ),
      is_error = true,
    },
      route_label(backend, intent),
      false
  end

  local result = { llm_output = output or "" }

  if use_cache and not result.is_error then
    session_cache[cache_key] = result
  end

  return result, route_label(backend, intent), false
end

n00n.api.register_tool({
  name = "explore_code",
  aliases = { "explore" },
  kind = "read",
  description = [[Unified codebase exploration router. Picks the best backend for the question:
- **file** or **skeleton** intent (or a file path): compact single-file skeleton via `index`
- **relations** intent: caller/callee maps via `codegraph`
- **cross_file** intent (default for NL questions): structural cross-file analysis via `codegraph`
- **search** intent: keyword or natural-language search via `semblem`
- **symbol** intent: symbol drill-down via `codegraph node`
- **impact** intent: blast-radius analysis via `codegraph impact`

Set `intent` explicitly when you know the backend. Otherwise the router infers from the query.
Use `command` and `symbol` for precise relations routing.]],

  schema = {
    type = "object",
    properties = {
      query = {
        type = "string",
        description = "Question, symbol, or file path to explore. Required unless `command` is provided.",
      },
      path = {
        type = "string",
        description = "File path for skeleton queries. A file extension selects the index backend in auto mode.",
      },
      project = {
        type = "string",
        description = "Project root for codegraph queries (defaults to cwd).",
      },
      intent = {
        type = "string",
        enum = { "auto", "file", "skeleton", "relations", "cross_file", "search", "symbol", "impact" },
        default = "auto",
      },
      command = {
        type = "string",
        enum = { "callers", "callees", "query" },
      },
      symbol = { type = "string" },
      use_cache = { type = "boolean", default = false },
      mode = {
        type = "string",
        enum = { "bm25", "hybrid", "semantic" },
        description = "Search mode for semblem (bm25, hybrid, or semantic).",
      },
    },
  },

  header = function(input)
    local project = input.project or cwd
    return ExploreResult.header(input.query or input.command or "", project)
  end,

  restore = function(_input, output, _is_error, ctx)
    return ExploreResult.restore(output, ctx)
  end,

  handler = function(input, ctx)
    if (not input.query or trim(input.query) == "") and not input.command then
      return { llm_output = "error: query is required", is_error = true }
    end

    local card, live_err = ExploreResult.live(ctx)
    if not card then
      return {
        llm_output = "error: failed to publish explore results: " .. tostring(live_err),
        is_error = true,
      }
    end

    local use_cache = input.use_cache == true
    local result, route, cached = dispatch(input, ctx, use_cache)
    if result.is_error then
      card:update(result.llm_output)
      return { llm_output = result.llm_output, body = card.buf, is_error = true }
    end

    local output = result.llm_output or ""
    local prefix = "[" .. route .. (cached and ", cached" or "") .. "]\n"
    local llm_output = prefix .. output
    card:update(output)

    return { llm_output = llm_output, body = card.buf }
  end,
})
