local M = {}

local function normalize_tool_name(tool_name)
  if not tool_name or #tool_name == 0 then
    return nil
  end
  return tool_name:gsub("-", "_"):lower()
end

function M.build_envelope(skill)
  if not skill then
    return nil
  end
  local has_allowed = skill.allowed_tools and #skill.allowed_tools > 0
  local has_disallowed = skill.disallowed_tools and #skill.disallowed_tools > 0
  if not has_allowed and not has_disallowed then
    return nil
  end
  return {
    name = skill.name,
    allowed_tools = skill.allowed_tools,
    disallowed_tools = skill.disallowed_tools,
  }
end

function M.evaluate(envelope, tool_name)
  if not envelope then
    return { allowed = true }
  end
  local normalized = normalize_tool_name(tool_name)
  if not normalized then
    return { allowed = false, reason = "tool name is required" }
  end

  if envelope.disallowed_tools then
    for _, denied in ipairs(envelope.disallowed_tools) do
      if normalize_tool_name(denied) == normalized then
        return {
          allowed = false,
          reason = "tool " .. tool_name .. " is disallowed by active skill policy",
        }
      end
    end
  end

  if envelope.allowed_tools and #envelope.allowed_tools > 0 then
    for _, allowed in ipairs(envelope.allowed_tools) do
      if normalize_tool_name(allowed) == normalized then
        return { allowed = true }
      end
    end
    return {
      allowed = false,
      reason = "tool " .. tool_name .. " is not allowed by active skill policy",
    }
  end

  return { allowed = true }
end

function M.instruction_content(envelope)
  if not envelope then
    return nil
  end
  local lines = { "Active skill policy for " .. envelope.name .. ":" }
  if envelope.allowed_tools and #envelope.allowed_tools > 0 then
    lines[#lines + 1] = "- allowed-tools: " .. table.concat(envelope.allowed_tools, ", ")
  end
  if envelope.disallowed_tools and #envelope.disallowed_tools > 0 then
    lines[#lines + 1] = "- disallowed-tools: " .. table.concat(envelope.disallowed_tools, ", ")
  end
  lines[#lines + 1] = "Use skill_policy.call_tool while this skill is active."
  return table.concat(lines, "\n")
end

function M.call_tool(ctx, envelope, tool_name, input)
  local decision = M.evaluate(envelope, tool_name)
  if not decision.allowed then
    return nil, decision.reason or "skill policy blocked tool call"
  end
  return n00n.agent.call_tool(ctx, tool_name, input)
end

return M
