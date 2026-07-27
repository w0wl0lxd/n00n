-- SDLC role catalogue and execution (team roles + PR-I reviewer/tester).
-- Each role has a system framing and a default cost-aware tier. Steps run as
-- their own subagent session so we get accurate token/cost telemetry (PR-B).
local M = {}

local subagent = require("n00n.subagent")
local usage = require("n00n.usage")

-- Role -> { tier, system }. Tiers follow the the three-Cs cost-effectiveness:
-- routine clarifying work is cheap, implementation of hard parts is strong.
M.ROLES = {
  product_manager = {
    tier = "weak",
    system = "You are a product manager. Clarify scope, acceptance criteria, and risks. Be concise; output a short bullet list.",
  },
  sprint = {
    tier = "weak",
    system = "You are a Sprint Agent. Refine the goal into a concrete scope, acceptance criteria, and an effort estimate. Keep output short and actionable.",
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

function M.usage(value)
  local measured, err = usage.normalize(value)
  if err then
    error(err, 2)
  end
  return measured
end

function M.add_usage(total, value)
  local measured, err = usage.add(total, value)
  if err then
    error(err, 2)
  end
  return measured
end

M.metrics = usage.price

-- Run one role as a subagent session. Returns {ok, text?, cost, model?, usage?, error?}.
-- @param ctx AgentContext
-- @param role string Key into M.ROLES.
-- @param prompt string Step prompt (already retrieval-augmented by caller).
-- @param opts table { model?, model_tier?, auto_tier?, thinking?, preview?, activity_label?, budget? }
function M.run(ctx, role, prompt, opts)
  opts = opts or {}
  local r = M.ROLES[role] or M.ROLES.developer

  local text, err, cost, usage_val, model_spec = subagent.launch(ctx, {
    description = role,
    prompt = prompt,
    system = r.system,
    model_spec = opts.model,
    model_tier = opts.model_tier or r.tier,
    auto_tier = opts.auto_tier,
    thinking = opts.thinking,
    preview = opts.preview,
    activity_label = opts.activity_label or role,
    budget = opts.budget,
    fail_on_pricing_error = true,
  })

  if err then
    return { ok = false, error = err, cost = cost, model = model_spec, usage = usage_val }
  end

  return { ok = true, text = text, cost = cost, model = model_spec, usage = usage_val }
end

return M
