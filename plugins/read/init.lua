local ToolView = require("n00n.tool_view")
local shorten_path = require("n00n.shorten_path")
local output_limits = require("n00n.output_limits")

local DESCRIPTION = "Read a file or directory. Returns contents with line numbers (1-indexed)."

local DEFAULT_MAX_OUTPUT_LINES = 500

local opts = n00n.api.register_options({
  max_line_bytes = { default = 500, min = 80, desc = "Truncate lines longer than this many bytes." },
  max_output_lines = output_limits.specs.max_output_lines,
})

local function line_nr_fmt(count)
  return "%" .. math.max(1, #tostring(count)) .. "d "
end

local function truncate_bytes(line, max_bytes)
  if #line <= max_bytes then
    return line
  end
  local cut = utf8.offset(line, 0, max_bytes + 1)
  if not cut or cut <= 1 then
    return "..."
  end
  return line:sub(1, cut - 1) .. "..."
end

local function read_view_opts(ctx)
  local tol = ctx:tool_output_lines()
  return { max_lines = (tol and tol.read) or 10, keep = "head" }
end

local function apply_highlights(view, lines, ext, prefix)
  local opts = prefix and { prefix = prefix } or nil
  local highlighted = n00n.ui.highlight(table.concat(lines, "\n"), ext, opts)
  if not highlighted then
    return
  end
  for i, hl_spans in ipairs(highlighted) do
    local plain = view.all_lines[i]
    if not plain then
      break
    end
    view:update_line(i, { plain[1], table.unpack(hl_spans) })
  end
  view:flush()
end

local function build_file_view(lines, start_line, total_lines, path, ctx, prefix)
  local buf = n00n.ui.buf()
  local view = ToolView.new(buf, read_view_opts(ctx))
  local nr_fmt = line_nr_fmt(total_lines)

  for i, line in ipairs(lines) do
    view:append({ { string.format(nr_fmt, start_line + i - 1), "line_nr" }, { line } })
  end

  local trunc_start = start_line + #lines
  if trunc_start <= total_lines then
    view:append({
      {
        string.format(
          "... Truncated %d lines. Use offset=%d to read further.",
          total_lines - trunc_start + 1,
          trunc_start
        ),
        "dim",
      },
    })
  end

  view:finish()

  local ext = path:match("%.([^%.]+)$") or ""
  n00n.async.run(function()
    apply_highlights(view, lines, ext, prefix)
  end)

  buf:on("click", function()
    view:toggle()
  end)
  return buf
end

local function build_dir_view(text, ctx)
  local buf = n00n.ui.buf()
  local view = ToolView.new(buf, read_view_opts(ctx))
  view:append_text(text)
  view:finish()
  buf:on("click", function()
    view:toggle()
  end)
  return buf
end

local function read_file(path, offset, limit, ctx)
  local start = math.max(offset or 1, 1)
  local max_lines = limit or opts.max_output_lines or ctx:config("max_output_lines", DEFAULT_MAX_OUTPUT_LINES)
  local max_line_bytes = opts.max_line_bytes

  local res, err = n00n.fs.read_lines(path, start, max_lines)
  if not res then
    return { llm_output = "read error: " .. tostring(err), is_error = true }
  end

  local lines = {}
  for _, line in ipairs(res.lines) do
    lines[#lines + 1] = truncate_bytes(line, max_line_bytes)
  end

  ctx:record_read(path)

  local parts = {}
  for i, line in ipairs(lines) do
    parts[#parts + 1] = (start + i - 1) .. ": " .. line
  end
  local llm_output = table.concat(parts, "\n")

  local trunc_start = start + #lines
  if trunc_start <= res.total_lines then
    llm_output = llm_output
      .. string.format(
        "\n\n...\n\nTruncated lines: %d-%d. Use offset=%d to read further.",
        trunc_start,
        res.total_lines,
        trunc_start
      )
  end

  local shown = #lines
  local annotation = shown < res.total_lines and string.format("%d of %d lines", shown, res.total_lines)
    or string.format("%d lines", shown)

  local basename = path:match("([^/]+)$")
  if not ctx:is_instruction_file(basename) then
    local parent = n00n.fs.dirname(path)
    if parent then
      local instructions = ctx:find_instructions(parent)
      if #instructions > 0 then
        return {
          llm_output = llm_output,
          body = build_file_view(lines, start, res.total_lines, path, ctx, res.prefix),
          annotation = annotation,
          instructions = instructions,
        }
      end
    end
  end

  return {
    llm_output = llm_output,
    body = build_file_view(lines, start, res.total_lines, path, ctx, res.prefix),
    annotation = annotation,
  }
end

local function list_dir(path, ctx)
  local entries, err = n00n.fs.dir(path)
  if not entries then
    return { llm_output = "read error: " .. tostring(err), is_error = true }
  end

  local sorted = {}
  for _, entry in ipairs(entries) do
    local name, typ = entry[1], entry[2]
    if typ == "directory" then
      sorted[#sorted + 1] = { name .. "/", true }
    elseif not ctx:is_instruction_file(name) then
      sorted[#sorted + 1] = { name, false }
    end
  end
  table.sort(sorted, function(a, b)
    if a[2] ~= b[2] then
      return a[2]
    end
    return a[1] < b[1]
  end)

  local names = {}
  for _, e in ipairs(sorted) do
    names[#names + 1] = e[1]
  end
  local text = table.concat(names, "\n")

  local instructions = ctx:find_instructions(path)
  local result = {
    llm_output = text,
    body = build_dir_view(text, ctx),
    annotation = #sorted .. " entries",
  }
  if #instructions > 0 then
    result.instructions = instructions
  end
  return result
end

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = [[- When using the **read** tool, only read the sections you actually need.
- Use `wc -l` to check total number of lines before reading to decide a reasonable **read** tool limit unless known already.
- Supports absolute, relative, and ~/ paths. No offset = start at 1; no limit = up to 500 lines.
- Use truncation hints (e.g. "truncated lines X-Y") to continue with the correct offset.]],
})

n00n.api.register_tool({
  name = "read",
  kind = "read",
  workload = "cheap",
  modes = { "default", "research", "build", "compact" },
  description = DESCRIPTION,
  strict = true,

  schema = {
    type = "object",
    additionalProperties = false,
    required = { "path", "offset", "limit" },
    properties = {
      path = {
        type = "string",
        required = true,
        alias = "file_path",
        description = "File or directory path (absolute, relative, or ~/)",
      },
      offset = {
        type = { "integer", "null" },
        required = true,
        description = "Starting line number (1-indexed, default 1)",
      },
      limit = {
        type = { "integer", "null" },
        required = true,
        description = "Maximum number of lines to read (default 500)",
      },
    },
  },

  header = function(input)
    local buf = n00n.ui.buf()
    local s = shorten_path(input.path or "")
    local start = input.offset or 1
    if input.limit then
      s = s .. ":" .. start .. "-" .. (start + input.limit - 1)
    elseif input.offset then
      s = s .. ":" .. start
    end
    buf:line({ { s, "path" } })
    return buf
  end,

  restore = function(input, output, _is_error, ctx)
    local lines, start_line, total_lines = {}, nil, nil
    for _, raw in ipairs(n00n.split(output, "\n")) do
      local nr, text = raw:match("^%s*(%d+): (.*)$")
      if nr then
        start_line = start_line or tonumber(nr)
        lines[#lines + 1] = text
      else
        local trunc_end = raw:match("Truncated lines: %d+%-(%d+)")
        if trunc_end then
          total_lines = tonumber(trunc_end)
        end
      end
    end
    if #lines == 0 then
      return ToolView.restore(output, read_view_opts(ctx))
    end
    start_line = start_line or 1
    total_lines = total_lines or (start_line + #lines - 1)
    return build_file_view(lines, start_line, total_lines, input.path or "", ctx)
  end,

  handler = function(input, ctx)
    local raw = input.path
    if not raw then
      return { llm_output = "error: path is required", is_error = true }
    end
    local path = n00n.fs.abspath(raw)
    local meta = n00n.fs.metadata(path)
    if not meta then
      return { llm_output = "error: path not found: " .. path, is_error = true }
    end
    if meta.is_dir then
      return list_dir(path, ctx)
    end
    return read_file(path, input.offset, input.limit, ctx)
  end,
})

-- Tests
do
  local function test_utf8_truncate()
    local s = "abc🎉xyz"
    local max_bytes = 4
    local result = truncate_bytes(s, max_bytes)
    -- Emoji is 4 bytes, so with max_bytes=4 we should get "abc..." (emoji doesn't fit)
    assert(result:sub(1, 3) == "abc", "should preserve ASCII prefix")
    assert(result:sub(-3) == "...", "should add ellipsis")
    -- Test with enough bytes for the emoji
    local result2 = truncate_bytes(s, 7)
    assert(result2:sub(1, 7) == "abc🎉", "should include full emoji when it fits")
  end

  test_utf8_truncate()
end
