-- Adaptive prompt refinement (HALO): turn a raw goal into an actionable brief
-- with an explicit acceptance criterion. Lexical only in v1 (no model call);
-- a strong-model rewrite can replace this later without changing callers.
local M = {}

function M.refine_goal_lexical(goal)
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

-- @param ctx AgentContext? (optional, triggers model-based refinement when present)
-- @param goal string Raw user goal.
-- @param supervisor_tier string? Optional model tier for refinement (defaults to "strong").
-- @return string Refined brief.
function M.refine_goal(ctx, goal, supervisor_tier)
  local resolved_ctx = ctx
  local resolved_goal = goal

  if not goal and type(ctx) == "string" then
    resolved_goal = ctx
    resolved_ctx = nil
  end

  if not resolved_ctx then
    return M.refine_goal_lexical(resolved_goal)
  end

  local model, merr = noon.agent.resolve_model(resolved_ctx, { tier = supervisor_tier or "strong" })
  if merr then
    return M.refine_goal_lexical(resolved_goal)
  end

  local system = "You are an expert SDLC project manager. Refine this high-level development goal "
    .. "into a detailed, clear, and actionable development brief. Identify any implicit requirements, "
    .. "edge cases, or potential technical risks, and define concrete acceptance criteria. Be concise and precise."

  local sess, sess_err = noon.agent.session(resolved_ctx, {
    model_spec = model.spec,
    system = system,
    audience = "general_sub",
    name = "almas-halo-refiner",
  })
  if sess_err then
    return M.refine_goal_lexical(resolved_goal)
  end

  local prompt = "Refine this goal:\n" .. resolved_goal
  local res, rerr = sess:prompt(prompt)
  sess:close()

  if rerr or not res or not res.text or res.text == "" then
    return M.refine_goal_lexical(resolved_goal)
  end

  return res.text
end

return M
