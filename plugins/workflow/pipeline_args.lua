-- Argument normalization for pipeline(). Split out so the spec can exercise it
-- without loading init.lua, which registers a tool on load.
local M = {}

-- Returns the stage array and the opts table, or nil plus an error message.
function M.normalize(stages, ...)
  if type(stages) ~= "function" then
    return stages, select(1, ...), nil
  end

  local collected = { stages }
  local extra_count = select("#", ...)
  local opts
  for i = 1, extra_count do
    local extra = select(i, ...)
    if type(extra) == "function" then
      collected[#collected + 1] = extra
    elseif i == extra_count and (type(extra) == "table" or extra == nil) then
      opts = extra
    else
      return nil, nil, "pipeline: stages must be functions, got " .. type(extra) .. " at argument " .. (i + 2)
    end
  end
  return collected, opts, nil
end

return M
