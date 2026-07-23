-- Team: Autonomous LLM-based Multi-Agent Software Engineering (Tawosi et al.,
-- ASE 2025). A supervisor decomposes an SDLC goal into role agents
-- (product_manager, planner, developer, tester, reviewer); each runs as its own
-- subagent on a cost-aware model tier. Built entirely on n00n.agent.* and the
-- existing provider/model-tier machinery — no core changes.
local ActivityPreview = require("n00n.activity_preview")
local memory = require("mem")
local retrieve = require("retrieve")
local roles = require("roles")
local ibn = require("ibn")
local quorum = require("quorum")
local swarm = require("swarm")
local telemetry = require("n00n.telemetry")

local function post_blackboard_status(event_type, step, run_id, extra)
  local post = {
    type = "status",
    content = event_type .. ": " .. (step.role or "unknown"),
    tags = { step.role or "unknown", run_id or "unknown" },
    task_id = run_id,
  }

  if extra then
    for k, v in pairs(extra) do
      post[k] = v
    end
  end

  pcall(function()
    local result = n00n.api.call_tool("blackboard", { action = "write", post = post })
  end)
end

local MAX_PLAN_STEPS = 8
local DEFAULT_PLAN_STEPS = 6
local DEFAULT_SWARM_ROUNDS = 2
local MAX_SWARM_ROUNDS = 4
local DEFAULT_TEAM_AGENTS = 16
local MAX_TEAM_AGENTS = 24
local MAX_TEAM_CONCURRENT = 4
local TEAM_TIMEOUT_SECS = 1800
local MAX_RELAY_BYTES = 12000

local function add_cost(total, value)
  if total == nil or value == nil then
    return nil
  end
  return total + value
end

local function cost_label(cost, model)
  if cost == nil then
    return " (cost unavailable, " .. (model or "?") .. ")"
  end
  return string.format(" (~$%.4f, %s)", cost, model or "?")
end

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
            enum = { "product_manager", "sprint", "planner", "developer", "tester", "reviewer" },
          },
          prompt = { type = "string" },
          tier = { type = "string", enum = { "weak", "medium", "strong" } },
          acceptance_criteria = { type = "string" },
          effort = { type = "string" },
        },
      },
    },
  },
}

local description =
  [[Run an ALMAS team for an SDLC goal. supervised returns a plan; autonomous executes it; swarm runs decentralized rounds. background returns an agent_id for agent_control.]]

local schema = {
  type = "object",
  required = { "goal" },
  additionalProperties = false,
  properties = {
    goal = {
      type = "string",
      description = "High-level SDLC goal.",
    },
    mode = {
      type = "string",
      enum = { "supervised", "autonomous", "swarm" },
      default = "supervised",
      description = '"supervised" (return plan), "autonomous" (run plan), "swarm" (decentralized rounds).',
    },
    max_rounds = {
      type = "integer",
      minimum = 1,
      maximum = MAX_SWARM_ROUNDS,
      description = "Swarm max rounds (default 2, max 4).",
    },
    max_concurrent = {
      type = "integer",
      minimum = 1,
      maximum = MAX_TEAM_CONCURRENT,
      description = "Swarm concurrency (default 4, max 4).",
    },
    max_agents = {
      type = "integer",
      minimum = 1,
      maximum = MAX_TEAM_AGENTS,
      description = "Team agent budget (default 16, max 24).",
    },
    max_steps = {
      type = "integer",
      minimum = 1,
      maximum = MAX_PLAN_STEPS,
      description = "Max plan steps (default 6, max 8).",
    },
    model = {
      type = "string",
      description = "Exact model for all agents. Overrides model_tier.",
    },
    model_tier = {
      type = "string",
      description = "Supervisor tier (weak/medium/strong). Default: strong.",
    },
    thinking = {
      type = { "string", "integer" },
      description = 'Thinking mode: "off", "adaptive", effort level, or token budget. Default: "adaptive".',
    },
    auto_tier = {
      type = "boolean",
      description = "Route subagent tier from step prompt. Default: true unless model set.",
    },
    use_retrieval = {
      type = "boolean",
      default = true,
      description = "Ground steps with repo retrieval.",
    },
    ibn_gate = {
      type = "boolean",
      default = true,
      description = "Use information-bottleneck fan-out gate in swarm.",
    },
    quorum = {
      type = "boolean",
      default = true,
      description = "Require validator quorum for autonomous/swarm.",
    },
    background = {
      type = "boolean",
      description = "Start in background session; return agent_id.",
    },
    compact = {
      type = "boolean",
      default = false,
      description = "TOON-encode retrieved context (token-saving).",
    },
    use_summary = {
      type = "boolean",
      default = false,
      description = "Use the Summary Agent index for retrieval.",
    },
    human_escalation = {
      type = "boolean",
      default = false,
      description = "Pause on step failure and return a resumable run_id.",
    },
    resume = {
      type = "string",
      description = "Paused run_id to resume.",
    },
    continue = {
      type = "string",
      description = "Human guidance appended when resuming.",
    },
  },
}

