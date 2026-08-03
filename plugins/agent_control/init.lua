-- Scoped agent control tools (list / status / control).
-- Encoding: n00n.json.tooned for structured payloads; plain text for acks;
-- on-disk policy.json stays JSON. No raw JSON dumps in the TUI body.

local helpers = require("agent_control_helpers")
local validate_id = helpers.validate_id
local agent_line = helpers.agent_line

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

local function encode_structured(value)
  local encoded, fmt = n00n.json.tooned(value)
  if not encoded then
    local fallback, err = n00n.json.encode(value)
    if not fallback then
      return nil, err or "encode failed"
    end
    return fallback, "json"
  end
  return encoded, fmt or "toon"
end

local function card(title, lines, annotation)
  local buf = n00n.ui.buf()
  buf:line({ { title, "bold" } })
  for _, line in ipairs(lines or {}) do
    buf:line({ { "  " .. line, "dim" } })
  end
  return buf, annotation
end

---------------------------------------------------------------------------
-- Policy storage (JSON on disk — intentional)
---------------------------------------------------------------------------

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
  local path = policies_path()
  if not path then
    return { version = 1, rules = {} }
  end
  local content = n00n.fs.read(path)
  if not content then
    return { version = 1, rules = {} }
  end
  local decoded = n00n.json.decode(content)
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
  local vok, vid = validate_id(rule.id)
  if not vok then
    return nil, "rule.id: " .. vid
  end
  if not rule.scope or type(rule.scope) ~= "table" then
    return nil, "rule.scope is required"
  end
  local scope_ok, scope_err = helpers.policy_scope_keys(rule)
  if not scope_ok then
    return nil, scope_err
  end
  if not rule.priority then
    return nil, "rule.priority is required"
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
  local sok, serr = save_policies(policies)
  if not sok then
    return nil, serr
  end
  return rule
end

local function policy_get(rule_id)
  if not rule_id or rule_id == "" then
    return nil, "rule_id is required"
  end
  for _, rule in ipairs(load_policies().rules) do
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
  local sok, serr = save_policies(policies)
  if not sok then
    return nil, serr
  end
  return true
end

---------------------------------------------------------------------------
-- agent_list
---------------------------------------------------------------------------

