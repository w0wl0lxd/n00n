local ToolView = require("n00n.tool_view")
local helpers = require("memory_helpers")
local ListPicker = require("n00n.list_picker")

local load_entries = helpers.load_entries
local rank_memories = helpers.rank_memories
local format_list = helpers.format_list
local build_frontmatter = helpers.build_frontmatter
local normalize_metadata = helpers.normalize_metadata
local merge_metadata = helpers.merge_metadata
local append_body = helpers.append_body
local build_lite_hint = helpers.build_lite_hint
local tags_match = helpers.tags_match

local function memories_path_suffix()
  local cwd = n00n.uv.cwd()
  local root = n00n.fs.root(cwd, ".git") or cwd
  return "projects/" .. helpers.project_id(root) .. "/memories"
end

local function resolve_dir()
  local state = n00n.env.state_dir()
  if not state then
    return nil, "cannot resolve state dir"
  end
  return n00n.fs.joinpath(state, memories_path_suffix())
end

local function parse_tag_filter(tags)
  if type(tags) ~= "string" or tags == "" then
    return nil
  end
  return helpers.normalize_string_list(tags)
end

local function filter_entries(entries, tag_filter)
  if not tag_filter then
    return entries
  end
  local filtered = {}
  for _, entry in ipairs(entries) do
    if tags_match(entry.meta or {}, tag_filter) then
      filtered[#filtered + 1] = entry
    end
  end
  return filtered
end

local function clamp_limit(limit)
  if type(limit) ~= "number" then
    return helpers.DEFAULT_SEARCH_LIMIT
  end
  local rounded = math.floor(limit)
  if rounded < 1 then
    return 1
  end
  if rounded > helpers.MAX_SEARCH_LIMIT then
    return helpers.MAX_SEARCH_LIMIT
  end
  return rounded
end

n00n.api.register_prompt_hint({
  prompt = "system",
  slot = "after_instructions",
  content = function()
    local dir = resolve_dir()
    if not dir then
      return nil
    end
    local entries = load_entries(dir)
    if #entries == 0 then
      return nil
    end
    local lite = build_lite_hint(entries)
    if lite then
      return lite
    end
    return "\n\nMemory files: " .. #entries .. " entries (use memory tool to view/search/update)\n"
  end,
})

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Proactively save non-obvious project gotchas and architecture decisions to **memory**. Use `search` for keyword/tag recall (not semantic paraphrase).",
})

local function render_content(content, path, ctx)
  local buf = n00n.ui.buf()
  local tol = ctx:tool_output_lines()
  local view = ToolView.new(buf, {
    max_lines = (tol and tol.other) or 20,
    keep = "head",
  })
  buf:on("click", function()
    view:toggle()
  end)

  local ext = path:match("%.([^%.]+)$") or "md"
  if not view:set_highlight(content, ext) then
    view:append_text(content)
  end
  view:finish()
  return buf
end

local function cmd_view(path, query, focus_path, dir, ctx)
  if not path then
    local entries = load_entries(dir)
    if query and #query > 0 then
      local ranked = rank_memories(entries, query, focus_path)
      return { llm_output = format_list(entries, ranked) }
    end
    return { llm_output = format_list(entries, nil) }
  end
  local file_path, err = helpers.safe_resolve(dir, path)
  if not file_path then
    return nil, err
  end
  local content, read_err = n00n.fs.read(file_path)
  if not content then
    return nil, "read error: " .. tostring(read_err)
  end
  local entry, parse_err = helpers.parse_memory_file(path, content)
  if not entry then
    return nil, parse_err
  end
  return {
    llm_output = entry.body,
    body = render_content(entry.body, path, ctx),
  }
end

