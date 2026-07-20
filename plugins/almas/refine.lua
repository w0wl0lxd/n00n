-- Adaptive prompt refinement (HALO): turn a raw goal into an actionable brief
-- with an explicit acceptance criterion. Lexical only in v1 (no model call);
-- a strong-model rewrite can replace this later without changing callers.
local M = {}

-- @param goal string Raw user goal.
-- @return string Refined brief.
function M.refine_goal(goal)
  local g = (goal or ""):gsub("^%s+", ""):gsub("%s+$", "")
  if g == "" then
    return goal
  end
  local brief = g
  local has_criterion = brief:match("%?$")
    or brief:lower():find("verify", 1, true)
    or brief:lower():find("test", 1, true)
    or brief:lower():find("accept", 1, true)
  if not has_criterion then
    brief = brief .. "\n\nAcceptance: implement the change and verify it builds and tests pass."
  end
  return brief
end

return M
