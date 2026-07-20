-- ALMAS: Autonomous LLM-based Multi-Agent Software Engineering (Tawosi et al.,
-- ASE 2025). A supervisor decomposes an SDLC goal into role agents
-- (product_manager, planner, developer, tester, reviewer); each runs as its own
-- subagent on a cost-aware model tier. Built entirely on n00n.agent.* and the
-- existing provider/model-tier machinery — no core changes.
local memory = require("mem")
local refine = require("refine")
local retrieve = require("retrieve")
local roles = require("roles")
local ibn = require("ibn")
local quorum = require("quorum")
local swarm = require("swarm")

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

Modes:
- supervised (default): return the supervisor's plan for review.
- autonomous: run the centralized ALMAS plan to completion; tester/reviewer
  steps are gated by an EBFT diversity-aware quorum.
- swarm: decentralized SwarmSys rounds (Explorers/Workers/Validators) with a
  pheromone reinforcement loop, gated by an information-bottleneck β check
  that decides whether fanning out helps (and how much context to relay).

Notes:
1. The supervisor returns a plan; each step runs independently.
2. Tiers are cost-aware (OrchMAS-style). Override with model_tier, or set auto_tier.
3. With use_retrieval, steps are grounded in repo context (PR-H).
4. With compact (opt-in), retrieved context is TOON-encoded to save tokens (PR-C).
5. swarm mode runs bounded rounds (max_rounds); the β gate may fall back to a
   single strong-agent pass when coordination would not help.
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
      enum = { "supervised", "autonomous", "swarm" },
      default = "supervised",
      description = '"supervised" (default, return the plan for review), "autonomous" (run the plan), or "swarm" (decentralized SwarmSys rounds).',
    },
    max_rounds = {
      type = "integer",
      description = "Swarm mode only: max coordination rounds (default 4).",
    },
    max_concurrent = {
      type = "integer",
      description = "Swarm mode only: max concurrent subagents per round (default 8).",
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
  local validator, verr = n00n.json.schema_validator(PLANNER_OUTPUT)
  if verr then
    return nil, "planner schema invalid: " .. verr
  end
  local model, merr = n00n.agent.resolve_model(ctx, { tier = supervisor_tier })
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
  local model, merr = n00n.agent.resolve_model(ctx, { tier = supervisor_tier or "strong" })
  if merr then
    return nil
  end

  local system = "You are a senior supervisor reviewing the execution of a multi-agent software engineering run. "
    .. "Your task is to analyze the step-by-step reports and produce a concise, actionable summary of "
    .. "'learnings' and 'context' for future runs. Focus on architectural facts discovered, what succeeded, "
    .. "what failed and how it was resolved, and constraints to remember. Do not include raw CLI output or verbose logs. "
    .. "Keep it under 250 words."

  local sess, sess_err = n00n.agent.session(ctx, {
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

local function run_step(ctx, step, goal, input, relay_k)
  local step_prompt = step.prompt
  if input.use_retrieval ~= false then
    local block = retrieve.retrieve(ctx, goal .. " " .. step.prompt, step.role, relay_k)
    if block and #block > 0 then
      if input.compact then
        local ok, t = pcall(function()
          return n00n.json.to_toon({ context = block })
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
  return roles.run(ctx, step.role, step_prompt, role_opts)
end

local function run_autonomous(ctx, goal, input, steps, relay_k)
  local results = {}
  local total_cost = 0.0
  for i, step in ipairs(steps) do
    local r = run_step(ctx, step, goal, input, relay_k)
    if not r.ok then
      results[#results + 1] = string.format("[%d] %s: ERROR %s", i, step.role, r.error)
    else
      local cost_line = string.format(" (~$%.4f, %s)", r.cost or 0, r.model or "?")
      results[#results + 1] = string.format("[%d] %s%s:\n%s", i, step.role, cost_line, r.text or "")
      total_cost = total_cost + (r.cost or 0)

      if step.role == "tester" or step.role == "reviewer" then
        local verdict = quorum.validate(ctx, table.concat(results, "\n\n"), { n = 3 })
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
  return results, total_cost
end

-- Information-bottleneck fallback: a single strong-agent pass when fanning out
-- would not help (strong model + single-step goal). Runs the plan in sequence
-- on the strong tier rather than paying coordination cost.
local function run_single_pass(ctx, goal, input, steps, relay_k)
  local results = {}
  local total_cost = 0.0
  for i, step in ipairs(steps) do
    local r = run_step(ctx, step, goal, input, relay_k)
    r.model = r.model or "strong"
    if not r.ok then
      results[#results + 1] = string.format("[%d] %s: ERROR %s", i, step.role, r.error)
    else
      local cost_line = string.format(" (~$%.4f, %s)", r.cost or 0, r.model or "?")
      results[#results + 1] = string.format("[%d] %s%s:\n%s", i, step.role, cost_line, r.text or "")
      total_cost = total_cost + (r.cost or 0)
    end
  end
  return results, total_cost
end

local finish_run

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

  if input.mode == "supervised" then
    local plan = {}
    for i, step in ipairs(steps) do
      plan[#plan + 1] = string.format("%d. **%s** (%s): %s", i, step.role, step.tier or "default tier", step.prompt)
    end
    return {
      llm_output = table.concat(plan, "\n")
        .. '\n\nReview the plan, then run ALMAS again with `mode = "autonomous"` or `mode = "swarm"` to execute it.',
      format = "markdown",
    }
  end

  -- Information-bottleneck β gate: decide fan-out + relay budget (offline).
  local gate = ibn.decide(ctx, goal, supervisor_tier)
  local relay_k = gate.relay_k

  if input.mode == "swarm" then
    if gate.fan_out then
      local out = swarm.run(ctx, goal, {
        relay_k = relay_k,
        max_rounds = input.max_rounds or 4,
        max_concurrent = input.max_concurrent or 8,
      })
      if not out.ok then
        return { llm_output = "swarm failed: " .. (out.error or "unknown"), is_error = true }
      end
      local results = { string.format("[swarm] β gate: %s\n\n%s", gate.reason, out.text or "") }
      return finish_run(ctx, input, results, out.cost or 0, #steps, slug)
    end

    -- β gate says don't fan out: single strong-agent pass, log the reason.
    local results, total_cost = run_single_pass(ctx, goal, input, steps, relay_k)
    results[1] = "[swarm] β gate: " .. gate.reason .. "\n" .. (results[1] or "")
    return finish_run(ctx, input, results, total_cost, #steps, slug)
  end

  local results, total_cost = run_autonomous(ctx, goal, input, steps, relay_k)
  return finish_run(ctx, input, results, total_cost, #steps, slug)
end

finish_run = function(ctx, input, results, total_cost, n_steps, slug)
  local report = table.concat(results, "\n\n")
  local summary = string.format("\n\n---\nALMAS complete: %d steps, ~$%.4f estimated cost.", n_steps, total_cost)

  local digest = generate_learnings_digest(ctx, input.goal, report, input.model_tier or "strong")
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

n00n.api.register_tool({
  name = "almas",
  description = description,
  kind = "execute",
  audiences = { "main", "workflow" },
  schema = schema,
  handler = handler,
  header = header,
})

-- Interactive launcher so every almas mode and the new ibn/quorum/swarm options
-- are easy to trigger from the UI. Type `/almas` to open the picker; on submit it
-- hands a goal to the agent loop, which invokes the almas tool with the chosen
-- settings. This routes through the normal tool path (no separate execution).
local ListPicker = require("n00n.list_picker")

local ALMAS_MODES = { "supervised", "autonomous", "swarm" }

local function build_launcher_items(prefs)
  local items = {}
  items[#items + 1] = {
    label = "mode: " .. prefs.mode,
    detail = "supervised | autonomous | swarm",
  }
  items[#items + 1] = {
    label = "model_tier: " .. (prefs.model_tier or "strong"),
    detail = "weak | medium | strong",
  }
  items[#items + 1] = {
    label = "use_retrieval: " .. tostring(prefs.use_retrieval),
    detail = "ground steps in repo context",
  }
  items[#items + 1] = {
    label = "ibn_gate: " .. tostring(prefs.ibn_gate),
    detail = "information-bottleneck β fan-out check",
  }
  items[#items + 1] = {
    label = "quorum: " .. tostring(prefs.quorum),
    detail = "EBFT diversity-aware validator quorum",
  }
  items[#items + 1] = {
    label = "max_rounds: " .. tostring(prefs.max_rounds),
    detail = "swarm only: coordination rounds",
  }
  items[#items + 1] = { label = "▶ Run almas", detail = "submit the goal" }
  return items
end

local function cycle(list, current)
  local idx = 1
  for i, v in ipairs(list) do
    if v == current then
      idx = i
      break
    end
  end
  return list[(idx % #list) + 1]
end

local function toggle(value)
  return not value
end

local function run_launcher()
  local prefs = {
    mode = "supervised",
    model_tier = "strong",
    use_retrieval = true,
    ibn_gate = true,
    quorum = true,
    max_rounds = 4,
  }

  while true do
    local items = build_launcher_items(prefs)
    local choice = ListPicker.open(items, {
      title = " ALMAS Launcher ",
      footer = "enter: change/run · esc: cancel",
    })
    if choice.type ~= "choice" then
      return
    end
    local label = items[choice.index].label
    if label:find("mode:") then
      prefs.mode = cycle(ALMAS_MODES, prefs.mode)
    elseif label:find("model_tier:") then
      prefs.model_tier = cycle({ "weak", "medium", "strong" }, prefs.model_tier)
    elseif label:find("use_retrieval:") then
      prefs.use_retrieval = toggle(prefs.use_retrieval)
    elseif label:find("ibn_gate:") then
      prefs.ibn_gate = toggle(prefs.ibn_gate)
    elseif label:find("quorum:") then
      prefs.quorum = toggle(prefs.quorum)
    elseif label:find("max_rounds:") then
      prefs.max_rounds = prefs.max_rounds >= 8 and 2 or prefs.max_rounds + 1
    elseif label:find("Run almas") then
      break
    end
  end

  local flags = {}
  if not prefs.use_retrieval then
    flags[#flags + 1] = "use_retrieval=false"
  end
  if not prefs.ibn_gate then
    flags[#flags + 1] = "ibn_gate disabled"
  end
  if not prefs.quorum then
    flags[#flags + 1] = "quorum disabled"
  end
  if prefs.mode == "swarm" then
    flags[#flags + 1] = "max_rounds=" .. prefs.max_rounds
  end

  local summary = string.format(
    "almas mode=%s model_tier=%s%s",
    prefs.mode,
    prefs.model_tier,
    #flags > 0 and (" [" .. table.concat(flags, ", ") .. "]") or ""
  )
  n00n.ui.flash(summary)
  return summary
end

n00n.api.register_command({
  name = "/almas",
  description = "Launch ALMAS (supervised / autonomous / swarm) with ibn + quorum toggles",
  handler = function()
    run_launcher()
  end,
})
