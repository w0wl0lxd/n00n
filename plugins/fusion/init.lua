-- Fusion sidekick delegation (Cognition Devin Fusion pattern).
-- Lead agent delegates via spec-quality briefs; sidekick runs in an isolated
-- cached context on a cost-aware tier (auto_tier on by default).

local subagent = require("n00n.subagent")

local description =
  [[Delegate to a Fusion sidekick. Pass goal, constraints, and definition_of_done — not file dumps.]]

local schema = {
  type = "object",
  required = { "description", "goal", "definition_of_done" },
  additionalProperties = false,
  properties = {
    description = {
      type = "string",
      description = "Short label (3-5 words).",
    },
    goal = {
      type = "string",
      description = "What to accomplish.",
    },
    constraints = {
      type = "string",
      description = "Scope and patterns.",
    },
    definition_of_done = {
      type = "string",
      description = "Success checks (tests, artifacts).",
    },
    escalation_triggers = {
      type = "string",
      description = "When to escalate to the lead.",
    },
    subagent_type = {
      type = "string",
      description = "research (read-only) or general (edit). Default: general.",
    },
    model_tier = {
      type = "string",
      description = "weak/medium/strong override.",
    },
    model = {
      type = "string",
      description = "Exact model override.",
    },
    auto_tier = {
      type = "boolean",
      description = "Tier from brief (default: true).",
    },
  },
}

local opts = n00n.api.register_options({
  auto_tier = { default = true, desc = "Route sidekick tier from the brief." },
  default_subagent_type = { default = "general", desc = "Default subagent_type when omitted." },
})

local function build_prompt(input)
  local parts = {
    "# Fusion sidekick brief\n",
    "## Goal\n",
    input.goal,
    "\n",
  }
  if input.constraints and input.constraints ~= "" then
    parts[#parts + 1] = "## Constraints\n"
    parts[#parts + 1] = input.constraints
    parts[#parts + 1] = "\n"
  end
  parts[#parts + 1] = "## Definition of done\n"
  parts[#parts + 1] = input.definition_of_done
  parts[#parts + 1] = "\n"
  if input.escalation_triggers and input.escalation_triggers ~= "" then
    parts[#parts + 1] = "## Escalate to lead when\n"
    parts[#parts + 1] = input.escalation_triggers
    parts[#parts + 1] = "\n"
  end
  parts[#parts + 1] =
    "\nExecute efficiently. Prefer index/codegraph/arbor before broad reads. Return concise file:line evidence, test/lint results, and a short summary — not full file dumps."
  return table.concat(parts)
end

local function handler(input, ctx)
  local subagent_type = input.subagent_type or opts.default_subagent_type
  if subagent_type ~= "research" and subagent_type ~= "general" then
    return { llm_output = "unknown subagent_type: " .. tostring(subagent_type), is_error = true }
  end

  local auto_tier = input.auto_tier
  if auto_tier == nil then
    auto_tier = opts.auto_tier
  end

  local model_tier = input.model_tier
  if not input.model and not model_tier then
    local fusion_config = ctx:config("fusion")
    model_tier = fusion_config and fusion_config.sidekick_tier or nil
  end

  local prompt = build_prompt(input)
  local result, err, cost, usage, model_spec = subagent.launch(ctx, {
    description = input.description,
    prompt = prompt,
    subagent_type = subagent_type,
    model_spec = input.model,
    model_tier = model_tier,
    auto_tier = auto_tier,
    audience = "general_sub",
  })

  if err then
    return { llm_output = sanitize_error(err), is_error = true, cost = cost, usage = usage, model = model_spec }
  end

  local footer = ""
  if cost and model_spec then
    footer = string.format("\n\n[sidekick cost: $%.4f · %s]", cost, model_spec)
  end

  return {
    llm_output = tostring(result) .. footer,
    cost = cost,
    usage = usage,
    model = model_spec,
  }
end

local function header(input)
  local label = input.description or ""
  if utf8 and utf8.len(label) > 40 then
    local offset = utf8.offset(label, 41)
    if offset then
      label = string.sub(label, 1, offset - 1)
    end
  end
  return "Executing: " .. label
end

n00n.api.register_tool({
  name = "fusion_delegate",
  description = description,
  schema = schema,
  admission = "orchestrator",
  handler = handler,
  header = header,
  audiences = { "main" },
  kind = "execute",
})
