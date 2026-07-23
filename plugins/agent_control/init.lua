local ok, memory_helpers = pcall(require, "memory.memory_helpers")
local policy_ok, policy = pcall(require, "n00n.policy")

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

local function validate_id(id)
  if not id or id == "" then
    return nil, "id is required"
  end
  if #id > 128 then
    return nil, "id exceeds maximum length of 128"
  end
  if id:find("%.%.") or id:find("/") or id:find("\\") or id:find("%z") or id:find("%c") then
    return nil, "id contains invalid characters (path traversal, control chars, or null not allowed)"
  end
  if id:find("[^%w%-%_.]") then
    return nil, "id contains invalid characters (only alphanumeric, dash, underscore, dot allowed)"
  end
  return true
end

local description = [[Control background agents started by task, team, or workflow.

Actions: list, status, message, pause, resume, stop, policy.]]

local schema = {
  type = "object",
  required = { "action" },
  additionalProperties = false,
  properties = {
    action = {
      type = "string",
      enum = { "list", "status", "message", "pause", "resume", "stop", "policy" },
      description = "Control action.",
    },
    agent_id = {
      type = "string",
      description = "Background agent id.",
    },
    message = {
      type = "string",
      description = "Steering instructions.",
    },
    policy = {
      type = "object",
      description = "Policy data for policy action.",
      properties = {
        action = {
          type = "string",
          enum = { "set", "get", "delete", "list" },
          description = "Policy action.",
        },
        rule = {
          type = "object",
          description = "Policy rule for set action.",
          properties = {
            id = { type = "string", description = "Unique policy identifier." },
            scope = {
              type = "object",
              description = "Policy scope.",
              properties = {
                tag = { type = "string", description = "Applies to agents with this tag." },
                session_type = { type = "string", description = "Applies to sessions of this type." },
                agent_id = { type = "string", description = "Applies to a specific agent." },
              },
            },
            restricted_tools = {
              type = "array",
              items = { type = "string" },
              description = "Tools that agents in scope cannot use.",
            },
            allowed_tools = {
              type = "array",
              items = { type = "string" },
              description = "Tools that agents in scope can use (whitelist mode).",
            },
            paused = { type = "boolean", description = "Whether agents in scope are paused." },
            priority = { type = "integer", description = "Policy priority (higher wins on conflict)." },
          },
          required = { "id", "scope", "priority" },
        },
        rule_id = {
          type = "string",
          description = "Policy rule ID for get/delete action.",
        },
      },
    },
  },
}

local function policies_dir()
  local state = n00n.env.state_dir()
  if not state then
    return nil, "cannot resolve state dir"
  end
  return n00n.fs.joinpath(state, "projects/" .. project_id() .. "/policies")
end

local function policies_path()
  local dir, err = policies_dir()
  if not dir then
    return nil, err
  end
  return n00n.fs.joinpath(dir, "policy.json")
end

local function load_policies()
  local path, err = policies_path()
  if not path then
    return nil, err
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

local function save_policies(policies)
  local dir, err = policies_dir()
  if not dir then
    return nil, err
  end

  n00n.fs.mkdir(dir, { parents = true })

  local path = n00n.fs.joinpath(dir, "policy.json")
  local content, enc_err = n00n.json.encode(policies)
  if not content then
    return nil, "encode error: " .. tostring(enc_err)
  end

  local write_ok, write_err = n00n.fs.write(path, content)
  if not write_ok then
    return nil, "write error: " .. tostring(write_err)
  end

  return true
end

