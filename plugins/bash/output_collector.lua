-- Streaming accumulator for bash's LLM-facing output. The bash tool streams
-- stdout/stderr line by line as the process runs, so the accumulator itself
-- must stay bounded; unlike tools that collect a complete result and
-- truncate once (see plugins/grep, plugins/batch), a runaway command here
-- can emit output far longer than any output cap before the process exits.

local truncate = require("n00n.truncate")

local M = {}

-- Extra bytes kept beyond `max_bytes` so `truncate` still has a full line to
-- work with when deciding where to cut and insert "...".
local TRUNCATE_CAPTURE_SLACK_BYTES = 4096

function M.new()
  return { parts = {}, stored_bytes = 0, total_bytes = 0, line_count = 0, capped = false }
end

-- Streams `line` into `collector`, capping stored bytes at roughly
-- `max_bytes` so a huge command output cannot grow the accumulator without
-- bound. Bytes beyond the cap are still counted (not stored) so the final
-- truncation marker stays byte-accurate.
function M.append_line(collector, line, max_lines, max_bytes)
  local sep_bytes = collector.line_count > 0 and 1 or 0
  collector.total_bytes = collector.total_bytes + sep_bytes + #line
  collector.line_count = collector.line_count + 1

  if collector.capped then
    return
  end

  local capacity = max_bytes + TRUNCATE_CAPTURE_SLACK_BYTES - collector.stored_bytes - sep_bytes
  if capacity <= 0 or collector.line_count > max_lines then
    collector.capped = true
    return
  end

  if sep_bytes > 0 then
    collector.parts[#collector.parts + 1] = "\n"
  end
  if #line > capacity then
    collector.parts[#collector.parts + 1] = line:sub(1, capacity)
    collector.stored_bytes = collector.stored_bytes + sep_bytes + capacity
    collector.capped = true
  else
    collector.parts[#collector.parts + 1] = line
    collector.stored_bytes = collector.stored_bytes + sep_bytes + #line
  end
end

function M.should_flush(collector, last_flush, now, max_lines, max_secs)
  return collector.line_count <= 2 or collector.line_count % max_lines == 0 or now - last_flush >= max_secs
end

function M.collected_output(collector, max_lines, max_bytes)
  local captured = table.concat(collector.parts)
  local output = truncate(captured, max_lines, max_bytes)
  local extra = collector.total_bytes - #captured
  if extra <= 0 then
    return output
  end

  local with_marker, replaced = output:gsub("(%[truncated )(%d+)( bytes%])", function(pre, count, post)
    return pre .. (tonumber(count) + extra) .. post
  end)
  if replaced > 0 then
    return with_marker
  end
  if output == "" then
    return "[truncated " .. extra .. " bytes]"
  end
  return output .. "\n\n[truncated " .. extra .. " bytes]"
end

return M
