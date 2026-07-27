-- Structured output helper module for subagent validation.
-- Provides constants, schema validation, and local tool creation for
-- structured output patterns used across task, workflow, and subagent plugins.

local M = {}

-- Constants
M.STRUCTURED_OUTPUT_NAME = "structured_output"
M.STRUCTURED_OUTPUT_DESCRIPTION = "Report your final result. Call it exactly once when your task is complete."
M.STRUCTURED_OUTPUT_ACK = "Output recorded."
M.STRUCTURED_OUTPUT_SUFFIX = "\n\nWhen finished, call the structured_output tool with your final result."
M.MAX_STRUCTURED_RETRIES = 1
M.MAX_SCHEMA_ERRORS = 3
M.MAX_SCHEMA_BYTES = 32 * 1024
M.MAX_SCHEMA_DEPTH = 16
M.SCHEMA_ROOT_ERROR = "output_schema must have type object"
M.SCHEMA_COMPILE_ERROR = "invalid output_schema"
M.SCHEMA_SIZE_ERROR = "output_schema exceeds 32768-byte limit"
M.SCHEMA_DEPTH_ERROR = "output_schema exceeds maximum depth of 16"
M.STRUCTURED_MISSING_ERROR = "subagent finished without calling structured_output"
M.STRUCTURED_INVALID_ERROR = "subagent result does not match output_schema"
M.NUDGE_MISSING =
  "You did not call the structured_output tool. Call it now with your final result matching its input schema."
M.INVALID_INPUT_PREFIX = "Input does not match the required schema. Fix the errors and call structured_output again:\n"

-- Check if a schema value is within the maximum depth limit
function M.schema_within_depth(value, depth)
  if type(value) ~= "table" then
    return true
  end
  if depth > M.MAX_SCHEMA_DEPTH then
    return false
  end
  for _, child in pairs(value) do
    if not M.schema_within_depth(child, depth + 1) then
      return false
    end
  end
  return true
end

-- Limit error messages to a reasonable number
function M.bounded_errors(errors)
  local out = {}
  for i = 1, math.min(#errors, M.MAX_SCHEMA_ERRORS) do
    out[i] = errors[i]
  end
  return table.concat(out, "\n")
end

-- Compile a schema validator with early validation checks
-- Returns (validator | nil, err)
function M.compile_validator(schema)
  if type(schema) ~= "table" or schema.type ~= "object" then
    return nil, M.SCHEMA_ROOT_ERROR
  end

  local schema_json, encode_err = n00n.json.encode(schema)
  if encode_err then
    return nil, M.SCHEMA_COMPILE_ERROR .. ": " .. encode_err
  end

  if #schema_json > M.MAX_SCHEMA_BYTES then
    return nil, M.SCHEMA_SIZE_ERROR
  end

  if not M.schema_within_depth(schema, 1) then
    return nil, M.SCHEMA_DEPTH_ERROR
  end

  local validator, compile_err = n00n.json.schema_validator(schema)
  if compile_err then
    return nil, M.SCHEMA_COMPILE_ERROR .. ": " .. compile_err
  end

  return validator, nil
end

-- Create a local tool spec for structured output
-- Returns a table with description, input_schema, and handler
function M.make_local_tool(schema, on_submit)
  return {
    description = M.STRUCTURED_OUTPUT_DESCRIPTION,
    input_schema = schema,
    handler = function(value)
      local validator, compile_err = M.compile_validator(schema)
      if compile_err then
        return nil, compile_err
      end

      local errs = validator:validate(value)
      if errs then
        local bounded = M.bounded_errors(errs)
        return nil, M.INVALID_INPUT_PREFIX .. bounded
      end

      if on_submit then
        on_submit(value)
      end

      return M.STRUCTURED_OUTPUT_ACK
    end,
  }
end

-- Run a session with structured output validation and retry logic
-- Returns (result | nil, err)
function M.run_with_validation(sess, message, validator, max_retries)
  max_retries = max_retries or M.MAX_STRUCTURED_RETRIES

  local captured
  local last_errors

  -- Set up local tool if validator is provided
  local local_tools
  if validator then
    local_tools = {
      [M.STRUCTURED_OUTPUT_NAME] = {
        description = M.STRUCTURED_OUTPUT_DESCRIPTION,
        input_schema = validator.schema, -- Note: validator may not expose schema, this is a placeholder
        handler = function(value)
          local errs = validator:validate(value)
          if errs then
            last_errors = M.bounded_errors(errs)
            return nil, M.INVALID_INPUT_PREFIX .. last_errors
          end
          captured = value
          return M.STRUCTURED_OUTPUT_ACK
        end,
      },
    }
  end

  -- Add suffix to message if validator is present
  local full_message = message
  if validator then
    full_message = message .. M.STRUCTURED_OUTPUT_SUFFIX
  end

  -- Initial prompt
  local result, err = sess:prompt(full_message)

  -- Retry loop for missing structured_output calls
  local retries = 0
  while not err and validator and not captured and retries < max_retries do
    retries = retries + 1
    result, err = sess:prompt(M.NUDGE_MISSING)
  end

  if err then
    return nil, err
  end

  if validator and not captured then
    local msg = last_errors and (M.STRUCTURED_INVALID_ERROR .. ":\n" .. last_errors) or M.STRUCTURED_MISSING_ERROR
    return nil, msg
  end

  if captured then
    return captured, nil
  end

  return result.text, nil
end

return M
