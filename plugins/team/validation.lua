-- Validation gate: ask a strong model if wave output satisfies acceptance criteria.
local M = {}

function M.validate_wave(ctx, wave_result, goal, input)
  if not wave_result or not wave_result.steps then
    return nil, "invalid wave result"
  end

  local parts = {}
  for _, step in ipairs(wave_result.steps) do
    parts[#parts + 1] = step.text or step.error or ""
  end
  local output = table.concat(parts, "\n\n")

  local prompt = "Review the following output from the "
    .. wave_result.wave_name
    .. " wave. "
    .. "Determine if it satisfies the acceptance criteria for the goal: "
    .. goal
    .. "\n\n"
    .. "Output:\n"
    .. output
    .. "\n\nRespond with 'PASS' if the output is acceptable, or 'FAIL' with a brief explanation if not."

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
  if response:upper():find("PASS") then
    return true
  else
    return nil, "validation failed: " .. response
  end
end

return M