local function cmd_search(query, tags, focus_path, limit, dir)
  if not query or #query == 0 then
    return nil, "query is required for search"
  end
  local entries = filter_entries(load_entries(dir), parse_tag_filter(tags))
  local ranked = rank_memories(entries, query, focus_path)
  local capped = {}
  local max = clamp_limit(limit)
  for i = 1, math.min(#ranked, max) do
    capped[#capped + 1] = ranked[i]
  end
  if #capped == 0 then
    return { llm_output = "No matching memories for query: " .. query }
  end
  local lines = { "Search results (keyword/tag match):" }
  for _, row in ipairs(capped) do
    lines[#lines + 1] = helpers.format_search_result(row.entry, row.score)
  end
  return { llm_output = table.concat(lines, "\n") }
end

local function cmd_write(path, content, metadata, dir, ctx, input)
  local file_path, err = helpers.safe_resolve(dir, path)
  if not file_path then
    return nil, err
  end
  local meta = n00n.fs.metadata(file_path)
  local existing_size = meta and meta.size or 0
  if meta then
    local text, read_err = n00n.fs.read(file_path)
    if not text then
      return nil, "read error: " .. tostring(read_err)
    end
    local existing_fm, parse_err = helpers.parse_frontmatter(text)
    if not existing_fm then
      return nil, parse_err
    end
    metadata = merge_metadata(existing_fm, metadata, input)
  end
  local payload, fm_err = build_frontmatter(metadata, content)
  if not payload then
    return nil, fm_err or "failed to build frontmatter"
  end
  local lc = helpers.count_lines(payload)
  if lc > helpers.MAX_LINES_PER_FILE then
    return nil, "content exceeds " .. helpers.MAX_LINES_PER_FILE .. " lines (" .. lc .. " lines); reduce content size"
  end
  if helpers.dir_total_bytes(dir) - existing_size + #payload > helpers.MAX_DIR_BYTES then
    return nil, "memory directory would exceed " .. helpers.MAX_DIR_BYTES .. " byte limit; delete stale entries first"
  end
  n00n.fs.mkdir(dir, { parents = true })
  local ok, write_err = n00n.fs.write(file_path, payload)
  if not ok then
    return nil, "write error: " .. tostring(write_err)
  end
  return {
    llm_output = "wrote " .. path .. " (" .. lc .. " lines)",
    body = render_content(content, path, ctx),
  }
end

local function cmd_append(path, content, dir, ctx)
  if not path then
    return nil, "'path' is required for append"
  end
  if not content then
    return nil, "'content' is required for append"
  end
  local file_path, err = helpers.safe_resolve(dir, path)
  if not file_path then
    return nil, err
  end
  local existing = ""
  local meta = n00n.fs.metadata(file_path)
  if meta then
    local text, read_err = n00n.fs.read(file_path)
    if not text then
      return nil, "read error: " .. tostring(read_err)
    end
    existing = text
  end
  local payload, fm_err = append_body(existing, content)
  if not payload then
    return nil, fm_err or "failed to build frontmatter"
  end
  local lc = helpers.count_lines(payload)
  if lc > helpers.MAX_LINES_PER_FILE then
    return nil, "content exceeds " .. helpers.MAX_LINES_PER_FILE .. " lines (" .. lc .. " lines); reduce content size"
  end
  local existing_size = meta and meta.size or 0
  if helpers.dir_total_bytes(dir) - existing_size + #payload > helpers.MAX_DIR_BYTES then
    return nil, "memory directory would exceed " .. helpers.MAX_DIR_BYTES .. " byte limit; delete stale entries first"
  end
  n00n.fs.mkdir(dir, { parents = true })
  local ok, write_err = n00n.fs.write(file_path, payload)
  if not ok then
    return nil, "write error: " .. tostring(write_err)
  end
  local entry = helpers.parse_memory_file(path, payload)
  if not entry then
    return nil, "append wrote file but failed to parse result"
  end
  return {
    llm_output = "appended to " .. path .. " (" .. helpers.count_lines(content) .. " lines added)",
    body = render_content(entry.body, path, ctx),
  }
end

local function cmd_delete(path, dir)
  local file_path, err = helpers.safe_resolve(dir, path)
  if not file_path then
    return nil, err
  end
  if not n00n.fs.metadata(file_path) then
    return nil, "'" .. path .. "' does not exist"
  end
  local ok, rm_err = n00n.fs.rm(file_path)
  if not ok then
    return nil, "delete error: " .. tostring(rm_err)
  end
  return "deleted " .. path
end

n00n.api.register_tool({
  name = "memory",
  defer_loading = true,
  namespace = "memory",
  description = "Persistent, project-scoped scratchpad for learnings, patterns, decisions, and gotchas across sessions. Save important context before compaction or to build project knowledge. Use `search` for keyword/tag recall (not semantic paraphrase). Keep entries concise and current. Delete outdated information.",

  schema = {
    type = "object",
    properties = {
      command = {
        type = "string",
        description = "Command: view, write, delete, search, append",
        required = true,
      },
      path = { type = "string", description = "Relative path (e.g. 'architecture.md'). Omit to list all." },
      content = { type = "string", description = "File content for 'write' or text to add for 'append'" },
      query = {
        type = "string",
        description = "Keyword query for 'search' or optional ranking when listing via 'view'",
      },
      tags = {
        type = "string",
        description = "Comma-separated tags metadata for 'write' and filter for 'search'",
      },
      focus_path = { type = "string", description = "Optional file path context to boost ranking" },
      limit = { type = "integer", description = "Max search results (default 10, max 50)" },
      topic = { type = "string", description = "Topic metadata for 'write'" },
      importance = { type = "integer", description = "Importance 1-5 for 'write' (default 1)" },
      layer = {
        type = "string",
        description = "Memory layer: lite or deep (default deep). Lite entries surface in session hints.",
      },
      synopsis = { type = "string", description = "One-line summary for lite layer injection" },
    },
  },

  header = function(input)
    if input.path then
      return (input.command or "") .. " " .. input.path
    end
    if input.command == "search" and input.query then
      return "search " .. input.query
    end
    return input.command
  end,

  restore = function(input, output, _is_error, ctx)
    if input.command == "append" and input.path then
      local dir = resolve_dir()
      if dir then
        local file_path, resolve_err = helpers.safe_resolve(dir, input.path)
        if file_path then
          local raw, read_err = n00n.fs.read(file_path)
          if raw then
            local entry = helpers.parse_memory_file(input.path, raw)
            return render_content(entry.body, input.path, ctx)
          end
          if read_err then
            return render_content(output, input.path, ctx)
          end
        elseif resolve_err then
          return render_content(output, input.path, ctx)
        end
      end
    end
    local content = (input.command == "write" and input.content) or output
    return render_content(content, input.path or "file.md", ctx)
  end,

  handler = function(input, ctx)
    local cmd = input.command
    local dir, dir_err = resolve_dir()
    if not dir then
      return { llm_output = "error: " .. dir_err, is_error = true }
    end

    local result, err
    if cmd == "view" then
      result, err = cmd_view(input.path, input.query, input.focus_path, dir, ctx)
    elseif cmd == "search" then
      result, err = cmd_search(input.query, input.tags, input.focus_path, input.limit, dir)
    elseif cmd == "write" then
      if not input.path then
        return { llm_output = "error: 'path' is required for write", is_error = true }
      end
      if not input.content then
        return { llm_output = "error: 'content' is required for write", is_error = true }
      end
      local metadata = normalize_metadata(input)
      result, err = cmd_write(input.path, input.content, metadata, dir, ctx, input)
    elseif cmd == "append" then
      result, err = cmd_append(input.path, input.content, dir, ctx)
    elseif cmd == "delete" then
      if not input.path then
        return { llm_output = "error: 'path' is required for delete", is_error = true }
      end
      result, err = cmd_delete(input.path, dir)
    else
      return {
        llm_output = "error: unknown command '"
          .. tostring(cmd)
          .. "'. Valid commands: view, write, delete, search, append",
        is_error = true,
      }
    end
    if err then
      return { llm_output = "error: " .. err, is_error = true }
    end
    return result
  end,
})

n00n.api.register_command({
  name = "/memory",
  description = "View, edit, and delete memory files",
  handler = function()
    local dir = resolve_dir()
    if not dir then
      n00n.ui.flash("Cannot resolve memory directory")
      return
    end

    local entries = helpers.collect_file_entries(dir)
    if #entries == 0 then
      n00n.ui.flash("No memory files yet")
      return
    end
    table.sort(entries, function(a, b)
      return a[1] < b[1]
    end)

    local function build_items()
      local items = {}
      for _, e in ipairs(entries) do
        items[#items + 1] = { label = e[1], detail = "(" .. (e[2] or 0) .. " bytes)" }
      end
      return items
    end

    local last_cursor = 1
    while true do
      local event = ListPicker.open(build_items(), {
        title = " Memory Files ",
        cursor = last_cursor,
        submit_keys = { "ctrl+o" },
        footer = {
          { "Enter", "open" },
          { "Ctrl+O", "edit" },
          { "Ctrl+D", "delete" },
        },
      })

      if event.type == "close" then
        break
      end

      last_cursor = event.index
      if event.type == "choice" then
        local item = entries[event.index]
        if item then
          local path = n00n.fs.joinpath(dir, item[1])
          local code = n00n.ui.open_editor(path)
          if code == 0 then
            local meta = n00n.fs.metadata(path)
            if meta then
              item[2] = meta.size
            end
          end
        end
      elseif event.type == "delete" then
        local item = entries[event.index]
        local ok, err = n00n.fs.rm(n00n.fs.joinpath(dir, item[1]))
        if ok then
          n00n.ui.flash("Deleted " .. item[1])
          table.remove(entries, event.index)
          if #entries == 0 then
            break
          end
          if last_cursor > #entries then
            last_cursor = #entries
          end
        else
          n00n.ui.flash("Delete failed: " .. tostring(err))
        end
      else
        break
      end
    end
  end,
})
