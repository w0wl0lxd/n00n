local shorten_path = require("n00n.shorten_path")
local secret_check = require("n00n.secret_check")
local ToolView = require("n00n.tool_view")

local DESCRIPTION = [[Write content to a file. Prefer edit_file or edit_file_lines for existing files.]]
local DIFF_SNAPSHOT_MAX_BYTES = 1024 * 1024

local function write_view_opts(ctx)
  local tol = ctx:tool_output_lines()
  return { max_lines = (tol and tol.write) or 10, keep = "head" }
end

local function build_view(content, path, ctx)
  local buf = n00n.ui.buf()
  local view = ToolView.new(buf, write_view_opts(ctx))
  view:set_highlight(content, path:match("%.([^%.]+)$") or "")
  view:finish()
  buf:on("click", function()
    view:toggle()
  end)
  return buf
end

local function snapshot_before(path)
  local metadata, metadata_err = n00n.fs.metadata(path)
  if not metadata then
    if metadata_err then
      return nil, "metadata error: " .. tostring(metadata_err)
    end
    return ""
  end
  if not metadata.is_file then
    return nil, nil, "file is not a regular file"
  end
  if metadata.size > DIFF_SNAPSHOT_MAX_BYTES then
    return nil, nil, "file exceeds maximum size"
  end

  -- A read failure only costs the diff. The handler wrote without reading the
  -- target before this tool showed diffs, so a file whose content is
  -- unreadable must stay overwritable when its directory is writable.
  local ok, existing, read_err = pcall(n00n.fs.read, path)
  if not ok then
    local message = tostring(existing)
    local lower = message:lower()
    if lower:find("utf-8", 1, true) or lower:find("utf8", 1, true) then
      return nil, nil, "existing file is not UTF-8"
    end
    return nil, nil, "cannot read the existing file: " .. message
  end
  if read_err then
    return nil, nil, "cannot read the existing file: " .. tostring(read_err)
  end
  if existing:find("[%z\1-\8\11\12\14-\31]") then
    return nil, nil, "existing file is binary or non-text"
  end
  return existing
end

n00n.api.register_tool({
  name = "write_file",
  aliases = { "write" },
  kind = "edit",
  mutable_path = "path",
  permission_scopes = "path",
  audiences = { "main", "general_sub", "interpreter" },
  modes = { "default", "build" },
  description = DESCRIPTION,

  schema = {
    type = "object",
    properties = {
      path = {
        type = "string",
        required = true,
        alias = "file_path",
      },
      content = {
        type = "string",
        required = true,
      },
      justification = {
        type = "string",
        description = "Required when content may contain secrets/PII. Explain why this content is safe to write.",
      },
    },
  },

  header = function(input)
    local buf = n00n.ui.buf()
    buf:line({ { shorten_path(input.path or ""), "path" } })
    return buf
  end,

  restore = function(input, output, _is_error, ctx)
    local content = input.content or ""
    if content == "" then
      return ToolView.restore(output, write_view_opts(ctx))
    end
    return build_view(content, input.path or "", ctx)
  end,

  handler = function(input, ctx)
    local raw = input.path
    if not raw then
      return { llm_output = "error: path is required", is_error = true }
    end
    local content = input.content
    if not content then
      return { llm_output = "error: content is required", is_error = true }
    end

    local secret_reason = secret_check.reason(content)
    if secret_reason and (not input.justification or input.justification:match("^%s*$")) then
      return { llm_output = "error: " .. secret_reason .. "; provide justification to write", is_error = true }
    end

    local path = n00n.fs.abspath(raw)

    local ok, err = ctx:check_before_edit(path)
    if not ok then
      return { llm_output = err, is_error = true }
    end
    if ctx:cancelled() then
      return { llm_output = "cancelled", is_error = true }
    end

    local before, snapshot_err, diff_fallback = snapshot_before(path)
    if snapshot_err then
      return { llm_output = snapshot_err, is_error = true }
    end
    if ctx:cancelled() then
      return { llm_output = "cancelled", is_error = true }
    end

    local parent = n00n.fs.dirname(path)
    if parent then
      n00n.fs.mkdir(parent, { parents = true })
    end

    local _, write_err = n00n.fs.write(path, content)
    if write_err then
      return { llm_output = "write error: " .. tostring(write_err), is_error = true }
    end

    ctx:record_read(path)

    local byte_count = #content
    local rel = shorten_path(path)
    local llm_output = string.format("wrote %d bytes to %s", byte_count, rel)
    local annotation = string.format("%d bytes", byte_count)

    if diff_fallback then
      return {
        llm_output = llm_output .. "; diff unavailable: " .. diff_fallback,
        body = build_view(content, path, ctx),
        annotation = annotation,
        written_path = path,
      }
    end

    return {
      llm_output = llm_output,
      diff_path = path,
      diff_before = before,
      diff_after = content,
      annotation = annotation,
      written_path = path,
    }
  end,
})
