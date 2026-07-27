-- Subagent launch helper module.
-- Provides a unified interface for launching subagents with model resolution,
-- system prompts, tool setup, and optional structured output validation.

local M = {}

local route_tier = require("n00n.route_tier").route_tier
local usage = require("n00n.usage")
local structured_output = require("n00n.structured_output")

-- Launch a subagent with the given options.
-- Returns (result | nil, err, cost, usage)
--
-- Options:
--   description (required): Short description for the subagent
--   prompt (required): The prompt to send to the subagent
--   subagent_type: "research" or "general" (default: "general")
--   model_spec: Exact model spec (optional)
--   model_tier: Capped tier: "weak", "medium", or "strong"
--   auto_tier: Pick model_tier from prompt automatically (optional)
--   thinking: Thinking mode configuration
--   output_schema: JSON Schema for structured output validation
--   audience: Tool audience (default: computed from subagent_type)
--   local_tools: Additional local tools to register
--   ctx: Agent context (required)
function M.launch(ctx, opts)
  if not opts then
    return nil, "opts is required", nil, nil
  end
  if not opts.description then
    return nil, "opts.description is required", nil, nil
  end
  if not opts.prompt then
    return nil, "opts.prompt is required", nil, nil
  end
  if not ctx then
    return nil, "ctx is required", nil, nil
  end

  local subagent_type = opts.subagent_type or "general"
  if subagent_type ~= "research" and subagent_type ~= "general" then
    return nil, "unknown subagent_type: " .. tostring(subagent_type), nil, nil
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
    return nil, model_err, nil, nil
  end

  -- Compute audience and prompt_id
  local audience = opts.audience or (subagent_type == "research" and "research_sub" or "general_sub")
  local prompt_id = subagent_type == "research" and "research" or "general"

  -- Build system prompt
  local system, system_err = n00n.agent.system_prompt(ctx, {
    prompt_id = prompt_id,
    instructions = true,
  })
  if system_err then
    return nil, system_err, nil, nil
  end

  -- Get tool definitions
  local tool_defs, tools_err = n00n.agent.tools(ctx, {
    audience = audience,
    spec = model.spec,
  })
  if tools_err then
    return nil, tools_err, nil, nil
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
      return nil, compile_err, nil, nil
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
    model_spec = model.spec,
    system = system,
    tools = tool_defs,
    local_tools = local_tools,
    audience = audience,
    name = opts.description,
    thinking = opts.thinking,
  })
  if sess_err then
    return nil, sess_err, nil, nil
  end

  -- Build message with structured output suffix if needed
  local message = opts.prompt
  if opts.output_schema then
    message = message .. structured_output.STRUCTURED_OUTPUT_SUFFIX
  end

  -- Run the prompt with retry logic for structured output
  local result, err = sess:prompt(message)
  local retries = 0
  while not err and validator and not captured and retries < structured_output.MAX_STRUCTURED_RETRIES do
    retries = retries + 1
    result, err = sess:prompt(structured_output.NUDGE_MISSING)
  end

  sess:close()

  if err then
    return nil, "sub-agent error: " .. err, nil, result
  end

  if validator and not captured then
    local msg = (last_errors and (structured_output.STRUCTURED_INVALID_ERROR .. ":\n" .. last_errors))
      or structured_output.STRUCTURED_MISSING_ERROR
    return nil, msg, nil, result
  end

  -- Calculate cost and usage
  local measured_usage, cost, metrics_err = usage.price(model.spec, result)
  if metrics_err then
    -- Return result even if pricing fails
    return captured or result.text, nil, nil, measured_usage
  end

  return captured or result.text, nil, cost, measured_usage
end

return M
