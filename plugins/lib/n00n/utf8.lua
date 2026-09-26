-- UTF-8 boundary helpers for byte-budgeted strings.
local M = {}

--- Longest prefix of {s} that is at most {max_bytes} bytes and still valid
--- UTF-8. Cutting mid-sequence turns the whole string into invalid data:
--- JSON encoding and tool-result conversion reject it instead of truncating.
--- An invalid byte anywhere in {s} is also cut away, so the result is always
--- valid UTF-8 even when the caller supplies malformed input.
function M.prefix(s, max_bytes)
  if max_bytes <= 0 then
    return ""
  end

  local valid_len, invalid_at = utf8.len(s)
  local valid_bytes = valid_len and #s or (invalid_at - 1)

  if max_bytes >= valid_bytes then
    return s:sub(1, valid_bytes)
  end

  local cut = utf8.offset(s, 0, max_bytes + 1)
  if not cut or cut <= 1 then
    return ""
  end
  return s:sub(1, cut - 1)
end

return M
