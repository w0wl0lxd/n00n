-- UTF-8 boundary helpers for byte-budgeted strings.
local M = {}

--- Longest prefix of {s} that is at most {max_bytes} bytes and still valid
--- UTF-8. Cutting mid-sequence turns the whole string into invalid data:
--- JSON encoding and tool-result conversion reject it instead of truncating.
function M.prefix(s, max_bytes)
  if max_bytes <= 0 then
    return ""
  end
  if max_bytes >= #s then
    return s
  end
  local cut = utf8.offset(s, 0, max_bytes + 1)
  if not cut or cut <= 1 then
    return ""
  end
  return s:sub(1, cut - 1)
end

return M
