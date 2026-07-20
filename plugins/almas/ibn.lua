-- Information-efficiency gate. A single agent keeps the full trace in one
-- context; a multi-agent run uses isolated contexts joined by bounded relay
-- messages. Under equal token budgets, strong single agents can outperform
-- multi-agent systems, so simple strong-model tasks avoid coordination cost.
-- This heuristic maps the paper's β into levers we already control (whether to
-- fan out, and how much context to relay). It is a heuristic, not a trained
-- estimator — offline, no model call.
local M = {}

-- relay_k by tier: weaker models get aggressive compression (smaller k), strong
-- models get near-sufficient relay (larger k).
local RELAY_K = { weak = 2, medium = 4, strong = 6 }

-- Multi-step signals that justify fanning out even for a strong model.
local MULTI_STEP = { "and", "then", "feature", "tests", "test", "refactor" }

local function is_multi_step(goal)
  local g = (goal or ""):lower()
  for _, sig in ipairs(MULTI_STEP) do
    if g:find(sig, 1, true) then
      return true
    end
  end
  return false
end

function M.resolve_tier(ctx, model, model_tier)
  if not model then
    return model_tier or "strong", nil
  end
  local resolved, err = n00n.agent.resolve_model(ctx, { spec = model })
  if err then
    return nil, err
  end
  return resolved.tier, nil
end

-- @param ctx AgentContext
-- @param goal string Refined goal.
-- @param supervisor_tier string "weak" | "medium" | "strong".
-- @return { fan_out: boolean, relay_k: integer, reason: string }
function M.decide(_ctx, goal, supervisor_tier)
  supervisor_tier = supervisor_tier or "strong"
  local relay_k = RELAY_K[supervisor_tier] or RELAY_K.medium
  local multi = is_multi_step(goal)

  if supervisor_tier == "strong" and not multi then
    return {
      fan_out = false,
      relay_k = relay_k,
      reason = "strong model + single-step goal: compression can hurt, skip fan-out",
    }
  end

  local reason
  if supervisor_tier == "strong" then
    reason = "strong model but multi-step goal: fan out to parallelize sub-tasks"
  else
    reason = supervisor_tier .. " model: compression via fan-out helps"
  end
  return { fan_out = true, relay_k = relay_k, reason = reason }
end

return M
