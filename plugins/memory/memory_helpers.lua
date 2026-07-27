local M = {}

M.MAX_LINES_PER_FILE = 200
M.MAX_DIR_BYTES = 50 * 1024
M.DEFAULT_SEARCH_LIMIT = 10
M.MAX_SEARCH_LIMIT = 50
M.LITE_HINT_LIMIT = 5
M.MIN_TOKEN_LEN = 2

local function normalize_string_list(values)
  if values == nil then
    return nil
  end
  if type(values) == "string" then
    local normalized = {}
    for token in values:gmatch("([^,]+)") do
      local trimmed = token:match("^%s*(.-)%s*$")
      if trimmed ~= "" then
        normalized[#normalized + 1] = trimmed
      end
    end
    return (#normalized > 0) and normalized or nil
  end
  if type(values) == "table" then
    local normalized = {}
    for _, token in ipairs(values) do
      if type(token) == "string" then
        local trimmed = token:match("^%s*(.-)%s*$")
        if trimmed ~= "" then
          normalized[#normalized + 1] = trimmed
        end
      end
    end
    return (#normalized > 0) and normalized or nil
  end
  return nil
end

local function clamp_importance(value)
  if type(value) == "string" then
    local parsed = tonumber(value)
    if not parsed then
      return 1
    end
    value = parsed
  end
  if type(value) ~= "number" then
    return 1
  end
  local rounded = math.floor(value)
  if rounded < 1 then
    return 1
  end
  if rounded > 5 then
    return 5
  end
  return rounded
end

local function normalize_layer(layer)
  if type(layer) ~= "string" then
    return "deep"
  end
  local normalized = layer:lower()
  if normalized == "lite" then
    return "lite"
  end
  return "deep"
end

function M.fnv1a_64(data)
  local lo = 0x84222325
  local hi = 0xcbf29ce4
  local p_lo = 0x000001b3
  local p_hi = 0x00000100
  for i = 1, #data do
    lo = bit32.bxor(lo, string.byte(data, i))
    local ll = lo * p_lo
    local ll_lo = ll % 0x100000000
    local ll_hi = (ll - ll_lo) / 0x100000000
    local new_hi = (hi * p_lo + lo * p_hi + ll_hi) % 0x100000000
    lo = ll_lo
    hi = new_hi
  end
  return string.format("%08x%08x", hi, lo)
end

function M.count_lines(s)
  if s == "" then
    return 1
  end
  local _, newlines = s:gsub("\n", "")
  if s:sub(-1) == "\n" then
    return math.max(newlines, 1)
  end
  return newlines + 1
end

function M.project_id(path)
  local base = n00n.fs.basename(path) or "root"
  return base .. "-" .. M.fnv1a_64(path)
end

function M.safe_resolve(memories_dir, relative)
  if not relative or relative == "" then
    return nil, "path is required"
  end
  local first = relative:sub(1, 1)
  if relative:find("\0") or first == "/" or first == "\\" then
    return nil, "path must be relative"
  end
  if relative:match("^%a:") then
    return nil, "path must be relative"
  end
  local resolved = n00n.fs.normalize(n00n.fs.joinpath(memories_dir, relative))
  local norm_base = n00n.fs.normalize(memories_dir)
  local sep = norm_base:find("\\") and "\\" or "/"
  local prefix = norm_base .. sep
  if resolved:sub(1, #prefix) ~= prefix then
    return nil, "path traversal outside memories directory is not allowed"
  end
  return resolved
end

function M.collect_file_entries(dir)
  local entries = n00n.fs.dir(dir)
  if not entries then
    return {}
  end
  local files = {}
  for _, entry in ipairs(entries) do
    if entry[2] == "file" then
      local meta = n00n.fs.metadata(n00n.fs.joinpath(dir, entry[1]))
      if meta then
        files[#files + 1] = { entry[1], meta.size, meta.mtime or 0 }
      end
    end
  end
  return files
end

function M.dir_total_bytes(dir)
  local total = 0
  for _, f in ipairs(M.collect_file_entries(dir)) do
    total = total + f[2]
  end
  return total
end

function M.parse_frontmatter(content)
  local rest = content:match("^%s*%-%-%-\n(.*)")
  if not rest then
    return {}, content
  end
  local end_pos = rest:find("\n%-%-%-")
  if not end_pos then
    return {}, content
  end
  local yaml_str = rest:sub(1, end_pos)
  local body = rest:sub(end_pos + 4):match("^%s*(.-)%s*$")
  local fm, _ = n00n.yaml.decode(yaml_str)
  if type(fm) ~= "table" then
    fm = {}
  end
  fm.tags = normalize_string_list(fm.tags)
  fm.importance = clamp_importance(fm.importance)
  fm.layer = normalize_layer(fm.layer)
  if type(fm.topic) == "string" then
    local topic = fm.topic:match("^%s*(.-)%s*$")
    fm.topic = (#topic > 0) and topic or nil
  else
    fm.topic = nil
  end
  if type(fm.synopsis) == "string" then
    local synopsis = fm.synopsis:match("^%s*(.-)%s*$")
    fm.synopsis = (#synopsis > 0) and synopsis or nil
  else
    fm.synopsis = nil
  end
  return fm, body or ""
end

function M.normalize_metadata(input)
  return {
    tags = normalize_string_list(input and input.tags),
    topic = type(input and input.topic) == "string" and input.topic:match("^%s*(.-)%s*$") or nil,
    importance = clamp_importance(input and input.importance),
    layer = normalize_layer(input and input.layer),
    synopsis = type(input and input.synopsis) == "string" and input.synopsis:match("^%s*(.-)%s*$") or nil,
  }
end

function M.build_frontmatter(meta, body)
  local fm = {}
  if meta.tags then
    fm.tags = meta.tags
  end
  if meta.topic then
    fm.topic = meta.topic
  end
  if meta.importance and meta.importance ~= 1 then
    fm.importance = meta.importance
  end
  if meta.layer and meta.layer ~= "deep" then
    fm.layer = meta.layer
  end
  if meta.synopsis then
    fm.synopsis = meta.synopsis
  end
  if next(fm) == nil then
    return body, nil
  end
  local encoded, encode_err = n00n.yaml.encode(fm)
  if encode_err or not encoded then
    return nil, encode_err or "failed to encode frontmatter"
  end
  encoded = encoded:gsub("\r\n", "\n"):match("^%s*(.-)\n?$")
  return "---\n" .. encoded .. "\n---\n" .. body, nil
end

function M.parse_memory_file(relative_path, raw)
  local fm, body = M.parse_frontmatter(raw)
  return {
    path = relative_path,
    meta = fm,
    body = body,
  }
end

function M.tokenize(text)
  if not text or #text == 0 then
    return {}
  end
  local tokens = {}
  for token in text:lower():gmatch("[%w_]+") do
    if #token >= M.MIN_TOKEN_LEN then
      tokens[token] = true
    end
  end
  return tokens
end

function M.tokenize_path(path)
  if not path or #path == 0 then
    return {}
  end
  local tokens = {}
  for part in path:lower():gmatch("[^/\\]+") do
    local base = part:match("(.+)%.[^%.]+$") or part
    for token in base:gmatch("[%w_]+") do
      if #token >= M.MIN_TOKEN_LEN then
        tokens[token] = true
      end
    end
  end
  return tokens
end

function M.tags_match(meta, required_tags)
  if not required_tags or #required_tags == 0 then
    return true
  end
  if not meta.tags then
    return false
  end
  local have = {}
  for _, tag in ipairs(meta.tags) do
    have[tag:lower()] = true
  end
  for _, tag in ipairs(required_tags) do
    if not have[tag:lower()] then
      return false
    end
  end
  return true
end

function M.searchable_text(entry)
  local meta = entry.meta or {}
  local parts = { entry.path or "", meta.topic or "", meta.synopsis or "", entry.body or "" }
  if meta.tags then
    parts[#parts + 1] = table.concat(meta.tags, " ")
  end
  return table.concat(parts, " ")
end

function M.memory_matches_query(entry, query)
  if not query or #query == 0 then
    return true
  end
  local haystack = M.searchable_text(entry)
  local lowered = haystack:lower()
  local needle = query:lower()
  if lowered:find(needle, 1, true) then
    return true
  end
  local qtokens = M.tokenize(query)
  local htokens = M.tokenize(haystack)
  for token in pairs(qtokens) do
    if htokens[token] then
      return true
    end
  end
  return false
end

function M.score_memory(entry, query, focus_path)
  local meta = entry.meta or {}
  local score = meta.importance or 1
  if query and #query > 0 then
    local qtokens = M.tokenize(query)
    local haystack = M.searchable_text(entry)
    local htokens = M.tokenize(haystack)
    for token in pairs(qtokens) do
      if htokens[token] then
        score = score + 10
      end
    end
    if haystack:lower():find(query:lower(), 1, true) then
      score = score + 20
    end
  end
  if focus_path and focus_path ~= "" then
    local ptokens = M.tokenize_path(focus_path)
    if meta.topic then
      for token in pairs(M.tokenize(meta.topic)) do
        if ptokens[token] then
          score = score + 5
        end
      end
    end
    if meta.tags then
      for _, tag in ipairs(meta.tags) do
        if ptokens[tag:lower()] then
          score = score + 5
        end
      end
    end
  end
  return score
end

function M.rank_memories(entries, query, focus_path)
  local ranked = {}
  local has_query = query and #query > 0
  for _, entry in ipairs(entries) do
    if (not has_query) or M.memory_matches_query(entry, query) then
      ranked[#ranked + 1] = {
        entry = entry,
        score = M.score_memory(entry, query, focus_path),
      }
    end
  end
  table.sort(ranked, function(a, b)
    if a.score ~= b.score then
      return a.score > b.score
    end
    return (a.entry.path or "") < (b.entry.path or "")
  end)
  return ranked
end

function M.lite_summary(entry)
  local meta = entry.meta or {}
  if meta.synopsis and #meta.synopsis > 0 then
    return meta.synopsis
  end
  local first = entry.body:match("^%s*([^\n]+)")
  if first and #first > 0 then
    return first
  end
  return entry.path
end

function M.format_search_result(entry, score)
  local meta = entry.meta or {}
  local parts = { entry.path }
  if meta.topic then
    parts[#parts + 1] = "topic=" .. meta.topic
  end
  if meta.tags then
    parts[#parts + 1] = "tags=" .. table.concat(meta.tags, ",")
  end
  if score and score > 0 then
    parts[#parts + 1] = string.format("score=%d", score)
  end
  return table.concat(parts, " | ")
end

function M.format_list(entries, ranked)
  if ranked then
    local lines = {}
    for _, row in ipairs(ranked) do
      lines[#lines + 1] = M.format_search_result(row.entry, row.score)
    end
    if #lines == 0 then
      return "No matching memories."
    end
    return table.concat(lines, "\n")
  end

  local files = entries
  if #files == 0 then
    return "No memories yet."
  end
  table.sort(files, function(a, b)
    return a.path < b.path
  end)
  local lines = {}
  local total = 0
  for _, entry in ipairs(files) do
    local meta = entry.meta or {}
    local size = entry.size or 0
    lines[#lines + 1] = M.format_search_result(entry, nil) .. " (" .. size .. " bytes)"
    total = total + size
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = #files .. " files, " .. total .. " bytes total"
  return table.concat(lines, "\n")
end

function M.load_entries(dir)
  local entries = {}
  for _, f in ipairs(M.collect_file_entries(dir)) do
    local rel = f[1]
    local file_path = n00n.fs.joinpath(dir, rel)
    local raw = n00n.fs.read(file_path)
    if raw then
      local entry = M.parse_memory_file(rel, raw)
      entry.size = f[2]
      entry.mtime = f[3]
      entries[#entries + 1] = entry
    end
  end
  return entries
end

function M.sanitize_hint_text(text, max_len)
  if not text then
    return ""
  end
  local trimmed = text:match("^%s*(.-)%s*$")
  trimmed = trimmed:gsub("[%c]", " ")
  trimmed = trimmed:gsub("^#+%s*", "")
  trimmed = trimmed:gsub("^%s*%-+%s*", "")
  trimmed = trimmed:gsub("---+", " ")
  trimmed = trimmed:gsub("<[^>]->", " ")
  trimmed = trimmed:gsub("%s+", " ")
  trimmed = trimmed:gsub("[Ii]gnore%s+[Aa]ll%s+[Pp]revious", "")
  trimmed = trimmed:gsub("[Ii]gnore%s+[Pp]revious", "")
  trimmed = trimmed:gsub("[Yy]ou%s+[Mm]ust", "")
  trimmed = trimmed:gsub("[Dd]eveloper%s+[Mm]ode", "")
  trimmed = trimmed:gsub("[Ss]ystem%s*:", "")
  trimmed = trimmed:gsub("[Uu]ser%s*:", "")
  trimmed = trimmed:gsub("[Aa]ssistant%s*:", "")
  trimmed = trimmed:gsub("[Hh]uman%s*:", "")
  trimmed = trimmed:match("^%s*(.-)%s*$")
  if #trimmed > max_len then
    trimmed = trimmed:sub(1, max_len) .. "..."
  end
  return trimmed
end

local function sanitize_hint_path(path)
  if not path then
    return "memory"
  end
  local safe = path:gsub("[%c]", ""):gsub(":", "_")
  if #safe > 64 then
    safe = safe:sub(1, 64)
  end
  return safe
end

function M.build_lite_hint(entries)
  local lite = {}
  for _, entry in ipairs(entries) do
    local meta = entry.meta or {}
    if meta.layer == "lite" then
      lite[#lite + 1] = entry
    end
  end
  table.sort(lite, function(a, b)
    local ai = (a.meta and a.meta.importance) or 1
    local bi = (b.meta and b.meta.importance) or 1
    if ai ~= bi then
      return ai > bi
    end
    return (a.path or "") < (b.path or "")
  end)

  local lines = {}
  local total_bytes = 0
  local max_total = 800
  local max_line = 120
  for i = 1, math.min(#lite, M.LITE_HINT_LIMIT) do
    local entry = lite[i]
    local summary = M.sanitize_hint_text(M.lite_summary(entry), max_line)
    if summary ~= "" then
      local line = "- " .. sanitize_hint_path(entry.path) .. ": " .. summary
      if total_bytes + #line > max_total then
        break
      end
      lines[#lines + 1] = line
      total_bytes = total_bytes + #line
    end
  end
  if #lines == 0 then
    return nil
  end
  return "\n\nProject memory (lite):\n" .. table.concat(lines, "\n") .. "\n"
end

function M.list_memories(dir)
  return M.format_list(M.load_entries(dir), nil)
end

function M.append_body(existing_raw, addition)
  local fm, body = M.parse_frontmatter(existing_raw)
  local separator = (#body > 0 and not body:match("%s$")) and "\n" or ""
  local next_body = body .. separator .. addition
  return M.build_frontmatter(fm, next_body)
end

return M
