local M = {}

local SKIP_TAGS = { script = true, style = true, noscript = true }
local LT, GT, SLASH, SPACE, TAB, CR, LF = 60, 62, 47, 32, 9, 13, 10

local function is_whitespace(byte)
  return byte == SPACE or byte == TAB or byte == CR or byte == LF
end

--- Convert HTML to compact text while omitting script, style, and noscript content.
function M.strip(html)
  local output = {}
  local in_tag = false
  local tag_start = 0
  local skip_tag = nil
  local last_was_space = true

  for index = 1, #html do
    local byte = html:byte(index)
    if byte == LT then
      in_tag = true
      tag_start = index + 1
    elseif byte == GT and in_tag then
      in_tag = false
      local tag = html:sub(tag_start, index - 1):lower():match("^%s*(%S+)")
      if skip_tag then
        if tag and tag:byte(1) == SLASH and tag:sub(2) == skip_tag then
          skip_tag = nil
        end
      elseif tag and SKIP_TAGS[tag] then
        skip_tag = tag
      end

      if not skip_tag and #output > 0 and not last_was_space then
        output[#output + 1] = " "
        last_was_space = true
      end
    elseif not in_tag and not skip_tag then
      if is_whitespace(byte) then
        if not last_was_space and #output > 0 then
          output[#output + 1] = " "
          last_was_space = true
        end
      else
        output[#output + 1] = html:sub(index, index)
        last_was_space = false
      end
    end
  end

  return table.concat(output):match("^%s*(.-)%s*$")
end

return M
