local M = {}

local function empty_policies()
  return { version = 1, rules = {} }
end

local function validate_array(values, label, validate_value)
  local count = 0
  for key in pairs(values) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 then
      return nil, label .. " must be an array"
    end
    count = count + 1
  end
  for index = 1, count do
    local value = rawget(values, index)
    if value == nil then
      return nil, label .. " must not be sparse"
    end
    if validate_value and not validate_value(value) then
      return nil, label .. " entries must be non-empty strings"
    end
  end
  return true
end

local function validate_policies(policies)
  if type(policies) ~= "table" or type(policies.rules) ~= "table" then
    return nil, "policy document must be an object with a rules array"
  end
  local rules_ok, rules_err = validate_array(policies.rules, "policy rules")
  if not rules_ok then
    return nil, rules_err
  end
  for index, rule in ipairs(policies.rules) do
    if type(rule) ~= "table" then
      return nil, "policy rule " .. index .. " must be an object"
    end
    if rule.scope ~= nil then
      if type(rule.scope) ~= "table" then
        return nil, "policy rule " .. index .. " scope must be an object"
      end
      for _, field in ipairs({ "agent_id", "session_type", "tag" }) do
        if rule.scope[field] ~= nil and type(rule.scope[field]) ~= "string" then
          return nil, "policy rule " .. index .. " scope." .. field .. " must be a string"
        end
      end
    end
    if rule.priority ~= nil and type(rule.priority) ~= "number" then
      return nil, "policy rule " .. index .. " priority must be a number"
    end
    if rule.restricted_tools ~= nil then
      if type(rule.restricted_tools) ~= "table" then
        return nil, "policy rule " .. index .. " restricted_tools must be an array"
      end
      local tools_ok, tools_err = validate_array(
        rule.restricted_tools,
        "policy rule " .. index .. " restricted_tools",
        function(value)
          return type(value) == "string" and value ~= ""
        end
      )
      if not tools_ok then
        return nil, tools_err
      end
    end
    if rule.allowed_tools ~= nil then
      if type(rule.allowed_tools) ~= "table" then
        return nil, "policy rule " .. index .. " allowed_tools must be an array"
      end
      local tools_ok, tools_err = validate_array(
        rule.allowed_tools,
        "policy rule " .. index .. " allowed_tools",
        function(value)
          return type(value) == "string" and value ~= ""
        end
      )
      if not tools_ok then
        return nil, tools_err
      end
    end
  end
  return policies
end

function M.load(path)
  if type(path) ~= "string" or path == "" then
    return nil, "cannot resolve policy file path"
  end

  local metadata, metadata_err = n00n.fs.metadata(path)
  if not metadata then
    if metadata_err then
      return nil, "failed to inspect policy file: " .. tostring(metadata_err)
    end
    return empty_policies()
  end
  if not metadata.is_file then
    return nil, "policy path is not a file"
  end

  local content, read_err = n00n.fs.read(path)
  if type(content) ~= "string" then
    return nil, "failed to read policy file: " .. tostring(read_err)
  end
  local ok, decoded, decode_err = pcall(n00n.json.decode, content)
  if not ok then
    return nil, "invalid policy JSON: " .. tostring(decoded)
  end
  if not decoded then
    return nil, "invalid policy JSON: " .. tostring(decode_err)
  end
  local policies, validation_err = validate_policies(decoded)
  if not policies then
    return nil, "invalid policy JSON: " .. validation_err
  end
  return policies
end

return M
