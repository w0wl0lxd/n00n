local M = {}

local FILE_EXT_PATTERN = "%.[A-Za-z0-9]+$"

local function trim(text)
  return (text or ""):match("^%s*(.-)%s*$") or ""
end

local function lower(text)
  return trim(text):lower()
end

function M.normalize_intent(input)
  local intent = input.intent
  if intent and intent ~= "auto" then
    return intent
  end

  local path = trim(input.path)
  if path ~= "" and path:match(FILE_EXT_PATTERN) then
    return "file"
  end

  if input.command then
    return "relations"
  end

  local query = lower(input.query)
  if query == "" then
    return "cross_file"
  end

  if
    query:match("caller")
    or query:match("callee")
    or query:match("who calls")
    or query:match("what calls")
    or query:match("blast")
    or query:match("impact")
    or query:match("trace.?path")
    or query:match("call path")
    or query:match("^map$")
    or query:match("project map")
    or query:match("^status$")
    or query:match("^diff$")
  then
    return "relations"
  end

  return "cross_file"
end

function M.parse_arbor_command(input)
  if input.command then
    return input.command
  end

  local query = lower(input.query)
  if query:match("caller") or query:match("who calls") then
    return "callers"
  end
  if query:match("callee") or query:match("what calls") then
    return "callees"
  end
  if query:match("trace.?path") or query:match("call path") then
    return "trace_path"
  end
  if query:match("blast") or query:match("impact") or query:match("^diff") then
    return "diff"
  end
  if query:match("^map") or query:match("project map") then
    return "map"
  end
  if query:match("^status") then
    return "status"
  end
  return "query"
end

function M.extract_symbol(query, command)
  if not query or query == "" then
    return nil
  end

  local patterns = {
    callers = {
      "callers? of%s+(.+)",
      "who calls%s+(.+)",
    },
    callees = {
      "callees? of%s+(.+)",
      "what does%s+(.+)%s+call",
      "what calls%s+(.+)",
    },
    query = {
      "^query%s+(.+)",
      "^search%s+(.+)",
    },
  }

  local command_patterns = patterns[command]
  if command_patterns then
    for _, pattern in ipairs(command_patterns) do
      local symbol = lower(query):match(pattern)
      if symbol then
        return trim(symbol)
      end
    end
  end

  return trim(query)
end

function M.extract_trace_symbols(query)
  local normalized = lower(query)
  local from_symbol, to_symbol = normalized:match("from%s+(.-)%s+to%s+(.+)$")
  if from_symbol and to_symbol then
    return trim(from_symbol), trim(to_symbol)
  end
  return nil, nil
end

function M.build_backend_input(input, intent)
  local project = input.project or "."

  if intent == "file" then
    local path = trim(input.path)
    if path == "" then
      path = trim(input.query)
    end
    return "index", { path = path }
  end

  if intent == "relations" then
    local command = M.parse_arbor_command(input)
    local backend_input = {
      command = command,
      project = project,
    }

    if command == "trace_path" then
      local from_symbol = input.from_symbol
      local to_symbol = input.to_symbol
      if not from_symbol or not to_symbol then
        from_symbol, to_symbol = M.extract_trace_symbols(input.query)
      end
      backend_input.from_symbol = from_symbol
      backend_input.to_symbol = to_symbol
    elseif command == "map" then
      backend_input.token_budget = input.token_budget
    elseif command ~= "diff" and command ~= "status" then
      backend_input.symbol = input.symbol or M.extract_symbol(input.query, command)
    end

    return "arbor", backend_input
  end

  return "codegraph", {
    query = input.query,
    projectPath = project,
  }
end

function M.cache_key(backend, backend_input)
  local parts = { backend }
  for key in pairs(backend_input) do
    parts[#parts + 1] = key
    parts[#parts + 1] = tostring(backend_input[key])
  end
  table.sort(parts)
  return table.concat(parts, "\0")
end

return M
