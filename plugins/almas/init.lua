-- ALMAS: Autonomous LLM-based Multi-Agent Software Engineering (Tawosi et al.,
-- ASE 2025). A supervisor decomposes an SDLC goal into role agents
-- (product_manager, planner, developer, tester, reviewer); each runs as its own
-- subagent on a cost-aware model tier. Built entirely on noon.agent.* and the
-- existing provider/model-tier machinery — no core changes.
local ToolView = require("noon.tool_view")
local memory = require("mem")
local refine = require("refine")
local retrieve = require("retrieve")
local roles = require("roles")
local route_tier = require("noon.route_tier").route_tier

local PLANNER_OUTPUT = {
  type = "object",
  required = { "steps" },
  properties = {
    steps = {
      type = "array",
      items = {
        type = "object",
        required = { "role", "prompt" },
        properties = {
          role = {
            type = "string",
            enum = { "product_manager", "planner", "developer", "tester", "reviewer" },
          },
          prompt = { type = "string" },
          tier = { type = "string", enum = { "weak", "medium", "strong" } },
        },
      },
    },
  },
}

local description =
  [[Launch an ALMAS team. A supervisor decomposes an SDLC goal into role agents and runs each as its own subagent on a cost-aware model tier:

- product_manager: scope & acceptance (weak)
- planner: step breakdown (medium)
- developer: implementation (strong)
- tester: validate (medium)
- reviewer: critique the diff (medium)

Notes:
1. The supervisor returns a plan; each step runs independently.
2. Tiers are cost-aware (OrchMAS-style). Override with model_tier, or set auto_tier.
3. With use_retrieval, steps are grounded in repo context (PR-H).
4. With compact (opt-in), retrieved context is TOON-encoded to save tokens (PR-C).
]]

local schema = {
  type = "object",
  required = { "goal" },
  additionalProperties = false,
  properties = {
    goal = {
      type = "string",
      description = "High-level SDLC goal, e.g. 'Add a retry helper and cover it with tests.'",
    },
    mode = {
      type = "string",
      enum = { "supervised", "autonomous" },
      default = "supervised",
      description = '"supervised" (default, return the plan for review) or "autonomous".',
    },
    model_tier = {
      type = "string",
      description = "Override the supervisor tier (weak/medium/strong). Defaults to strong.",
    },
    auto_tier = {
      type = "boolean",
      description = "Route each subagent tier from its step prompt (opt-in).",
    },
    use_retrieval = {
      type = "boolean",
      default = true,
      description = "Ground steps with repo retrieval.",
    },
    compact = {
      type = "boolean",
      default = false,
      description = "Encode retrieved context as TOON (token-saving, opt-in).",
    },
  },
}

local NUDGE = "You have not called structured_output. Call it now with the plan object."

local function plan_prompt(goal)
  return "Decompose this goal into ordered SDLC steps. Assign each step exactly one role "
    .. "from: product_manager, planner, developer, tester, reviewer. "
    .. "Output the plan via the structured_output tool.\n\nGoal:\n"
    .. goal
end

local function run_supervisor(ctx, goal, supervisor_tier)
  local validator, verr = noon.json.schema_validator(PLANNER_OUTPUT)
  if verr then
    return nil, "planner schema invalid: " .. verr
  end
  local model, merr = noon.agent.resolve_model(ctx, { tier = supervisor_tier })
  if merr then
    return nil, merr
  end
  local system, serr = noon.agent.system_prompt(ctx, { prompt_id = "general", instructions = true })
  if serr then
    return nil, serr
  end
  local tools, terr = noon.agent.tools(ctx, { spec = model.spec, audience = "general_sub", include_mcp = true })
  if terr then
    return nil, terr
  end

  local captured
  local local_tools = {
    structured_output = {
      description = "Output the plan as {steps:[{role, prompt, tier?}]}.",
      input_schema = PLANNER_OUTPUT,
      handler = function(value)
        local e = validator:validate(value)
        if e then
          return nil, "invalid plan: " .. table.concat(e, "; ")
        end
        captured = value
        return "Plan recorded."
      end,
    },
  }

  local sess, sess_err = noon.agent.session(ctx, {
    model_spec = model.spec,
    system = system,
    tools = tools,
    local_tools = local_tools,
    audience = "general_sub",
    name = "almas-supervisor",
  })
  if sess_err then
    return nil, sess_err
  end

  local res, rerr = sess:prompt(plan_prompt(goal))
  if not rerr and not captured then
    res, rerr = sess:prompt(NUDGE)
  end
  sess:close()
  if rerr then
    return nil, "supervisor failed: " .. rerr
  end
  if not captured then
    return nil, "supervisor produced no plan"
  end
  return captured.steps, nil
