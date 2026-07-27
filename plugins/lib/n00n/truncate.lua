local function truncate(text, max_lines, max_bytes)
  if #text == 0 then
    return ""
  end

  if #text <= max_bytes then
    local _, n = text:gsub("\n", "")
    if n + 1 <= max_lines then
      return text
    end
  end

  local out = {}
  local bytes = 0
  local lines = 0
  local pos = 1
  local text_len = #text

  while pos <= text_len and lines < max_lines do
    local nl = text:find("\n", pos, true)
    local line_end = nl and (nl - 1) or text_len
    local line = text:sub(pos, line_end)
    if line:sub(-1) == "\r" then
      line = line:sub(1, -2)
    end

    local sep_len = #out > 0 and 1 or 0
    local remaining = max_bytes - bytes - sep_len
    if remaining < 3 then
      break
    end

    if #line <= remaining then
      out[#out + 1] = line
      bytes = bytes + sep_len + #line
      lines = lines + 1
      pos = nl and (nl + 1) or (text_len + 1)
    else
      local content_limit = remaining - 3
      if content_limit > 0 then
        local cut = utf8.offset(line, 0, content_limit + 1)
        if cut and cut > 1 then
          out[#out + 1] = line:sub(1, cut - 1) .. "..."
          bytes = bytes + sep_len + (cut - 1) + 3
        else
          out[#out + 1] = "..."
          bytes = bytes + sep_len + 3
        end
      else
        out[#out + 1] = "..."
        bytes = bytes + sep_len + 3
      end
      lines = lines + 1
      break
    end
  end

  local result = table.concat(out, "\n")
  if #result < #text then
    if #result > 0 then
      result = result .. "\n\n[truncated " .. (#text - #result) .. " bytes]"
    else
      result = "[truncated " .. #text .. " bytes]"
    end
  end
  return result
end

return truncate
