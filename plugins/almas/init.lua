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
  steps are gated by a diversity-aware validator quorum.
- swarm: decentralized SwarmSys rounds (Explorers/Workers/Validators) with a
  pheromone reinforcement loop, gated by an information-bottleneck β check
  that decides whether fanning out helps (and how much context to relay).

Notes:
1. The supervisor returns a plan; each step runs independently.
2. Agents are routed by cost-aware tiers by default. Set model for an exact model,
   model_tier for a fixed tier, or auto_tier=false to disable adaptive routing.
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
    model = {
      type = "string",
      description = "Exact model spec for every ALMAS agent. Overrides model_tier and role tiers.",
    },
    model_tier = {
      type = "string",
      description = "Supervisor/model tier (weak/medium/strong). Defaults to strong when model is omitted.",
    },
    thinking = {
      type = { "string", "integer" },
      description = 'Thinking mode for ALMAS agents: "off", "adaptive", an effort level through "max", or a token budget. Defaults to "max".',
    },
    auto_tier = {
      type = "boolean",
      description = "Route each subagent tier from its step prompt. Defaults to true unless an exact model is set.",
    },
    use_retrieval = {
      type = "boolean",
      default = true,
      description = "Ground steps with repo retrieval.",
    },
    ibn_gate = {
      type = "boolean",
      default = true,
      description = "Use the information-bottleneck fan-out gate in swarm mode.",
    },
    quorum = {
      type = "boolean",
      default = true,
      description = "Require validator quorum for autonomous validation and swarm acceptance.",
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

local function run_supervisor(ctx, goal, opts)
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
    name = "almas-supervisor",
    thinking = opts.thinking,
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

local function generate_learnings_digest(ctx, goal, report, opts)
  local model, merr =
    n00n.agent.resolve_model(ctx, { spec = opts.model, tier = not opts.model and opts.model_tier or nil })
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
    thinking = opts.thinking,
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

  local role_opts =
    { model = input.model, model_tier = step.tier, auto_tier = input.auto_tier, thinking = input.thinking }
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

      if input.quorum ~= false and (step.role == "tester" or step.role == "reviewer") then
        local verdict =
          quorum.validate(ctx, table.concat(results, "\n\n"), { n = 3, model = input.model, thinking = input.thinking })
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
-- would not help (strong model + single-step goal). Runs the plan in sequence,
-- honoring each step's tier, rather than paying coordination cost.
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
  input.mode = input.mode or "supervised"
  input.model_tier = input.model_tier or "strong"
  if input.auto_tier == nil then
    input.auto_tier = input.model == nil
  end
  if input.thinking == nil then
    input.thinking = "max"
  end
  local goal = refine.refine_goal(ctx, input.goal, input)

  local slug = memory.slug(input.goal)
  local prior = memory.load(ctx, slug)
  if prior and #prior > 0 then
    goal = goal .. "\n\nPrior learnings for this goal:\n" .. prior
  end

  local steps, perr = run_supervisor(ctx, goal, input)
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
  local ibn_tier, model_err = ibn.resolve_tier(ctx, input.model, input.model_tier)
  if model_err then
    return { llm_output = model_err, is_error = true }
  end
  local gate = input.ibn_gate == false and { fan_out = true, relay_k = 6, reason = "IBN gate disabled" }
    or ibn.decide(ctx, goal, ibn_tier)
  local relay_k = gate.relay_k

  if input.mode == "swarm" then
    if gate.fan_out then
      local out = swarm.run(ctx, goal, {
        relay_k = relay_k,
        max_rounds = input.max_rounds or 4,
        max_concurrent = input.max_concurrent or 8,
        model = input.model,
        thinking = input.thinking,
        quorum = input.quorum,
      })
      if not out.ok then
        return { llm_output = "swarm failed: " .. (out.error or "unknown"), is_error = true }
      end
      local results = { string.format("[swarm] β gate: %s\n\n%s", gate.reason, out.text or "") }
      return finish_run(ctx, input, results, out.cost or 0, out.rounds or 0, "rounds", slug)
    end

    -- β gate says don't fan out: single strong-agent pass, log the reason.
    local results, total_cost = run_single_pass(ctx, goal, input, steps, relay_k)
    results[1] = "[swarm] β gate: " .. gate.reason .. "\n" .. (results[1] or "")
    return finish_run(ctx, input, results, total_cost, #steps, "steps", slug)
  end

  local results, total_cost = run_autonomous(ctx, goal, input, steps, relay_k)
  return finish_run(ctx, input, results, total_cost, #steps, "steps", slug)
end

finish_run = function(ctx, input, results, total_cost, completed, unit, slug)
  local report = table.concat(results, "\n\n")
  local summary = string.format("\n\n---\nALMAS complete: %d %s, ~$%.4f estimated cost.", completed, unit, total_cost)

  local digest = generate_learnings_digest(ctx, input.goal, report, input)
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

local TextInput = require("n00n.text_input")

local ALMAS_MODES = { "supervised", "autonomous", "swarm" }
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
    "Use the almas tool now. Do not only describe ALMAS or restate this request.",
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
    max_rounds = 4,
  }
  local model = TextInput.new()
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
    prefs.model = trim(model:value())
    if prefs.model == "" then
      prefs.model = nil
    end
    local rows = {
      { "Mode", prefs.mode },
      { "Model tier", prefs.model_tier },
      { "Exact model", prefs.model or "(use tier)" },
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
      { selected == RUN_ROW and "❯ Run ALMAS" or "  Run ALMAS", selected == RUN_ROW and "selected" or "item" },
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
    title = " ALMAS ",
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
      n00n.ui.flash("Enter an ALMAS goal first")
      render()
      return false
    end
    local _, err = n00n.session.prompt(agent_prompt(value, prefs))
    if err then
      n00n.ui.flash("ALMAS: " .. err)
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
    elseif event.type == "paste" and editing then
      local input = editing == "goal" and goal or model
      input:insert_text(event.text)
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
          local finished = editing
          editing = nil
          selected = finished == "model" and 4 or RUN_ROW
          render()
        else
          local input = editing == "goal" and goal or model
          if input:handle_key(key) ~= TextInput.Result.IGNORED then
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
          prefs.mode = cycle(ALMAS_MODES, prefs.mode)
        elseif selected == 2 then
          prefs.model_tier = cycle(MODEL_TIERS, prefs.model_tier)
        elseif selected == 3 then
          editing = "model"
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
          prefs.max_rounds = prefs.max_rounds >= 8 and 2 or prefs.max_rounds + 1
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
  name = "/almas",
  description = "Configure and run an ALMAS team for a goal",
  handler = function(args)
    run_launcher(trim(args))
  end,
})