end

local function generate_learnings_digest(ctx, goal, report, supervisor_tier)
  local model, merr = noon.agent.resolve_model(ctx, { tier = supervisor_tier or "strong" })
  if merr then
    return nil
  end

  local system = "You are a senior supervisor reviewing the execution of a multi-agent software engineering run. "
    .. "Your task is to analyze the step-by-step reports and produce a concise, actionable summary of "
    .. "'learnings' and 'context' for future runs. Focus on architectural facts discovered, what succeeded, "
    .. "what failed and how it was resolved, and constraints to remember. Do not include raw CLI output or verbose logs. "
    .. "Keep it under 250 words."

  local sess, sess_err = noon.agent.session(ctx, {
    model_spec = model.spec,
    system = system,
    audience = "general_sub",
    name = "almas-learning-digest",
  })
  if sess_err then
    return nil
  end

  local prompt = string.format("Original Goal:\n%s\n\nExecution Report:\n%s", goal, report)
  local res, rerr = sess:prompt(prompt)
  sess:close()

  if rerr or not res or not res.text or res.text == "" then
    return nil
  end

  return res.text
end

local function handler(input, ctx)
  local supervisor_tier = input.model_tier or "strong"
  local goal = refine.refine_goal(ctx, input.goal, supervisor_tier)

  local slug = memory.slug(input.goal)
  local prior = memory.load(ctx, slug)
  if prior and #prior > 0 then
    goal = goal .. "\n\nPrior learnings for this goal:\n" .. prior
  end

  local steps, perr = run_supervisor(ctx, goal, supervisor_tier)
  if perr then
    return { llm_output = perr, is_error = true }
  end

  if input.mode ~= "autonomous" then
    local plan = {}
    for i, step in ipairs(steps) do
      plan[#plan + 1] = string.format("%d. **%s** (%s): %s", i, step.role, step.tier or "default tier", step.prompt)
    end
    return {
      llm_output = table.concat(plan, "\n")
        .. '\n\nReview the plan, then run ALMAS again with `mode = "autonomous"` to execute it.',
      format = "markdown",
    }
  end

  local results = {}
  local total_cost = 0.0
  for i, step in ipairs(steps) do
    local step_prompt = step.prompt
    if input.use_retrieval ~= false then
      local block = retrieve.retrieve(ctx, goal .. " " .. step.prompt, step.role, 6)
      if block and #block > 0 then
        if input.compact then
          local ok, t = pcall(function()
            return noon.json.to_toon({ context = block })
          end)
          if ok and t then
            step_prompt = step_prompt .. "\n\nRelevant context (TOON):\n" .. t
          else
            step_prompt = step_prompt .. "\n\nRelevant context:\n" .. block
          end
        else
          step_prompt = step_prompt .. "\n\nRelevant context:\n" .. block
        end
      end
    end

    local role_opts = { model_tier = step.tier, auto_tier = input.auto_tier }
    local r = roles.run(ctx, step.role, step_prompt, role_opts)
    if not r.ok then
      results[#results + 1] = string.format("[%d] %s: ERROR %s", i, step.role, r.error)
    else
      local cost_line = string.format(" (~$%.4f, %s)", r.cost or 0, r.model or "?")
      results[#results + 1] = string.format("[%d] %s%s:\n%s", i, step.role, cost_line, r.text or "")
      total_cost = total_cost + (r.cost or 0)
    end
  end

  local report = table.concat(results, "\n\n")
  local summary = string.format("\n\n---\nALMAS complete: %d steps, ~$%.4f estimated cost.", #steps, total_cost)

  local digest = generate_learnings_digest(ctx, input.goal, report, supervisor_tier)
  if digest then
    memory.save(ctx, slug, digest)
  else
    memory.save(ctx, slug, report .. summary)
  end

  return { llm_output = report .. summary, format = "markdown" }
end

local function header(input)
  return "almas: " .. (input.goal or ""):sub(1, 40)
end

noon.api.register_tool({
  name = "almas",
  description = description,
  kind = "execute",
  audiences = { "main", "workflow" },
  schema = schema,
  handler = handler,
  header = header,
})
