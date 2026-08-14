local NO_RESULTS_MSG = "No search results found"
local BACKEND_ERROR_MSG = "Exa backend returned an error"

local function extract_text(parsed)
  local content = parsed.result and parsed.result.content
  if type(content) ~= "table" then
    return nil
  end
  for _, item in ipairs(content) do
    if type(item) == "table" and type(item.text) == "string" and #item.text > 0 then
      return item.text
    end
  end
end

local function decode_json(data, label)
  local parsed, parse_err = n00n.json.decode(data)
  if not parsed then
    return nil, label .. " parse error: " .. tostring(parse_err)
  end
  if type(parsed) ~= "table" then
    return nil, label .. " response must be a JSON object"
  end
  if parsed.error ~= nil or (type(parsed.result) == "table" and parsed.result.isError == true) then
    return nil, BACKEND_ERROR_MSG
  end
  return extract_text(parsed) or NO_RESULTS_MSG
end

local function parse_sse(body)
  for line in body:gmatch("[^\r\n]+") do
    local data = line:match("^data:%s*(.+)")
    if data then
      local text, parse_err = decode_json(data, "SSE JSON")
      if not text then
        return nil, parse_err
      end
      if text ~= NO_RESULTS_MSG then
        return text
      end
    end
  end
  return NO_RESULTS_MSG
end

local function parse_response(body, content_type)
  local media_type = tostring(content_type or ""):lower()
  local trimmed = body:match("^%s*(.-)%s*$")
  if trimmed == "" then
    return NO_RESULTS_MSG
  end
  if media_type:find("application/json", 1, true) or trimmed:match("^[%{%[]") then
    return decode_json(trimmed, "JSON-RPC")
  end
  if media_type:find("text/event-stream", 1, true) or body:match("^%s*data:") or body:find("\ndata:", 1, true) then
    return parse_sse(body)
  end
  return nil, "unsupported Exa response"
end

return parse_response
