-- SDLC role catalogue and execution (ALMAS roles + PR-I reviewer/tester).
-- Each role has a system framing and a default cost-aware tier. Steps run as
-- their own subagent session so we get accurate token/cost telemetry (PR-B).
local M = {}

local route_tier = require("noon.route_tier").route_tier

-- Role -> { tier, system }. Tiers follow the ALMAS "three Cs" cost-effectiveness:
-- routine clarifying work is cheap, implementation of hard parts is strong.
M.ROLES = {
  product_manager = {
    tier = "weak",
    system = "You are a product manager. Clarify scope, acceptance criteria, and risks. Be concise; output a short bullet list.",
  },
  planner = {
    tier = "medium",
    system = "You are a sprint planner. Break the goal into ordered, concrete implementation steps with file:line references where possible.",
  },
  developer = {
    tier = "strong",
    system = "You are a senior engineer. Implement the step with minimal, correct changes. Return the files changed and a short summary.",
  },
  tester = {
    tier = "medium",
    system = "You are a test engineer. Write or run tests that validate the change. Report pass/fail with concrete evidence (command + output).",
  },
  reviewer = {
    tier = "medium",
    system = "You are a code reviewer. Critique the diff for correctness, security, and simplicity. End with either APPROVED or a numbered list of blocking issues.",
  },
}

-- Run one role as a subagent session. Returns {ok, text?, cost, model?, error?}.
-- @param ctx AgentContext
-- @param role string Key into M.ROLES.
-- @param prompt string Step prompt (already retrieval-augmented by caller).
-- @param opts table { model_tier?, auto_tier? }
function M.run(ctx, role, prompt, opts)
  opts = opts or {}
  local r = M.ROLES[role] or M.ROLES.developer
  local tier = (opts.auto_tier and route_tier(prompt)) or opts.model_tier or r.tier

  local model, merr = noon.agent.resolve_model(ctx, { tier = tier })
  if merr then
    return { ok = false, error = merr }
  end
  local tools, terr = noon.agent.tools(ctx, { spec = model.spec, audience = "general_sub", include_mcp = true })
  if terr then
    return { ok = false, error = terr }
  end

  local sess, serr = noon.agent.session(ctx, {
    model_spec = model.spec,
    system = r.system,
    tools = tools,
    audience = "general_sub",
    name = role,
  })
  if serr then
    return { ok = false, error = serr }
  end

  local res, rerr = sess:prompt(prompt)
  sess:close()
  if rerr then
    return { ok = false, error = rerr }
  end

  local cost = 0.0
  if res then
    local c, cerr = noon.agent.usage_cost(model.spec, res.input_tokens or 0, res.output_tokens or 0)
    if not cerr then
      cost = c
    end
  end
  return {
    ok = true,
    text = res and res.text,
    cost = cost,
    model = model.spec,
  }
end

return M
