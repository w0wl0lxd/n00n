local M = {}

local FILE_EXT_PATTERN = "%.[A-Za-z0-9]+$"

local function trim(text)
  return (text or ""):match("^%s*(.-)%s*$") or ""
end

local function lower(text)
  return trim(text):lower()
end

-- Treat as a file path only when the whole string looks path-like.
-- Reject NL queries that merely mention a file ("how does X work in main.rs").
local function looks_like_file_path(text, allow_whitespace)
  local value = trim(text)
  if value == "" then
    return false
  end
  if not allow_whitespace and value:find("%s") then
    return false
  end
  return value:match(FILE_EXT_PATTERN) ~= nil
end

function M.normalize_intent(input)
  local intent = input.intent
  if intent and intent ~= "auto" then
    return intent
  end

  if input.command then
    return "relations"
  end

  local path = trim(input.path)
  if path ~= "" and looks_like_file_path(path, true) then
    return "file"
  end

  if looks_like_file_path(input.query, false) then
    return "file"
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
    or query:match("what does%s+.-call$")
    or query:match("trace.?path")
    or query:match("call path")
    or query:match("^map$")
    or query:match("project map")
    or query:match("^status$")
    or query:match("^diff$")
  then
    return "relations"
  end

  -- New intents for auto-detection
  if query:match("^impact") or query:match("blast.?radius") or query:match("affected") then
    return "impact"
  end

  if query:match("symbol") or query:match("definition") or query:match("decl") then
    return "symbol"
  end

  return "cross_file"
end

function M.parse_arbor_command(input)
  if input.command then
    return input.command
  end

  local query = lower(input.query)
  if query:match("caller") or query:match("who calls") or query:match("what calls") then
    return "callers"
  end
  if query:match("callee") or query:match("what does%s+.-call$") then
    return "callees"
  end
  if query:match("trace.?path") or query:match("call path") then
    return "trace_path"
  end
  if query:match("^diff") then
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

  local trimmed = trim(query)
  local lowered = lower(query)

  local token = "[%w_%.:%->]+"

  local patterns = {
    callers = {
      "^callers?%s+of%s+()(" .. token .. ")%s*$",
      "^who%s+calls%s+()(" .. token .. ")%s*$",
      "^what%s+calls%s+()(" .. token .. ")%s*$",
    },
    callees = {
      "^callees?%s+of%s+()(" .. token .. ")%s*$",
      "^what%s+does%s+()(" .. token .. ")%s+call$",
    },
    query = {
      "^query%s+()(.-)%s*$",
      "^search%s+()(.-)%s*$",
    },
    impact = {
      "^impact%s+of%s+changing%s+()(" .. token .. ")%s*$",
      "^impact%s+of%s+()(" .. token .. ")%s*$",
    },
  }

  local command_patterns = patterns[command]
  if command_patterns then
    for _, pattern in ipairs(command_patterns) do
      local _, _, pos, symbol = lowered:find(pattern)
      if pos and symbol then
        return trim(trimmed:sub(pos, pos + #symbol - 1))
      end
    end
  end

  return trimmed
end

function M.extract_trace_symbols(query)
  local trimmed = trim(query)
  local normalized = lower(query)

  local token = "[%w_%.:%->]+"
  local _, _, from_pos, from_symbol, to_pos, to_symbol =
    normalized:find("from%s+()(" .. token .. ")%s+to%s+()(" .. token .. ")")
  if from_pos and to_pos and from_symbol and to_symbol then
    local function slice(pos, sym)
      return trim(trimmed:sub(pos, pos + #sym - 1))
    end
    return slice(from_pos, from_symbol), slice(to_pos, to_symbol)
  end
  return nil, nil
end

function M.build_backend_input(input, intent)
  local project = input.project or "."

  if intent == "file" or intent == "skeleton" then
    local path = trim(input.path)
    if path == "" then
      path = trim(input.query)
    end
    return "index", { path = path }
  end

  if intent == "search" then
    return "semblem",
      {
        command = "search",
        query = input.query,
        repo = project,
        mode = input.mode or "bm25",
      }
  end

  if intent == "relations" or intent == "trace" then
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

  if intent == "symbol" then
    local symbol = input.symbol or trim(input.query)
    return "codegraph", {
      command = "node",
      name = symbol,
      projectPath = project,
    }
  end

  if intent == "impact" then
    local symbol = input.symbol or M.extract_symbol(input.query, "impact")
    return "arbor", {
      command = "impact",
      symbol = symbol,
      project = project,
    }
  end

  return "codegraph", {
    query = input.query,
    projectPath = project,
  }
end

function M.cache_key(backend, backend_input)
  local keys = {}
  for key in pairs(backend_input) do
    keys[#keys + 1] = key
  end

  table.sort(keys)

  local parts = { string.format("%d:%s", #backend, backend) }
  for _, key in ipairs(keys) do
    local value = tostring(backend_input[key])
    parts[#parts + 1] = string.format("%d:%s=%d:%s", #key, key, #value, value)
  end

  return table.concat(parts, "\0")
end

return M