local function policy_set(rule)
  if not rule.id or rule.id == "" then
    return nil, "rule.id is required"
  end
  local ok, vid = validate_id(rule.id)
  if not ok then
    return nil, "rule.id: " .. vid
  end
  if not rule.scope then
    return nil, "rule.scope is required"
  end
  if type(rule.scope) ~= "table" then
    return nil, "rule.scope must be a table"
  end
  if not rule.priority then
    return nil, "rule.priority is required"
  end

  local scope_keys = 0
  local valid_keys = { tag = true, session_type = true, agent_id = true }
  for key, value in pairs(rule.scope) do
    if not valid_keys[key] then
      return nil, "rule.scope has unknown key: " .. tostring(key)
    end
    if value then
      scope_keys = scope_keys + 1
    end
  end
  if scope_keys ~= 1 then
    return nil, "rule.scope must have exactly one of tag, session_type, or agent_id"
  end

  if rule.restricted_tools and rule.allowed_tools then
    return nil, "restricted_tools and allowed_tools are mutually exclusive"
  end

  local policies = load_policies()

  local found = false
  for i, existing in ipairs(policies.rules) do
    if existing.id == rule.id then
      policies.rules[i] = rule
      found = true
      break
    end
  end

  if not found then
    policies.rules[#policies.rules + 1] = rule
  end

  local ok, err = save_policies(policies)
  if not ok then
    return nil, err
  end

  return rule
end

local function policy_get(rule_id)
  if not rule_id or rule_id == "" then
    return nil, "rule_id is required"
  end

  local policies = load_policies()
  for _, rule in ipairs(policies.rules) do
    if rule.id == rule_id then
      return rule
    end
  end

  return nil, "policy not found: " .. rule_id
end

local function policy_delete(rule_id)
  if not rule_id or rule_id == "" then
    return nil, "rule_id is required"
  end

  local policies = load_policies()
  local new_rules = {}
  local found = false
  for _, rule in ipairs(policies.rules) do
    if rule.id == rule_id then
      found = true
    else
      new_rules[#new_rules + 1] = rule
    end
  end

  if not found then
    return nil, "policy not found: " .. rule_id
  end

  policies.rules = new_rules
  local ok, err = save_policies(policies)
  if not ok then
    return nil, err
  end

  return true
end

local function policy_list()
  local policies = load_policies()
  return policies.rules
end

local function find_agent(id)
  local agents, err = n00n.session.live()
  if not agents then
    return nil, err
  end
  for _, agent in ipairs(agents) do
    if agent.id == id then
      return agent
    end
  end
  return nil, "background agent is not live: " .. id
end

local function handler(input)
  if input.action == "list" then
    local agents, err = n00n.session.live()
    if not agents then
      return { llm_output = err, is_error = true }
    end
    return n00n.json.encode(agents)
  end

  if input.action == "policy" then
    if not input.policy or not input.policy.action then
      return { llm_output = "policy.action is required for policy action", is_error = true }
    end

    local paction = input.policy.action

    if paction == "set" then
      if not input.policy.rule then
        return { llm_output = "policy.rule is required for set", is_error = true }
      end
      local rule, err = policy_set(input.policy.rule)
      if not rule then
        return { llm_output = "Error: " .. tostring(err), is_error = true }
      end
      local encoded, enc_err = n00n.json.encode(rule)
      if not encoded then
        return { llm_output = "Policy set: " .. rule.id, policy = rule }
      end
      return { llm_output = encoded, policy = rule }
    elseif paction == "get" then
      if not input.policy.rule_id then
        return { llm_output = "policy.rule_id is required for get", is_error = true }
      end
      local rule, err = policy_get(input.policy.rule_id)
      if not rule then
        return { llm_output = "Error: " .. tostring(err), is_error = true }
      end
      local encoded, enc_err = n00n.json.encode(rule)
      if not encoded then
        return { llm_output = "Error: encode failed", is_error = true }
      end
      return { llm_output = encoded, policy = rule }
    elseif paction == "delete" then
      if not input.policy.rule_id then
        return { llm_output = "policy.rule_id is required for delete", is_error = true }
      end
      local ok, err = policy_delete(input.policy.rule_id)
      if not ok then
        return { llm_output = "Error: " .. tostring(err), is_error = true }
      end
      return { llm_output = "Policy deleted: " .. input.policy.rule_id }
    elseif paction == "list" then
      local rules = policy_list()
      local encoded, enc_err = n00n.json.encode(rules)
      if not encoded then
        return { llm_output = "Error: encode failed", is_error = true }
      end
      return { llm_output = encoded, policies = rules }
    else
      return { llm_output = "Error: unknown policy action " .. paction, is_error = true }
    end
  end

  if not input.agent_id or input.agent_id == "" then
    return { llm_output = "agent_id is required for " .. input.action, is_error = true }
  end

  if input.action == "status" then
    local agent, err = n00n.session.status(input.agent_id)
    if not agent then
      return { llm_output = err, is_error = true }
    end
    return n00n.json.encode(agent)
  end

  if input.action == "message" or input.action == "resume" then
    if not input.message or input.message == "" then
      return { llm_output = "message is required for " .. input.action, is_error = true }
    end

    local session_type = nil
    local tags = nil
    if policy_ok and policy then
      local status, status_err = n00n.session.status(input.agent_id)
      if status then
        session_type = status.session_type
        tags = status.tags
      end
      local policy_result = policy.evaluate_policy(input.agent_id, session_type, tags, "session.prompt")
      if not policy_result.allowed then
        return { llm_output = "Policy blocked: " .. (policy_result.reason or "unknown"), is_error = true }
      end
    end

    local state, err = n00n.session.prompt(input.message, { session = input.agent_id })
    if not state then
      return { llm_output = err, is_error = true }
    end
    return n00n.json.encode({ agent_id = input.agent_id, action = input.action, state = state })
  end

  if input.action == "pause" then
    local stopped, err = n00n.session.cancel(input.agent_id)
    if not stopped then
      return { llm_output = err, is_error = true }
    end
    return n00n.json.encode({ agent_id = input.agent_id, paused = true })
  end

  local stopped, err = n00n.session.cancel(input.agent_id)
  if not stopped then
    return { llm_output = err, is_error = true }
  end
  return n00n.json.encode({ agent_id = input.agent_id, stopped = true })
end

n00n.api.register_tool({
  name = "agent_control",
  description = description,
  kind = "execute",
  audiences = { "main" },
  schema = schema,
  handler = handler,
})
