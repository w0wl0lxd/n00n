-- Diversity-aware validator quorum. Validators use distinct model tiers and
-- critique prompts when tier routing is active. An exact model intentionally
-- trades model diversity for reproducibility, so confidence is down-weighted.
local M = {}

local N_VALIDATORS = 3
local CRITIQUE_PROMPTS = { "security", "correctness", "simplicity" }
local APPROVED = "APPROVED"

local VALIDATOR_SYSTEM = {
  security = "You are a security reviewer. Check the artifact for vulnerabilities, injection, secrets, and auth/边界 issues. End with APPROVED or a numbered list of blocking issues.",
  correctness = "You are a correctness reviewer. Check the artifact for logic bugs, edge cases, and contract violations. End with APPROVED or a numbered list of blocking issues.",
  simplicity = "You are a simplicity reviewer. Check the artifact for needless complexity, dead code, and clarity. End with APPROVED or a numbered list of blocking issues.",
}

-- Pick n (tier, prompt) pairs that maximize diversity. Prefer distinct tiers;
-- if the config exposes only one tier, fall back to distinct prompts and lower
-- confidence (proxy for u_ε liveness/usability risk).
local function pick_validators(ctx, opts)
  opts = opts or {}
  local n = opts.n or N_VALIDATORS
  local tiers = opts.tiers or { "weak", "medium", "strong" }
  local pairs = {}
  local has_diversity = not opts.model and #tiers >= 2
  for i = 1, n do
    local tier = tiers[(i - 1) % #tiers + 1]
    local prompt = CRITIQUE_PROMPTS[(i - 1) % #CRITIQUE_PROMPTS + 1]
    pairs[#pairs + 1] = { tier = tier, prompt = prompt }
  end
  return pairs, has_diversity
end

local function run_one(ctx, v, artifact, opts)
  if opts.budget then
    local budget_ok, budget_err = opts.budget:consume()
    if not budget_ok then
      return { approved = false, issues = { budget_err }, model = v.tier }
    end
  end
  local model, merr = n00n.agent.resolve_model(ctx, { spec = opts.model, tier = not opts.model and v.tier or nil })
  if merr then
    return { approved = false, issues = { merr }, model = v.tier }
  end
  local tools, terr = n00n.agent.tools(ctx, { spec = model.spec, audience = "general_sub", include_mcp = true })
  if terr then
    return { approved = false, issues = { terr }, model = v.tier }
  end
  local sess, serr = n00n.agent.session(ctx, {
    model_spec = model.spec,
    system = VALIDATOR_SYSTEM[v.prompt],
    tools = tools,
    audience = "general_sub",
    name = "team-quorum-" .. v.prompt,
    thinking = opts.thinking,
  })
  if serr then
    return { approved = false, issues = { serr }, model = v.tier }
  end
  local prompt = "Review this artifact for " .. v.prompt .. ". " .. artifact
  local res, rerr = sess:prompt(prompt)
  sess:close()
  if rerr or not res or not res.text then
    return { approved = false, issues = { rerr or "no output" }, model = v.tier }
  end
  local approved = res.text:match(APPROVED) ~= nil
  local issues = {}
  if not approved then
    for line in res.text:gmatch("[^\n]+") do
      issues[#issues + 1] = line
    end
  end
  return { approved = approved, issues = issues, model = v.tier, text = res.text }
end

-- @param ctx AgentContext
-- @param artifact string Text to validate.
-- @param opts table { n?, tiers?, model?, thinking? }
-- @return { accepted: boolean, issues: [string], confidence: number, diverse: boolean }
function M.validate(ctx, artifact, opts)
  opts = opts or {}
  local validators, diverse = pick_validators(ctx, opts)
  local approvals, issues = 0, {}
  local approved_tiers = {}
  for _, v in ipairs(validators) do
    local r = run_one(ctx, v, artifact, opts)
    if r.approved then
      approvals = approvals + 1
      approved_tiers[r.model] = true
    else
      for _, issue in ipairs(r.issues) do
        issues[#issues + 1] = "[" .. v.prompt .. "] " .. issue
      end
    end
  end

  local n = #validators
  local threshold = math.ceil((n + 1) / 2)
  local accepted = approvals >= threshold

  local confidence = approvals / n
  if accepted and not diverse then
    confidence = confidence * 0.7
  end

  return { accepted = accepted, issues = issues, confidence = confidence, diverse = diverse }
end

return M
