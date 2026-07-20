local description = [[Control background agents started by task or team.

Actions:
- list: list every live agent and its status.
- status: inspect one live agent by agent_id.
- message: queue steering instructions for an agent.
- stop: cancel an agent's current run without deleting its session.]]

local schema = {
  type = "object",
  required = { "action" },
  additionalProperties = false,
  properties = {
    action = {
      type = "string",
      enum = { "list", "status", "message", "stop" },
      description = "Control action.",
    },
    agent_id = {
      type = "string",
      description = "Background agent id. Required for status, message, and stop.",
    },
    message = {
      type = "string",
      description = "Steering instructions. Required for message.",
    },
  },
}

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

  if input.action == "message" then
    if not input.message or input.message == "" then
      return { llm_output = "message is required for message", is_error = true }
    end
    local state, err = n00n.session.prompt(input.message, { session = input.agent_id })
    if not state then
      return { llm_output = err, is_error = true }
    end
    return n00n.json.encode({ agent_id = input.agent_id, state = state })
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