local NUDGE = "You have not called structured_output. Call it now with the plan object."

local function new_agent_budget(requested)
  local limit = math.min(requested or DEFAULT_TEAM_AGENTS, MAX_TEAM_AGENTS)
  return {
    limit = limit,
    used = 0,
    consume = function(self)
      if self.used >= self.limit then
        return nil, "team agent-call budget exhausted (" .. self.limit .. "; hard maximum " .. MAX_TEAM_AGENTS .. ")"
      end
      self.used = self.used + 1
      return true
    end,
  }
end

local function plan_prompt(goal)
  return "Decompose this goal into ordered SDLC steps. Assign each step exactly one role "
    .. "from: product_manager, sprint, planner, developer, tester, reviewer. "
    .. "Use the 'sprint' role to refine the goal into acceptance criteria and an effort estimate. "
    .. "For any step, you may include optional 'acceptance_criteria' and 'effort' fields. "
    .. "Output the plan via the structured_output tool.\n\nGoal:\n"
    .. goal
end

local function run_supervisor(ctx, goal, opts)
  local budget_ok, budget_err = opts._agent_budget:consume()
  if not budget_ok then
    return nil, budget_err
  end
  local validator, verr = n00n.json.schema_validator(PLANNER_OUTPUT)
  if verr then
    return nil, "planner schema invalid: " .. verr
  end
  local model, merr =
    n00n.agent.resolve_model(ctx, { spec = opts.model, tier = not opts.model and opts.model_tier or nil })
  if merr then
    return nil, merr
  end
  local system, serr = n00n.agent.system_prompt(ctx, { prompt_id = "general", instructions = true })
  if serr then
    return nil, serr
  end
  local tools, terr = n00n.agent.tools(ctx, { spec = model.spec, audience = "general_sub", include_mcp = true })
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

  local sess, sess_err = n00n.agent.session(ctx, {
    model_spec = model.spec,
    system = system,
    tools = tools,
    local_tools = local_tools,
    audience = "general_sub",
    name = "team-supervisor",
    thinking = opts.thinking,
  })
  if sess_err then
    return nil, sess_err
  end

  local res, rerr = opts._preview:prompt(sess, plan_prompt(goal), "supervisor")
  if not rerr and not captured then
    local nudged
    nudged, rerr = opts._preview:prompt(sess, NUDGE, "supervisor")
    if nudged then
      res = nudged
    end
  end
  sess:close()
  local usage, cost, metrics_err = roles.metrics(model.spec, res)
  if metrics_err then
    return nil, "supervisor usage pricing failed: " .. metrics_err, nil, usage
  end
  if rerr then
    return nil, "supervisor failed: " .. rerr, cost, usage
  end
  if not captured then
    return nil, "supervisor produced no plan", cost, usage
  end
  local max_steps = math.min(opts.max_steps or DEFAULT_PLAN_STEPS, MAX_PLAN_STEPS)
  local steps = {}
  for i = 1, math.min(#captured.steps, max_steps) do
    steps[i] = captured.steps[i]
  end
  if #steps == 0 then
    return nil, "supervisor produced an empty plan", cost, usage
  end
  return steps, nil, cost, usage
end

local function run_step(ctx, step, goal, input, relay_k, prior_results)
  local step_prompt = step.prompt
  if step.acceptance_criteria and #step.acceptance_criteria > 0 then
    step_prompt = step_prompt .. "\n\nAcceptance criteria:\n" .. step.acceptance_criteria
  end
  if prior_results and #prior_results > 0 then
    local prior = table.concat(prior_results, "\n\n"):sub(-MAX_RELAY_BYTES)
    step_prompt = step_prompt .. "\n\nResults from earlier plan steps:\n" .. prior
  end
  if input.use_retrieval ~= false then
    local block = retrieve.retrieve(ctx, goal .. " " .. step.prompt, step.role, relay_k, input.use_summary)
    if block and #block > 0 then
      if input.compact then
        local encoded, fmt = n00n.json.tooned({ context = block })
        if encoded then
          step_prompt = step_prompt .. "\n\nRelevant context (" .. (fmt or "json") .. "):\n" .. encoded
        else
          step_prompt = step_prompt .. "\n\nRelevant context:\n" .. block
        end
      else
        step_prompt = step_prompt .. "\n\nRelevant context:\n" .. block
      end
    end
  end

  local role_opts = {
    model = input.model,
    model_tier = step.tier,
    auto_tier = input.auto_tier,
    thinking = input.thinking,
    budget = input._agent_budget,
    preview = input._preview,
  }
  return roles.run(ctx, step.role, step_prompt, role_opts)
end

local function run_autonomous(ctx, goal, input, steps, relay_k, logger, resume_state)
  local results = {}
  local total_cost = 0.0
  local total_usage = roles.usage()
  local start_index = 1
  if resume_state then
    results = resume_state.results or results
    total_cost = resume_state.total_cost or total_cost
    total_usage = resume_state.total_usage or total_usage
    start_index = resume_state.start_index or start_index
  end
  local failures = 0
  for i = start_index, #steps do
    local step = steps[i]
    if i == start_index and input.continue and #input.continue > 0 then
      step.prompt = step.prompt .. "\n\nHuman guidance:\n" .. input.continue
    end
    if logger then
      logger.log("step_started", { index = i, role = step.role, tier = step.tier })
    end
    post_blackboard_status("step_started", step, run_id, { index = i })
    local r = run_step(ctx, step, goal, input, relay_k, results)
    total_cost = add_cost(total_cost, r.cost)
    total_usage = roles.add_usage(total_usage, r.usage)
    if not r.ok then
      failures = failures + 1
      results[#results + 1] = string.format("[%d] %s: ERROR %s", i, step.role, r.error)
      if logger then
        logger.log("step_error", { index = i, role = step.role, error = r.error })
      end
      post_blackboard_status("step_error", step, run_id, { index = i, error = r.error })
      if input.human_escalation then
        memory.save_state(ctx, input.resume or memory.slug(goal), {
          goal = goal,
          steps = steps,
          results = results,
          total_cost = total_cost,
          total_usage = total_usage,
          failed_index = i,
          start_index = i,
        })
        return results,
          total_cost,
          failures,
          total_usage,
          {
            paused = true,
            run_id = input.resume or memory.slug(goal),
            failed_step = i,
            failed_role = step.role,
            error = r.error,
          }
      end
      break
    else
      local cost_line = cost_label(r.cost, r.model)
      results[#results + 1] = string.format("[%d] %s%s:\n%s", i, step.role, cost_line, r.text or "")
      if logger then
        logger.log("step_done", { index = i, role = step.role, cost = r.cost or 0, model = r.model })
      end
      post_blackboard_status("step_done", step, run_id, { index = i, cost = r.cost or 0, model = r.model })

      if input.quorum ~= false and (step.role == "tester" or step.role == "reviewer") then
        local verdict = quorum.validate(ctx, table.concat(results, "\n\n"), {
          n = 3,
          model = input.model,
          thinking = input.thinking,
          budget = input._agent_budget,
          preview = input._preview,
        })
        total_cost = add_cost(total_cost, verdict.cost)
        total_usage = roles.add_usage(total_usage, verdict.usage)
        if not verdict.accepted then
          results[#results + 1] = string.format(
            "[quorum] %s output not endorsed by diverse validators (confidence %.2f):\n%s",
            step.role,
            verdict.confidence,
            table.concat(verdict.issues, "\n")
          )
        end
      end
    end
  end
  return results, total_cost, failures, total_usage, nil
end

-- Information-bottleneck fallback: a single strong-agent pass when fanning out
-- would not help (strong model + single-step goal). Runs the plan in sequence,
-- honoring each step's tier, rather than paying coordination cost.
local function run_single_pass(ctx, goal, input, steps, relay_k, logger, resume_state)
  local results = {}
  local total_cost = 0.0
  local total_usage = roles.usage()
  local start_index = 1
  if resume_state then
    results = resume_state.results or results
    total_cost = resume_state.total_cost or total_cost
    total_usage = resume_state.total_usage or total_usage
    start_index = resume_state.start_index or start_index
  end
  local failures = 0
  for i = start_index, #steps do
    local step = steps[i]
    if i == start_index and input.continue and #input.continue > 0 then
      step.prompt = step.prompt .. "\n\nHuman guidance:\n" .. input.continue
    end
    if logger then
      logger.log("step_started", { index = i, role = step.role, tier = step.tier })
    end
    post_blackboard_status("step_started", step, run_id, { index = i })
    local r = run_step(ctx, step, goal, input, relay_k, results)
    r.model = r.model or "strong"
    total_cost = add_cost(total_cost, r.cost)
    total_usage = roles.add_usage(total_usage, r.usage)
    if not r.ok then
      failures = failures + 1
      results[#results + 1] = string.format("[%d] %s: ERROR %s", i, step.role, r.error)
      if logger then
        logger.log("step_error", { index = i, role = step.role, error = r.error })
      end
      post_blackboard_status("step_error", step, run_id, { index = i, error = r.error })
      if input.human_escalation then
        memory.save_state(ctx, input.resume or memory.slug(goal), {
          goal = goal,
          steps = steps,
          results = results,
          total_cost = total_cost,
          total_usage = total_usage,
          failed_index = i,
          start_index = i,
        })
        return results,
          total_cost,
          failures,
          total_usage,
          {
            paused = true,
            run_id = input.resume or memory.slug(goal),
            failed_step = i,
            failed_role = step.role,
            error = r.error,
          }
      end
      break
    else
      local cost_line = cost_label(r.cost, r.model)
      results[#results + 1] = string.format("[%d] %s%s:\n%s", i, step.role, cost_line, r.text or "")
      if logger then
        logger.log("step_done", { index = i, role = step.role, cost = r.cost or 0, model = r.model })
      end
      post_blackboard_status("step_done", step, run_id, { index = i, cost = r.cost or 0, model = r.model })
    end
  end
  return results, total_cost, failures, total_usage, nil
end

local finish_run

local function run_team(input, ctx)
  if input.background then
    local forwarded = {}
    for key, value in pairs(input) do
      forwarded[key] = value
    end
    forwarded.background = false
    local prompt = "Use the team tool now. Do not only describe this request.\n\n" .. n00n.json.encode(forwarded)
    local id, err = n00n.session.new({ prompt = prompt, focus = false })
    if not id then
      return { llm_output = err, is_error = true }
    end
    local title = "team: " .. (input.goal or ""):sub(1, 60)
    pcall(function()
      n00n.session.set_title({ id = id, title = title })
    end)
    return n00n.json.encode({ agent_id = id, status = "started", title = title })
  end

  input.mode = input.mode or "supervised"
  input.model_tier = input.model_tier or "strong"
  if input.auto_tier == nil then
    input.auto_tier = input.model == nil
  end
  if input.thinking == nil then
    input.thinking = "adaptive"
  end
  input._agent_budget = new_agent_budget(input.max_agents)
  local goal = input.goal

  local slug = memory.slug(input.goal)
  local prior = memory.load(ctx, slug)
  if prior and #prior > 0 then
    goal = goal .. "\n\nPrior learnings for this goal:\n" .. prior
  end

  local run_id = n00n.workflow.hash(input.goal .. "\0" .. tostring(os.time()))
  local team_dir = memory.base_dir()
  local logger
  if team_dir then
    logger = telemetry.open(n00n.fs.joinpath(team_dir, "events"), run_id)
  end

  local steps, perr, supervisor_cost, supervisor_usage
  local resume_state
  if input.resume and #input.resume > 0 then
    resume_state = memory.load_state(ctx, input.resume)
    if resume_state then
      steps = resume_state.steps
      goal = resume_state.goal
      input.mode = input.mode or "autonomous"
    else
      return { llm_output = "resume run_id not found: " .. input.resume, is_error = true }
    end
  end

  if not steps then
    steps, perr, supervisor_cost, supervisor_usage = run_supervisor(ctx, goal, input)
    supervisor_usage = roles.usage(supervisor_usage)
  end
  if perr then
    if logger then
      logger.log("run_error", { error = perr })
    end
    return { llm_output = perr, is_error = true, cost = supervisor_cost, usage = supervisor_usage }
  end

  if input.mode == "supervised" then
    if logger then
      logger.log("run_started", { mode = "supervised", goal = input.goal })
    end
    local plan = {}
    for i, step in ipairs(steps) do
      local extra = ""
      if step.effort then
        extra = extra .. " — effort: " .. step.effort
      end
      plan[#plan + 1] =
        string.format("%d. **%s** (%s): %s%s", i, step.role, step.tier or "default tier", step.prompt, extra)
      if step.acceptance_criteria and #step.acceptance_criteria > 0 then
        plan[#plan + 1] = "   - *Acceptance*: " .. step.acceptance_criteria
      end
    end
    if logger then
      logger.log("run_done", { mode = "supervised", steps = #steps, goal = input.goal })
    end
    return {
      llm_output = table.concat(plan, "\n")
        .. '\n\nReview the plan, then run `team` again with `mode = "autonomous"` or `mode = "swarm"` to execute it.',
      format = "markdown",
      cost = supervisor_cost,
      usage = supervisor_usage,
    }
  end

  if logger then
    logger.log("run_started", { mode = input.mode, goal = input.goal })
  end

  -- Information-bottleneck β gate: decide fan-out + relay budget (offline).
  local ibn_tier, model_err = ibn.resolve_tier(ctx, input.model, input.model_tier)
  if model_err then
    return { llm_output = model_err, is_error = true, cost = supervisor_cost, usage = supervisor_usage }
  end
  local gate = input.ibn_gate == false and { fan_out = true, relay_k = 6, reason = "IBN gate disabled" }
    or ibn.decide(ctx, goal, ibn_tier)
  local relay_k = gate.relay_k

  if input.mode == "swarm" then
    if gate.fan_out then
      local out = swarm.run(ctx, goal, {
        relay_k = relay_k,
        max_rounds = math.min(input.max_rounds or DEFAULT_SWARM_ROUNDS, MAX_SWARM_ROUNDS),
        max_concurrent = math.min(input.max_concurrent or MAX_TEAM_CONCURRENT, MAX_TEAM_CONCURRENT),
        model = input.model,
        budget = input._agent_budget,
        thinking = input.thinking,
        quorum = input.quorum,
        use_summary = input.use_summary,
        preview = input._preview,
      })
      local total_cost = add_cost(supervisor_cost, out.cost)
      local total_usage = roles.add_usage(supervisor_usage, out.usage)
      if not out.ok then
        return {
          llm_output = "swarm failed: " .. (out.error or "unknown"),
          is_error = true,
          cost = total_cost,
          usage = total_usage,
        }
      end
      local results = { string.format("[swarm] β gate: %s\n\n%s", gate.reason, out.text or "") }
      return finish_run(ctx, input, results, total_cost, out.rounds or 0, "rounds", slug, nil, total_usage, logger)
    end

    -- β gate says don't fan out: single strong-agent pass, log the reason.
    local results, sp_cost, sp_failures, sp_usage, pause =
      run_single_pass(ctx, goal, input, steps, relay_k, logger, resume_state)
    if pause then
      if logger then
        logger.log(
          "human_escalation",
          { run_id = pause.run_id, failed_step = pause.failed_step, failed_role = pause.failed_role }
        )
      end
      return {
        llm_output = n00n.json.encode(pause),
        format = "json",
        is_error = true,
      }
    end
    total_cost = add_cost(supervisor_cost, sp_cost)
    total_usage = roles.add_usage(supervisor_usage, sp_usage)
    results[1] = "[swarm] β gate: " .. gate.reason .. "\n" .. (results[1] or "")
    return finish_run(ctx, input, results, total_cost, #results, "steps", slug, sp_failures, total_usage, logger)
  end

  local results, auto_cost, auto_failures, auto_usage, pause =
    run_autonomous(ctx, goal, input, steps, relay_k, logger, resume_state)
  if pause then
    if logger then
      logger.log(
        "human_escalation",
        { run_id = pause.run_id, failed_step = pause.failed_step, failed_role = pause.failed_role }
      )
    end
    return {
      llm_output = n00n.json.encode(pause),
      format = "json",
      is_error = true,
    }
  end
  total_cost = add_cost(supervisor_cost, auto_cost)
  total_usage = roles.add_usage(supervisor_usage, auto_usage)
  return finish_run(ctx, input, results, total_cost, #results, "steps", slug, auto_failures, total_usage, logger)
end

local function handler(input, ctx)
  if input.background then
    return run_team(input, ctx)
  end
  local preview, preview_err = ActivityPreview.new(ctx, "team: " .. (input.goal or "team"), { session_rows = true })
  if not preview then
    return { llm_output = "failed to publish team preview: " .. tostring(preview_err), is_error = true }
  end
  input._preview = preview
  local ok, result = pcall(run_team, input, ctx)
  if not ok then
    return { llm_output = "team failed: " .. tostring(result), is_error = true, body = preview.view.buf }
  end
  result.body = preview.view.buf
  return result
end

finish_run = function(ctx, input, results, total_cost, completed, unit, slug, failures, usage, logger)
  local report = table.concat(results, "\n\n")
  local failed = failures or 0
  local successful = math.max(completed - failed, 0)
  local summary
  if total_cost == nil then
    summary = string.format("\n\n---\nTeam complete: %d %s. Cost estimate unavailable.", successful, unit)
  else
    summary = string.format("\n\n---\nTeam complete: %d %s, ~$%.4f estimated cost.", successful, unit, total_cost)
  end
  if failed > 0 then
    summary = summary .. string.format(" %d step(s) failed; the run is incomplete.", failed)
  end

  memory.save(ctx, slug, report .. summary)

  if logger then
    if failed > 0 then
      logger.log("run_error", { completed = completed, failed = failed, total_cost = total_cost, unit = unit })
    else
      logger.log("run_done", { completed = completed, total_cost = total_cost, unit = unit })
    end
  end

  return {
    llm_output = report .. summary,
    format = "markdown",
    is_error = failed > 0,
    cost = total_cost,
    usage = roles.usage(usage),
  }
end

local function header(input)
  return "team: " .. (input.goal or ""):sub(1, 40)
end

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- For multi-step work, use **team** (ALMAS-led agent team) with `compact=true` and `use_retrieval=true` to save tokens. Use **workflow** when you need a sandboxed supervisor script to orchestrate agents at scale.",
})

n00n.api.register_tool({
  name = "team",
  description = description,
  kind = "execute",
  audiences = { "main", "workflow" },
  schema = schema,
  timeout = TEAM_TIMEOUT_SECS,
  handler = handler,
  header = header,
})

local TEAM_MODES = { "supervised", "autonomous", "swarm" }
local MODEL_TIERS = { "weak", "medium", "strong" }
local THINKING_LEVELS = { "off", "adaptive", "low", "medium", "high", "xhigh", "max" }
local MENU_ROWS = 10
local RUN_ROW = 11

local function cycle(list, current)
  local idx = 1
  for i, value in ipairs(list) do
    if value == current then
      idx = i
      break
    end
  end
  return list[(idx % #list) + 1]
end

local function trim(value)
  return (value or ""):match("^%s*(.-)%s*$")
end

local function prompt_value(value)
  return tostring(value)
end

local function agent_prompt(goal, prefs)
  local config = {
    { "mode", prefs.mode },
    { "model_tier", prefs.model_tier },
    { "thinking", prefs.thinking },
    { "auto_tier", prefs.auto_tier },
    { "use_retrieval", prefs.use_retrieval },
    { "ibn_gate", prefs.ibn_gate },
    { "quorum", prefs.quorum },
    { "max_rounds", prefs.max_rounds },
  }
  if prefs.model then
    table.insert(config, 2, { "model", prefs.model })
  end
  local lines = {
    "Use the team tool now. Do not only describe the team or restate this request.",
    "",
    "Goal:",
    goal,
    "",
    "Configuration:",
  }
  for _, item in ipairs(config) do
    lines[#lines + 1] = "- " .. item[1] .. ": " .. prompt_value(item[2])
  end
  return table.concat(lines, "\n")
end

local function run_launcher(initial_goal)
  local prefs = {
    mode = "supervised",
    model_tier = "strong",
    thinking = "max",
    auto_tier = true,
    use_retrieval = true,
    ibn_gate = true,
    quorum = true,
    max_rounds = DEFAULT_SWARM_ROUNDS,
  }
  local TextInput = require("n00n.text_input")
  local goal = TextInput.new()
  goal:insert_text(initial_goal or "")
  local selected = trim(initial_goal) == "" and 10 or 1
  local editing
  if trim(initial_goal) == "" then
    editing = "goal"
  end
  local width = 80
  local buf = n00n.ui.buf()
  local win

  local function render()
    local rows = {
      { "Mode", prefs.mode },
      { "Model tier", prefs.model_tier },
      { "Model", prefs.model or "Default (tier routing)" },
      { "Thinking", prefs.thinking },
      { "Auto tier", tostring(prefs.auto_tier) },
      { "Retrieval", tostring(prefs.use_retrieval) },
      { "IBN gate", tostring(prefs.ibn_gate) },
      { "Quorum", tostring(prefs.quorum) },
      { "Max rounds", tostring(prefs.max_rounds) },
    }
    local lines = {}
    for i, row in ipairs(rows) do
      local marker = selected == i and "❯ " or "  "
      lines[#lines + 1] = {
        { marker .. row[1] .. ": ", selected == i and "selected" or "dim" },
        { row[2], selected == i and "selected" or "" },
      }
    end
    lines[#lines + 1] = { { (selected == 10 and "❯ " or "  ") .. "Goal", selected == 10 and "selected" or "dim" } }
    local rendered = goal:render("  ", 2, width)
    for _, line in ipairs(rendered.lines) do
      lines[#lines + 1] = line
    end
    lines[#lines + 1] = { { "", "" } }
    lines[#lines + 1] = {
      { selected == RUN_ROW and "❯ Start team" or "  Start team", selected == RUN_ROW and "selected" or "item" },
    }
    buf:set_lines(lines)
    if win then
      if editing == "goal" then
        win:set_cursor(10 + rendered.cursor_row)
      elseif selected <= 9 then
        win:set_cursor(selected)
      elseif selected == 10 then
        win:set_cursor(10)
      else
        win:set_cursor(#lines)
      end
    end
  end

  win = n00n.ui.open_win(buf, {
    title = " Team ",
    footer = {
      { "↑/↓", "navigate" },
      { "Enter", "change/edit/run" },
      { "Ctrl+Enter", "run" },
      { "Esc", "cancel" },
    },
    width = "70%",
    height = 18,
    cursor_line = true,
  })
  width = win.width
  render()

  local function submit()
    local value = trim(goal:value())
    if value == "" then
      selected = 10
      editing = "goal"
      n00n.ui.flash("Enter a team goal first")
      render()
      return false
    end
    local _, err = n00n.session.prompt(agent_prompt(value, prefs))
    if err then
      n00n.ui.flash("Team: " .. err)
      return false
    end
    win:close()
    return true
  end

  while true do
    local event = win:recv()
    if not event or event.type == "close" then
      return
    elseif event.type == "resize" then
      width = event.width
      render()
    elseif event.type == "paste" and editing == "goal" then
      goal:insert_text(event.text)
      render()
    elseif event.type == "key" then
      local key = event.key
      if key == "esc" or key == "ctrl+c" then
        if editing then
          editing = nil
          render()
        else
          win:close()
          return
        end
      elseif key == "ctrl+enter" then
        if submit() then
          return
        end
      elseif editing then
        if key == "enter" then
          editing = nil
          selected = RUN_ROW
          render()
        else
          if goal:handle_key(key) ~= TextInput.Result.IGNORED then
            render()
          end
        end
      elseif key == "up" or key == "shift+tab" then
        selected = (selected - 2) % RUN_ROW + 1
        render()
      elseif key == "down" or key == "tab" then
        selected = selected % RUN_ROW + 1
        render()
      elseif key == "enter" then
        if selected == 1 then
          prefs.mode = cycle(TEAM_MODES, prefs.mode)
        elseif selected == 2 then
          prefs.model_tier = cycle(MODEL_TIERS, prefs.model_tier)
        elseif selected == 3 then
          win:hide()
          local picked = n00n.ui.pick_model(prefs.model)
          win:show()
          if picked then
            prefs.model = picked
          end
        elseif selected == 4 then
          prefs.thinking = cycle(THINKING_LEVELS, prefs.thinking)
        elseif selected == 5 then
          prefs.auto_tier = not prefs.auto_tier
        elseif selected == 6 then
          prefs.use_retrieval = not prefs.use_retrieval
        elseif selected == 7 then
          prefs.ibn_gate = not prefs.ibn_gate
        elseif selected == 8 then
          prefs.quorum = not prefs.quorum
        elseif selected == 9 then
          prefs.max_rounds = prefs.max_rounds >= MAX_SWARM_ROUNDS and 1 or prefs.max_rounds + 1
        elseif selected == 10 then
          editing = "goal"
        elseif submit() then
          return
        end
        render()
      end
    end
  end
end

n00n.api.register_command({
  name = "/team",
  description = "Configure and run an agent team for a goal",
  handler = function(args)
    run_launcher(trim(args))
  end,
})
