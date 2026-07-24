-- Wave dispatch: group plan steps by role for sequential wave execution.
local M = {}

local function group_by_waves(steps)
  local waves = {
    plan = {},
    implement = {},
    validate = {},
  }

  for i, step in ipairs(steps) do
    if step.role == "product_manager" or step.role == "sprint" or step.role == "planner" then
      waves.plan[#waves.plan + 1] = { index = i, step = step }
    elseif step.role == "developer" then
      waves.implement[#waves.implement + 1] = { index = i, step = step }
    elseif step.role == "tester" or step.role == "reviewer" then
      waves.validate[#waves.validate + 1] = { index = i, step = step }
    else
      waves.implement[#waves.implement + 1] = { index = i, step = step }
    end
  end

  return waves
end

function M.compute_waves(steps)
  return group_by_waves(steps)
end

function M.wave_names()
  return { "plan", "implement", "validate" }
end

function M.is_empty(waves, name)
  if not waves or not waves[name] then
    return true
  end
  return #waves[name] == 0
end

return M
