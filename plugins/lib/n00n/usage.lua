local M = {}

local MAX_SAFE_INTEGER = 9007199254740991

local TOKEN_FIELDS = {
  "fresh_input_tokens",
  "cache_read_tokens",
  "cache_write_tokens",
  "input_tokens",
  "output_tokens",
}

local function checked_cost(value)
  if value == nil then
    return nil, "usage pricing returned no cost"
  end
  if type(value) ~= "number" or value ~= value or value == math.huge or value == -math.huge or value < 0 then
    return nil, "cost must be a finite non-negative number"
  end
  return value
end

local function optional_token_count(result, field)
  local value = result[field]
  if value == nil then
    return nil
  end
  if
    type(value) ~= "number"
    or value ~= value
    or value == math.huge
    or value == -math.huge
    or value < 0
    or value > MAX_SAFE_INTEGER
    or value % 1 ~= 0
  then
    return nil, field .. " must be a finite non-negative integer"
  end
  return value
end

function M.normalize(result)
  if result == nil then
    result = {}
  elseif type(result) ~= "table" then
    return nil, "usage must be a table"
  end

  local values = {}
  for _, field in ipairs(TOKEN_FIELDS) do
    local value, err = optional_token_count(result, field)
    if err then
      return nil, err
    end
    values[field] = value
  end

  local cache_read = values.cache_read_tokens or 0
  local cache_write = values.cache_write_tokens or 0
  local cached = cache_read + cache_write
  if cached > MAX_SAFE_INTEGER then
    return nil, "cached input token categories exceed the safe integer range"
  end
  local fresh_input = values.fresh_input_tokens
  local input = values.input_tokens
  if fresh_input == nil and input == nil then
    fresh_input = 0
    input = cached
  elseif fresh_input == nil then
    fresh_input = input - cached
    if fresh_input < 0 then
      return nil, "input token categories do not conserve input_tokens"
    end
  elseif input == nil then
    input = fresh_input + cached
  elseif fresh_input + cached ~= input then
    return nil, "input token categories do not conserve input_tokens"
  end
  if fresh_input > MAX_SAFE_INTEGER or input > MAX_SAFE_INTEGER then
    return nil, "input token categories exceed the safe integer range"
  end

  return {
    fresh_input_tokens = fresh_input,
    cache_read_tokens = cache_read,
    cache_write_tokens = cache_write,
    input_tokens = input,
    output_tokens = values.output_tokens or 0,
  }
end

function M.add(total, value)
  local normalized_total, total_err = M.normalize(total)
  if total_err then
    return nil, total_err
  end
  local normalized_value, value_err = M.normalize(value)
  if value_err then
    return nil, value_err
  end
  return M.normalize({
    fresh_input_tokens = normalized_total.fresh_input_tokens + normalized_value.fresh_input_tokens,
    cache_read_tokens = normalized_total.cache_read_tokens + normalized_value.cache_read_tokens,
    cache_write_tokens = normalized_total.cache_write_tokens + normalized_value.cache_write_tokens,
    output_tokens = normalized_total.output_tokens + normalized_value.output_tokens,
  })
end

function M.price(model_spec, result)
  local measured, usage_err = M.normalize(result)
  if usage_err then
    return nil, nil, usage_err
  end
  if result == nil then
    return measured, 0.0, nil
  end
  if result.cost ~= nil then
    local cost, cost_err = checked_cost(result.cost)
    return measured, cost, cost_err
  end

  local cost, cost_err = n00n.agent.usage_cost(model_spec, measured.input_tokens, measured.output_tokens, {
    fresh_input_tokens = measured.fresh_input_tokens,
    cache_read_tokens = measured.cache_read_tokens,
    cache_write_tokens = measured.cache_write_tokens,
    fast = result.fast == true,
  })
  if cost_err then
    return measured, nil, cost_err
  end
  local checked, checked_err = checked_cost(cost)
  return measured, checked, checked_err
end

return M
