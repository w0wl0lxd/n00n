-- Validation gate: ask a strong model if wave output satisfies acceptance criteria.
local M = {}

function M.validate_wave(ctx, wave_result, goal, input)
  if not wave_result or not wave_result.steps then
    return nil, "invalid wave result"
  end

  local parts = {}
  for _, entry in ipairs(wave_result.steps) do
    local step = entry.step
    local step_text = string.format("Step %d (%s):", entry.index, step.role or "unknown")
    if step.acceptance_criteria and #step.acceptance_criteria > 0 then
      step_text = step_text .. "\nAcceptance criteria: " .. step.acceptance_criteria
    end
    parts[#parts + 1] = step_text
  end
  local criteria_text = table.concat(parts, "\n\n")

  local output_parts = {}
  for _, entry in ipairs(wave_result.steps) do
    local step = entry.step
    local step_output = wave_result.step_outputs and wave_result.step_outputs[entry.index] or "(no output)"
    output_parts[#output_parts + 1] =
      string.format("Step %d (%s) output:\n%s", entry.index, step.role or "unknown", step_output)
  end
  local output = table.concat(output_parts, "\n\n")

  local prompt = "You are a quality gate validator for the "
    .. wave_result.wave_name
    .. " wave of an ALMAS multi-agent software engineering run.\n\n"
    .. "Goal:\n"
    .. goal
    .. "\n\n"
    .. "Acceptance criteria for this wave:\n"
    .. criteria_text
    .. "\n\n"
    .. "Wave output:\n"
    .. output
    .. "\n\n"
    .. "Evaluate whether the wave output satisfies the acceptance criteria. "
    .. "Respond with exactly 'PASS' if the output is acceptable, or 'FAIL' followed by a brief explanation if not."

  local sess, sess_err = n00n.agent.session(ctx, {
    model_spec = input.model or "strong",
    system = "You are a code review validator. Respond with PASS or FAIL.",
    tools = {},
    audience = "general_sub",
  })
  if sess_err then
    return nil, "validation session error: " .. sess_err
  end

  local res, rerr = sess:prompt(prompt)
  sess:close()

  if rerr then
    return nil, "validation prompt error: " .. rerr
  end

  local response = res or ""
  local trimmed = response:match("^%s*(.-)%s*$") or response
  local first_word = trimmed:match("^%S+") or ""
  local upper_word = first_word:upper()
  if upper_word == "PASS" or upper_word == "PASS." or upper_word == "PASS:" or upper_word == "PASSED" then
    return true
  else
    local explanation = trimmed:match("^FAIL%s*(.+)") or trimmed
    return nil, explanation
  end
end

return M
