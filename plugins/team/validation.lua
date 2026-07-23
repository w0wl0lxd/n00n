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
    output_parts[#output_parts + 1] = string.format(
      "Step %d (%s) output:\n%s",
      entry.index,
      step.role or "unknown",
      step.text or step.error or "(no output)"
    )
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
  local upper_response = response:upper()
  if upper_response:find("^PASS") or upper_response:find("\nPASS") then
    return true
  else
    local explanation = response:match("FAIL%s*(.+)") or response
    return nil, explanation
  end
end

return M
