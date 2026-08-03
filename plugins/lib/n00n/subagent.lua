-- Subagent launch helper module.
-- Provides a unified interface for launching subagents with model resolution,
-- system prompts, tool setup, and optional structured output validation.

local M = {}

local route_tier = require("n00n.route_tier").route_tier
local usage = require("n00n.usage")
local structured_output = require("n00n.structured_output")

local ORCHESTRATION_TOOLS = { "task", "team", "workflow", "agent_control", "batch" }

local function excluded_tools(opts)
  local excluded = {}
  local seen = {}
  if not opts.allow_orchestration then
    for _, name in ipairs(ORCHESTRATION_TOOLS) do
      excluded[#excluded + 1] = name
      seen[name] = true
    end
  end
  for _, name in ipairs(opts.except_tools or {}) do
    if not seen[name] then
      excluded[#excluded + 1] = name
      seen[name] = true
    end
  end
  return excluded
end

-- Launch a subagent with the given options.
-- Returns (result | nil, err, cost, usage, model_spec)
--
-- Options:
--   description (required): Short description for the subagent
--   prompt (required): The prompt to send to the subagent
--   subagent_type: "research" or "general" (default: "general")
--   model_spec: Exact model spec (optional)
--   model_tier: Capped tier: "weak", "medium", or "strong"
--   auto_tier: Pick model_tier from prompt automatically (optional)
--   thinking: Thinking mode configuration
--   system: Override the default system prompt (optional)
--   output_schema: JSON Schema for structured output validation
--   audience: Tool audience (default: computed from subagent_type)
--   include_mcp: Include MCP tools (default: true)
--   only_tools: Optional allowlist of tool names
--   except_tools: Optional denylist of tool names
--   allow_orchestration: Expose recursive orchestration tools (default: false)
--   system_append: Trusted instruction appended to the system prompt
--   local_tools: Additional local tools to register
--   preview: ActivityPreview object wrapping sess:prompt (optional)
--   activity_label: Label used with preview (default: description)
--   budget: Budget object with :consume() method (optional)
--   fail_on_pricing_error: Return an error if usage pricing fails (default: false)
--   ctx: Agent context (required)
function M.launch(ctx, opts)
  if not opts then
    return nil, "opts is required", nil, nil, nil
  end
  if not opts.description then
    return nil, "opts.description is required", nil, nil, nil
  end
  if not opts.prompt then
    return nil, "opts.prompt is required", nil, nil, nil
  end
  if not ctx then
    return nil, "ctx is required", nil, nil, nil
  end
  if opts.system_append ~= nil and type(opts.system_append) ~= "string" then
    return nil, "opts.system_append must be a string", nil, nil, nil
  end

  local function guard_check(prompt)
    if not opts.budget then
      return true
    end
    if opts.budget.check then
      return opts.budget:check(prompt)
    end
    if opts.budget.consume then
      return opts.budget:consume()
    end
    return true
  end

  local function guard_record(prompt, err)
    if not opts.budget then
      return true
    end
    if opts.budget.record then
      return opts.budget:record(prompt, err)
    end
    if opts.budget.observe then
      return opts.budget:observe(prompt, err)
    end
    return true
  end

  -- Runaway-guard check before any expensive setup work (model resolution, etc.)
  local guard_ok, guard_err = guard_check(opts.prompt)
  if not guard_ok then
    return nil, guard_err, nil, nil, nil
  end

  local subagent_type = opts.subagent_type or "general"
  if subagent_type ~= "research" and subagent_type ~= "general" then
    return nil, "unknown subagent_type: " .. tostring(subagent_type), nil, nil, nil
  end

  -- Resolve model tier (auto_tier overrides model_tier when no explicit spec)
  local model_tier = opts.model_tier
  if not opts.model_spec and opts.auto_tier == true then
    model_tier = route_tier(opts.prompt)
  end

  -- Resolve model
  local model, model_err = n00n.agent.resolve_model(ctx, {
    spec = opts.model_spec,
    tier = not opts.model_spec and model_tier or nil,
  })
  if model_err then
    return nil, model_err, nil, nil, nil
  end
  local model_spec = model.spec

  -- Compute audience and prompt_id
  local audience = opts.audience or (subagent_type == "research" and "research_sub" or "general_sub")
  local prompt_id = subagent_type == "research" and "research" or "general"

  -- Build system prompt
  local system
  if opts.system then
    system = opts.system
  else
    local system_err
    system, system_err = n00n.agent.system_prompt(ctx, {
      prompt_id = prompt_id,
      instructions = true,
    })
    if system_err then
      return nil, system_err, nil, nil, model_spec
    end
  end
  if opts.system_append then
    system = system .. "\n\n" .. opts.system_append
  end

  -- Get tool definitions
  local excluded = excluded_tools(opts)

  local tool_defs, tools_err = n00n.agent.tools(ctx, {
    audience = audience,
    spec = model_spec,
    only = opts.only_tools,
    except = excluded,
    include_mcp = opts.include_mcp,
  })

  if tools_err then
    return nil, tools_err, nil, nil, model_spec
  end

  -- Set up local tools
  local local_tools = opts.local_tools or {}
  local validator
  local captured
  local last_errors

  -- Add structured output tool if schema is provided
  if opts.output_schema then
    local compile_err
    validator, compile_err = structured_output.compile_validator(opts.output_schema)
    if compile_err then
      return nil, compile_err, nil, nil, model_spec
    end

    local_tools[structured_output.STRUCTURED_OUTPUT_NAME] = {
      description = structured_output.STRUCTURED_OUTPUT_DESCRIPTION,
      input_schema = opts.output_schema,
      handler = function(value)
        local errs = validator:validate(value)
        if errs then
          last_errors = structured_output.bounded_errors(errs)
          return nil, structured_output.INVALID_INPUT_PREFIX .. last_errors
        end
        captured = value
        return structured_output.STRUCTURED_OUTPUT_ACK
      end,
    }
  end

  -- Create session
  local sess, sess_err = n00n.agent.session(ctx, {
    model_spec = model_spec,
    system = system,
    tools = tool_defs,
    local_tools = local_tools,
    audience = audience,
    name = opts.description,
    thinking = opts.thinking,
    mode = subagent_type,
    include_mcp = opts.include_mcp,
    except = excluded,
  })
  if sess_err then
    return nil, sess_err, nil, nil, model_spec
  end

  -- Build message with structured output suffix if needed
  local message = opts.prompt
  if opts.output_schema then
    message = message .. structured_output.STRUCTURED_OUTPUT_SUFFIX
  end

  local preview = opts.preview
  local label = opts.activity_label or opts.description

  -- Run the prompt with retry logic for structured output
  local result, err
  if preview then
    result, err = preview:prompt(sess, message, label)
  else
    result, err = sess:prompt(message)
  end

  -- Record result for runaway heuristics
  local record_ok, record_err = guard_record(opts.prompt, err)
  if not record_ok then
    sess:close()
    local charged_usage, charged_cost = usage.price(model_spec, result)
    return nil, record_err, charged_cost, charged_usage, model_spec
  end
  local retries = 0
  while not err and validator and not captured and retries < structured_output.MAX_STRUCTURED_RETRIES do
    retries = retries + 1
    if preview then
      result, err = preview:prompt(sess, structured_output.NUDGE_MISSING, label)
    else
      result, err = sess:prompt(structured_output.NUDGE_MISSING)
    end
  end

  sess:close()

  local priced_usage, cost, metrics_err = usage.price(model_spec, result)

  if err then
    return nil, "sub-agent error: " .. err, cost, priced_usage, model_spec
  end

  if validator and not captured then
    local msg = (last_errors and (structured_output.STRUCTURED_INVALID_ERROR .. ":\n" .. last_errors))
      or structured_output.STRUCTURED_MISSING_ERROR
    return nil, msg, cost, priced_usage, model_spec
  end

  -- Calculate cost and usage
  if metrics_err then
    if opts.fail_on_pricing_error then
      return nil, "pricing failed: " .. metrics_err, nil, priced_usage, model_spec
    end
    -- Return result even if pricing fails
    return captured or result.text, nil, nil, priced_usage, model_spec
  end

  return captured or result.text, nil, cost, priced_usage, model_spec
end

return M
