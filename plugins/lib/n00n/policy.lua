-- Policy enforcement wrapper for tool calls.
local M = {}

local ok, memory_helpers = pcall(require, "memory.memory_helpers")

local function project_id()
  if ok and memory_helpers then
    local cwd = n00n.uv.cwd()
    local root = n00n.fs.root(cwd, ".git") or cwd
    return memory_helpers.project_id(root)
  end
  local cwd = n00n.uv.cwd()
  local base = n00n.fs.basename(cwd) or "root"
  return base .. "-default"
end

local function policies_path()
  local state = n00n.env.state_dir()
  if not state then
    return nil, "cannot resolve state dir"
  end
  return n00n.fs.joinpath(state, "projects/" .. project_id() .. "/policies/policy.json")
end

local function load_policies()
  local path, err = policies_path()
  if not path then
    return { version = 1, rules = {} }
  end

  local content, read_err = n00n.fs.read(path)
  if not content then
    return { version = 1, rules = {} }
  end

  local decoded, dec_err = n00n.json.decode(content)
  if not decoded then
    return { version = 1, rules = {} }
  end

  return decoded
end

local function evaluate_policy_direct(agent_id, session_type, tags, tool_name)
  local policies = load_policies()
  if not policies or not policies.rules or #policies.rules == 0 then
    return { allowed = true }
  end

  local matched_rules = {}
  for _, rule in ipairs(policies.rules) do
    local matches = false
    local scope = rule.scope or {}

    if scope.agent_id and scope.agent_id == agent_id then
      matches = true
    elseif scope.session_type and scope.session_type == session_type then
      matches = true
    elseif scope.tag and tags then
      for _, tag in ipairs(tags) do
        if tag == scope.tag then
          matches = true
          break
        end
      end
    end

    if matches then
      matched_rules[#matched_rules + 1] = rule
    end
  end

  if #matched_rules == 0 then
    return { allowed = true }
  end

  table.sort(matched_rules, function(a, b)
    if (a.priority or 0) ~= (b.priority or 0) then
      return (a.priority or 0) > (b.priority or 0)
    end
    local a_specificity = 0
    local b_specificity = 0
    if a.scope.agent_id then
      a_specificity = 3
    elseif a.scope.session_type then
      a_specificity = 2
    elseif a.scope.tag then
      a_specificity = 1
    end
    if b.scope.agent_id then
      b_specificity = 3
    elseif b.scope.session_type then
      b_specificity = 2
    elseif b.scope.tag then
      b_specificity = 1
    end
    return a_specificity > b_specificity
  end)

  local rule = matched_rules[1]

  if rule.paused then
    return { allowed = false, reason = "agent paused by policy" }
  end

  if rule.restricted_tools and #rule.restricted_tools > 0 then
    for _, restricted in ipairs(rule.restricted_tools) do
      if restricted == tool_name then
        return { allowed = false, reason = "tool " .. tool_name .. " is restricted by policy" }
      end
    end
  end

  if rule.allowed_tools and #rule.allowed_tools > 0 then
    local allowed = false
    for _, allowed_tool in ipairs(rule.allowed_tools) do
      if allowed_tool == tool_name then
        allowed = true
        break
      end
    end
    if not allowed then
      return { allowed = false, reason = "tool " .. tool_name .. " is not in policy allowlist" }
    end
  end

  return { allowed = true }
end

function M.call_tool(ctx, agent_id, session_type, tags, tool_name, input)
  local policy_result = evaluate_policy_direct(agent_id, session_type, tags, tool_name)
  if not policy_result.allowed then
    return nil, policy_result.reason or "policy blocked tool call"
  end

  return n00n.agent.call_tool(ctx, tool_name, input)
end

return M