n00n.api.register_tool({
  name = "agent_list",
  description = "List live background agents (task/team/workflow sessions).",
  kind = "execute",
  admission = "cheap",
  audiences = { "main" },
  schema = {
    type = "object",
    additionalProperties = false,
    properties = {},
  },
  header = function()
    return "agent_list"
  end,
  handler = function()
    local agents, err = n00n.session.live()
    if not agents then
      return { llm_output = err, is_error = true }
    end
    local current, _ = n00n.session.current()
    current = tostring(current or "")
    local filtered = {}
    for _, agent in ipairs(agents) do
      if tostring(agent.id or "") ~= current and not agent.focused then
        filtered[#filtered + 1] = agent
      end
    end
    local lines = {}
    for _, agent in ipairs(filtered) do
      lines[#lines + 1] = agent_line(agent)
    end
    if #lines == 0 then
      lines[1] = "(no live agents)"
    end
    local encoded, fmt = encode_structured({ agents = filtered, count = #filtered })
    if not encoded then
      return { llm_output = "encode failed", is_error = true }
    end
    local body, annotation = card(string.format("list · %d agents", #filtered), lines, tostring(#filtered))
    return {
      llm_output = encoded,
      format = "plain",
      body = body,
      annotation = annotation .. " (" .. fmt .. ")",
    }
  end,
})

---------------------------------------------------------------------------
-- agent_status
---------------------------------------------------------------------------

n00n.api.register_tool({
  name = "agent_status",
  description = "Show status for one live background agent.",
  kind = "execute",
  admission = "cheap",
  audiences = { "main" },
  schema = {
    type = "object",
    required = { "agent_id" },
    additionalProperties = false,
    properties = {
      agent_id = { type = "string", description = "Live agent/session id.", required = true },
    },
  },
  header = function(input)
    return "status · " .. tostring(input.agent_id or "?")
  end,
  handler = function(input)
    if not input.agent_id or input.agent_id == "" then
      return { llm_output = "agent_id is required", is_error = true }
    end
    local current, _ = n00n.session.current()
    if input.agent_id == tostring(current or "") then
      return { llm_output = "cannot query the focused session", is_error = true }
    end
    local agent, err = n00n.session.status(input.agent_id)
    if not agent then
      return { llm_output = err, is_error = true }
    end
    local encoded, fmt = encode_structured(agent)
    if not encoded then
      return { llm_output = "encode failed", is_error = true }
    end
    local lines = {
      agent_line(agent),
      "focused: " .. tostring(agent.focused == true),
    }
    if agent.output and agent.output ~= "" then
      lines[#lines + 1] = "output: " .. tostring(agent.output):sub(1, 120)
    end
    if agent.paused_team and agent.paused_team.run_id then
      lines[#lines + 1] = "paused_team: " .. tostring(agent.paused_team.run_id)
    end
    local body, annotation = card("status · " .. input.agent_id, lines, tostring(agent.status or "?"))
    return {
      llm_output = encoded,
      format = "plain",
      body = body,
      annotation = annotation .. " (" .. fmt .. ")",
    }
  end,
})

---------------------------------------------------------------------------
-- agent_control (mutating; deferred)
---------------------------------------------------------------------------

local function build_resume_prompt(run_info, guidance)
  return helpers.build_resume_prompt(run_info, guidance, function(value)
    return n00n.json.encode(value)
  end)
end

local control_schema = {
  type = "object",
  required = { "action" },
  additionalProperties = false,
  properties = {
    action = {
      type = "string",
      enum = { "message", "pause", "resume", "stop", "policy" },
      description = "Mutating control action.",
      required = true,
    },
    agent_id = { type = "string", description = "Target agent id." },
    message = { type = "string", description = "Steering text for message/resume." },
    policy = {
      type = "object",
      description = "Policy payload when action=policy.",
      properties = {
        action = {
          type = "string",
          enum = { "set", "get", "delete", "list" },
        },
        rule = {
          type = "object",
          description = "Rule for set.",
          properties = {
            id = { type = "string", description = "Unique rule id." },
            scope = {
              type = "object",
              description = "Scope filters.",
              properties = {
                tag = { type = "string", description = "Filter by tag." },
                session_type = { type = "string", description = "Filter by session type." },
                agent_id = { type = "string", description = "Filter by agent id." },
              },
            },
            restricted_tools = {
              type = "array",
              items = { type = "string" },
              description = "Denied tools.",
            },
            allowed_tools = {
              type = "array",
              items = { type = "string" },
              description = "Allowed tools (whitelist).",
            },
            paused = { type = "boolean", description = "Pause agents in scope." },
            priority = { type = "integer", description = "Priority (higher wins)." },
          },
          required = { "id", "scope", "priority" },
        },
        rule_id = { type = "string" },
      },
    },
  },
}

local function control_handler(input)
  if input.action == "policy" then
    if not input.policy or not input.policy.action then
      return { llm_output = "policy.action is required", is_error = true }
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
      local encoded, fmt = encode_structured(rule)
      if not encoded then
        return { llm_output = "Error: encode failed", is_error = true }
      end
      local body = card("policy set · " .. tostring(rule.id), { "priority " .. tostring(rule.priority) }, "set")
      return { llm_output = encoded, body = body, annotation = "set (" .. fmt .. ")" }
    elseif paction == "get" then
      if not input.policy.rule_id then
        return { llm_output = "policy.rule_id is required for get", is_error = true }
      end
      local rule, err = policy_get(input.policy.rule_id)
      if not rule then
        return { llm_output = "Error: " .. tostring(err), is_error = true }
      end
      local encoded, fmt = encode_structured(rule)
      if not encoded then
        return { llm_output = "Error: encode failed", is_error = true }
      end
      local body = card("policy · " .. input.policy.rule_id, { "loaded" }, "get")
      return { llm_output = encoded, body = body, annotation = "get (" .. fmt .. ")" }
    elseif paction == "delete" then
      if not input.policy.rule_id then
        return { llm_output = "policy.rule_id is required for delete", is_error = true }
      end
      local dok, derr = policy_delete(input.policy.rule_id)
      if not dok then
        return { llm_output = "Error: " .. tostring(derr), is_error = true }
      end
      local msg = "Policy deleted: " .. input.policy.rule_id
      local body = card(msg, {}, "deleted")
      return { llm_output = msg, body = body, annotation = "deleted" }
    elseif paction == "list" then
      local rules = load_policies().rules
      local encoded, fmt = encode_structured(rules)
      if not encoded then
        return { llm_output = "Error: encode failed", is_error = true }
      end
      local lines = {}
      for _, rule in ipairs(rules) do
        lines[#lines + 1] = tostring(rule.id) .. " · priority " .. tostring(rule.priority)
      end
      if #lines == 0 then
        lines[1] = "(no policies)"
      end
      local body = card(string.format("policies · %d", #rules), lines, tostring(#rules))
      return { llm_output = encoded, body = body, annotation = tostring(#rules) .. " (" .. fmt .. ")" }
    end
    return { llm_output = "Error: unknown policy action " .. tostring(paction), is_error = true }
  end

  if not input.agent_id or input.agent_id == "" then
    return { llm_output = "agent_id is required for " .. tostring(input.action), is_error = true }
  end

  if input.action == "message" then
    if not input.message or input.message == "" then
      return { llm_output = "message is required for message", is_error = true }
    end
    if policy_ok and policy then
      local status = n00n.session.status(input.agent_id)
      local session_type = status and status.session_type or nil
      local tags = status and status.tags or nil
      local policy_result = policy.evaluate_policy(input.agent_id, session_type, tags, "session.prompt")
      if not policy_result.allowed then
        return { llm_output = "Policy blocked: " .. (policy_result.reason or "unknown"), is_error = true }
      end
    end
    local state, err = n00n.session.prompt(input.message, {
      session = input.agent_id,
      steer = true,
      control = true,
    })
    if not state then
      return { llm_output = err, is_error = true }
    end
    local plain = string.format("message · %s", input.agent_id)
    local body = card(plain, { tostring(state) }, "message")
    return { llm_output = plain, body = body, annotation = "message" }
  end

  if input.action == "resume" then
    local status, status_err = n00n.session.status(input.agent_id)
    if not status then
      return { llm_output = status_err or "session status unavailable", is_error = true }
    end
    if policy_ok and policy then
      for _, tool_name in ipairs({ "session.prompt", "team" }) do
        local policy_result = policy.evaluate_policy(input.agent_id, status.session_type, status.tags, tool_name)
        if not policy_result.allowed then
          return { llm_output = "Policy blocked: " .. (policy_result.reason or "unknown"), is_error = true }
        end
      end
    end
    local run_info = status.paused_team
    if not run_info then
      return { llm_output = "no paused team run found for agent " .. input.agent_id, is_error = true }
    end
    local prompt, prompt_err = build_resume_prompt(run_info, input.message)
    if not prompt then
      return { llm_output = prompt_err, is_error = true }
    end
    local state, err = n00n.session.prompt(prompt, {
      session = input.agent_id,
      steer = true,
      control = true,
    })
    if not state then
      return { llm_output = err, is_error = true }
    end
    local plain = string.format("resume · %s · %s", input.agent_id, tostring(run_info.run_id))
    local body = card(plain, { tostring(state) }, "resume")
    return { llm_output = plain, body = body, annotation = "resume" }
  end

  if input.action == "pause" then
    -- TUI has no pause; cancel is stop. Return a clear error (matches daemon verb table).
    return {
      llm_output = "unsupported verb `pause` on backend `tui` (use stop, or a worker agent)",
      is_error = true,
    }
  end

  -- stop
  local stopped, err = n00n.session.cancel(input.agent_id)
  if not stopped then
    return { llm_output = err, is_error = true }
  end
  local plain = "stopped · " .. input.agent_id
  local body = card(plain, {}, "stopped")
  return { llm_output = plain, body = body, annotation = "stopped" }
end

n00n.api.register_tool({
  name = "agent_control",
  description = "Mutate a background agent: message, stop, resume, or manage policy. Prefer agent_list/agent_status for reads. Pause is unsupported on TUI sessions.",
  kind = "execute",
  admission = "cheap",
  audiences = { "main" },
  defer_loading = true,
  schema = control_schema,
  header = function(input)
    return "control · " .. tostring(input.action or "?")
  end,
  handler = control_handler,
})
