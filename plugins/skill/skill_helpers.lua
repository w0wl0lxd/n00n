local M = {}

local DEFAULT_PREVIEW_LINES = 40

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

local function normalize_paths(paths)
  return normalize_string_list(paths)
end

local function normalize_tool_list(values)
  return normalize_string_list(values)
end

local function normalize_steps(steps)
  if type(steps) ~= "table" then
    return nil
  end
  local normalized = {}
  for _, step in ipairs(steps) do
    if type(step) == "table" and type(step.name) == "string" then
      local name = step.name:match("^%s*(.-)%s*$")
      if name ~= "" then
        local section = step.section
        if type(section) == "string" then
          section = section:match("^%s*(.-)%s*$")
          if section == "" then
            section = nil
          end
        else
          section = nil
        end
        normalized[#normalized + 1] = {
          name = name,
          section = section,
          tools = normalize_tool_list(step.tools),
        }
      end
    end
  end
  return (#normalized > 0) and normalized or nil
end

function M.skill_fingerprint(path)
  local meta = n00n.fs.metadata(path)
  if not meta then
    return nil
  end
  local content = n00n.fs.read(path) or ""
  local digest = "0"
  local ok, hash = pcall(function()
    return n00n.workflow.hash(content)
  end)
  if ok and hash then
    digest = hash
  else
    local ok_hash, memory_helpers = pcall(require, "memory.memory_helpers")
    if ok_hash and memory_helpers and memory_helpers.fnv1a_64 then
      digest = memory_helpers.fnv1a_64(content)
    else
      -- Last-resort stable fallback: djb2 over bytes.
      local h = 5381
      for i = 1, #content do
        h = bit32.bor(bit32.lshift(h, 5), h) + string.byte(content, i)
      end
      digest = string.format("%08x", h)
    end
  end
  return string.format("%s:%s:%s:%s", path, tostring(meta.mtime or 0), tostring(meta.size or 0), digest)
end

function M.glob_pattern_to_lua(pattern)
  local glob = pattern:gsub("\\", "/")
  local out = "^"
  local i = 1
  while i <= #glob do
    if glob:sub(i, i + 1) == "**" then
      out = out .. ".*"
      i = i + 2
    elseif glob:sub(i, i) == "*" then
      out = out .. "[^/]*"
      i = i + 1
    elseif glob:sub(i, i) == "?" then
      out = out .. "[^/]"
      i = i + 1
    else
      local c = glob:sub(i, i)
      if c:find("[%^%$%(%)%%%.%[%]%+%-%?]") then
        out = out .. "%" .. c
      else
        out = out .. c
      end
      i = i + 1
    end
  end
  return out .. "$"
end

function M.path_matches_pattern(focus_path, pattern, scope_root)
  if not focus_path or focus_path == "" or not pattern or pattern == "" then
    return false
  end
  local focus = focus_path:gsub("\\", "/")
  local root = (scope_root or "."):gsub("\\", "/")
  if root:sub(-1) ~= "/" then
    root = root .. "/"
  end
  local rel = focus
  if focus:sub(1, #root) == root then
    rel = focus:sub(#root + 1)
  end
  local lua_pat = M.glob_pattern_to_lua(pattern)
  return rel:match(lua_pat) ~= nil or focus:match(lua_pat) ~= nil
end

function M.collect_skill_fingerprints(dir, entries)
  local list = entries or {}
  local dir_entries = n00n.fs.dir(dir)
  if not dir_entries then
    return list
  end

  local skill_path = n00n.fs.joinpath(dir, "SKILL.md")
  local fp = M.skill_fingerprint(skill_path)
  if fp then
    list[#list + 1] = fp
  end

  for _, entry in ipairs(dir_entries) do
    if entry[2] == "directory" then
      M.collect_skill_fingerprints(n00n.fs.joinpath(dir, entry[1]), list)
    end
  end
  return list
end

function M.build_discovery_fingerprint(roots)
  local entries = {}
  for _, entry in ipairs(roots) do
    if n00n.fs.metadata(entry.root) then
      M.collect_skill_fingerprints(entry.root, entries)
    end
  end
  table.sort(entries)
  return table.concat(entries, "\n")
end

function M.record_skill_conflict(conflicts, name, existing, incoming)
  local entry = conflicts[name]
  if not entry then
    conflicts[name] = {
      winner = incoming.location,
      shadowed = { existing.location },
    }
    return
  end
  entry.winner = incoming.location
  entry.shadowed[#entry.shadowed + 1] = existing.location
end

function M.build_conflict_report(conflicts)
  if not conflicts or next(conflicts) == nil then
    return ""
  end
  local names = {}
  for name in pairs(conflicts) do
    names[#names + 1] = name
  end
  table.sort(names)

  local lines = {}
  for _, name in ipairs(names) do
    local entry = conflicts[name]
    local shadowed = table.concat(entry.shadowed, ", ")
    lines[#lines + 1] = "- " .. name .. ": active at " .. entry.winner .. "; shadowed: " .. shadowed
  end
  return "\n\n<skill_conflicts>\n" .. table.concat(lines, "\n") .. "\n</skill_conflicts>"
end

function M.build_skill_stats(stats)
  if not stats then
    return ""
  end
  local cache = stats.cache_hit and "true" or "false"
  return string.format(
    '\n\n<skill_stats cache_hit="%s" skills="%d" conflicts="%d" />\n',
    cache,
    stats.skill_count or 0,
    stats.conflict_count or 0
  )
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
  if not fm then
    fm = {}
  end
  if type(fm) ~= "table" then
    fm = {}
  end
  fm.paths = normalize_paths(fm.paths)
  fm["allowed-tools"] = normalize_tool_list(fm["allowed-tools"])
  fm["disallowed-tools"] = normalize_tool_list(fm["disallowed-tools"])
  fm.tags = normalize_string_list(fm.tags)
  fm.steps = normalize_steps(fm.steps)
  fm["disable-model-invocation"] = fm["disable-model-invocation"] == true
  if type(fm.synopsis) == "string" then
    local synopsis = fm.synopsis:match("^%s*(.-)%s*$")
    fm.synopsis = (#synopsis > 0) and synopsis or nil
  else
    fm.synopsis = nil
  end
  return fm, body
end

function M.read_skill_body(skill)
  if skill.resolve then
    local resolved = skill.resolve()
    if not resolved then
      return nil, "skill resolve returned nil"
    end
    return resolved, nil
  end
  if skill.content then
    return skill.content, nil
  end
  if not skill.location or skill.location:sub(1, 8) == "builtin:" then
    return nil, "skill body unavailable"
  end
  local raw = n00n.fs.read(skill.location)
  if not raw then
    return nil, "failed to read skill file"
  end
  local _, body = M.parse_frontmatter(raw)
  if not body or #body == 0 then
    return nil, "skill body is empty"
  end
  return body, nil
end

function M.extract_section(body, section_name)
  if not section_name or #section_name == 0 then
    return nil, "section is required"
  end
  local target = section_name:lower()
  local current = nil
  local lines = {}
  for _, line in ipairs(n00n.split(body, "\n")) do
    local heading = line:match("^##%s+(.+)$")
    if heading then
      if current then
        break
      end
      if heading:lower() == target then
        current = heading
      end
    elseif current then
      lines[#lines + 1] = line
    end
  end
  if not current then
    return nil, "section not found: " .. section_name
  end
  local section = table.concat(lines, "\n"):match("^%s*(.-)%s*$")
  if not section or #section == 0 then
    return nil, "section is empty: " .. section_name
  end
  return section, nil
end

function M.preview_body(body, max_lines, synopsis)
  if synopsis and #synopsis > 0 then
    return synopsis, false
  end
  local limit = max_lines or DEFAULT_PREVIEW_LINES
  local lines = {}
  local count = 0
  local all_lines = n00n.split(body, "\n")
  for _, line in ipairs(all_lines) do
    if count >= limit then
      break
    end
    lines[#lines + 1] = line
    count = count + 1
  end
  local preview = table.concat(lines, "\n")
  local truncated = #all_lines > count
  return preview, truncated
end

function M.build_tool_policy_lines(skill)
  local lines = {}
  if skill.allowed_tools and #skill.allowed_tools > 0 then
    lines[#lines + 1] = "allowed-tools: " .. table.concat(skill.allowed_tools, ", ")
  end
  if skill.disallowed_tools and #skill.disallowed_tools > 0 then
    lines[#lines + 1] = "disallowed-tools: " .. table.concat(skill.disallowed_tools, ", ")
  end
  if #lines == 0 then
    return ""
  end
  return "## Tool policy\n" .. table.concat(lines, "\n") .. "\n\n"
end

function M.resolve_skill_content(skill, input)
  local body, err = M.read_skill_body(skill)
  if not body then
    return nil, err
  end

  if input.section and #input.section > 0 then
    return M.extract_section(body, input.section)
  end

  if input.preview == true and input.full ~= true then
    local preview, truncated = M.preview_body(body, input.preview_lines, skill.synopsis)
    if truncated then
      preview = preview .. '\n\n[preview truncated; call with full=true or section="..."]'
    end
    return preview, nil
  end

  return body, nil
end

function M.validate_skill(skill)
  local issues = {}
  if not skill.name or #skill.name == 0 then
    issues[#issues + 1] = "missing name"
  end
  if not skill.description or #skill.description == 0 then
    issues[#issues + 1] = "missing description"
  end
  if skill.allowed_tools and skill.disallowed_tools then
    issues[#issues + 1] = "allowed-tools and disallowed-tools are mutually exclusive"
  end
  local _, err = M.read_skill_body(skill)
  if err then
    issues[#issues + 1] = err
  end
  return issues
end

function M.build_validation_report(results)
  if #results == 0 then
    return "\n\n<skill_validation>\nNo skills to validate.\n</skill_validation>"
  end
  local lines = {}
  for _, entry in ipairs(results) do
    if #entry.issues == 0 then
      lines[#lines + 1] = "- " .. entry.name .. ": ok"
    else
      lines[#lines + 1] = "- " .. entry.name .. ": " .. table.concat(entry.issues, "; ")
    end
  end
  return "\n\n<skill_validation>\n" .. table.concat(lines, "\n") .. "\n</skill_validation>"
end

function M.tool_policy_hint(skill)
  if skill.allowed_tools and #skill.allowed_tools > 0 then
    return " [allowed-tools: " .. table.concat(skill.allowed_tools, ", ") .. "]"
  end
  if skill.disallowed_tools and #skill.disallowed_tools > 0 then
    return " [disallowed-tools: " .. table.concat(skill.disallowed_tools, ", ") .. "]"
  end
  return ""
end

function M.tokenize_path(path)
  local tokens = {}
  local seen = {}
  local function add_token(token)
    local normalized = token:lower()
    if #normalized >= 2 and not seen[normalized] then
      seen[normalized] = true
      tokens[#tokens + 1] = normalized
    end
  end
  for segment in path:gmatch("[^/\\]+") do
    for token in segment:gmatch("[%w_]+") do
      add_token(token)
    end
  end
  return tokens
end

function M.graph_rank_signals(project)
  local root = project or n00n.uv.cwd() or "."
  local signals = {
    codegraph_indexed = false,
  }
  if n00n.fs.metadata(n00n.fs.joinpath(root, ".codegraph")) then
    signals.codegraph_indexed = true
  end
  return signals
end

function M.graph_rank_bonus(skill, signals)
  if not skill.paths or #skill.paths == 0 then
    return 0
  end
  local bonus = 0
  if signals.codegraph_indexed then
    bonus = bonus + 3
  end
  return bonus
end

function M.score_skill(skill, focus_path, options)
  if not focus_path or #focus_path == 0 then
    return 0
  end
  local score = 0
  local tokens = M.tokenize_path(focus_path)
  local name = (skill.name or ""):lower()
  local desc = (skill.description or ""):lower()
  for _, token in ipairs(tokens) do
    if name:find(token, 1, true) then
      score = score + 5
    end
    if desc:find(token, 1, true) then
      score = score + 2
    end
    if skill.tags then
      for _, tag in ipairs(skill.tags) do
        local normalized_tag = tag:lower()
        if normalized_tag == token or normalized_tag:find(token, 1, true) then
          score = score + 8
        end
      end
    end
  end
  if options and options.graph_rank and options.signals then
    score = score + M.graph_rank_bonus(skill, options.signals)
  end
  return score
end

function M.rank_skills(skills, focus_path, options)
  local ranked = {}
  for name, skill in pairs(skills) do
    ranked[#ranked + 1] = {
      name = name,
      skill = skill,
      score = M.score_skill(skill, focus_path, options),
    }
  end
  table.sort(ranked, function(a, b)
    if a.score ~= b.score then
      return a.score > b.score
    end
    return a.name < b.name
  end)
  return ranked
end

function M.format_structured_plan(steps)
  local lines = {}
  for index, step in ipairs(steps) do
    local line = string.format("%d. %s", index, step.name)
    if step.section then
      line = line .. " (section: " .. step.section .. ")"
    end
    if step.tools and #step.tools > 0 then
      line = line .. " [tools: " .. table.concat(step.tools, ", ") .. "]"
    end
    lines[#lines + 1] = line
  end
  return table.concat(lines, "\n")
end

function M.build_plan(skill, body)
  if skill and skill.steps and #skill.steps > 0 then
    return M.format_structured_plan(skill.steps), nil
  end
  if not body then
    return nil, "skill body unavailable"
  end
  return M.extract_plan_from_body(body)
end

function M.extract_plan_from_body(body)
  local lines = {}
  local seen = {}
  for _, line in ipairs(n00n.split(body, "\n")) do
    local section = line:match("^##%s+(.+)$")
    if section and not seen[section] then
      seen[section] = true
      lines[#lines + 1] = "- " .. section
    else
      local step = line:match("^%d+%.%s+(.+)$")
      if step and not seen[step] then
        seen[step] = true
        lines[#lines + 1] = "- " .. step
      end
    end
  end
  if #lines == 0 then
    return nil, "no plan sections found"
  end
  return table.concat(lines, "\n"), nil
end

function M.extract_plan(body)
  return M.extract_plan_from_body(body)
end

function M.build_skill_list(skills, ranked)
  local sorted = {}
  if ranked then
    for _, entry in ipairs(ranked) do
      sorted[#sorted + 1] = entry.skill
    end
  else
    for _, s in pairs(skills) do
      sorted[#sorted + 1] = s
    end
    table.sort(sorted, function(a, b)
      return a.name < b.name
    end)
  end

  if #sorted == 0 then
    return "\n\n<available_skills>\nNo skills available.\n</available_skills>"
  end

  local function format_entry(s, score)
    local desc = s.description
    if s.manual_only then
      desc = "[manual-only] " .. desc
    end
    if #desc > 120 then
      desc = desc:sub(1, 117) .. "..."
    end
    local prefix = ""
    if score and score > 0 then
      prefix = string.format("(%d) ", score)
    end
    return "- " .. prefix .. s.name .. ": " .. desc .. M.tool_policy_hint(s)
  end

  local lines = {}
  if ranked then
    for _, entry in ipairs(ranked) do
      lines[#lines + 1] = format_entry(entry.skill, entry.score)
    end
  else
    for _, s in ipairs(sorted) do
      lines[#lines + 1] = format_entry(s, nil)
    end
  end
  return "\n\n<available_skills>\n" .. table.concat(lines, "\n") .. "\n</available_skills>"
end

function M.build_skill_names(skills, include_manual_only)
  local names = {}
  for _, s in pairs(skills) do
    if include_manual_only or not s.manual_only then
      names[#names + 1] = s.name
    end
  end
  table.sort(names)

  if #names == 0 then
    return ""
  end
  return "\n\nAvailable skills: " .. table.concat(names, ", ")
end

return M
