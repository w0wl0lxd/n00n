# Agent Control Policy Contract

**Feature**: specs/002-agent-coordination-visibility  
**Date**: 2026-07-23  
**Status**: Phase 1 complete

## Overview

The agent control policy layer enforces restrictions on agent behavior based on policy rules. Policies are defined per project and evaluated before tool execution. The policy layer extends the existing `agent_control` plugin with a new `policy` action and a policy table for configuration.

## Tool Schema Extension

The existing `agent_control` tool schema is extended with a new `policy` action:

```lua
{
  name = "agent_control",
  description = "Control background agents started by task, team, or workflow. Actions: list, status, message, pause, resume, stop, policy.",
  kind = "execute",
  audiences = { "main" },
  schema = {
    type = "object",
    required = { "action" },
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
  },
}
```

## Policy Table Shape

Policies are stored in `state_dir/projects/{pid}/policies/policy.json` with the following structure:

```json
{
  "version": 1,
  "rules": [
    {
      "id": "policy-001",
      "scope": {
        "tag": "background"
      },
      "restricted_tools": ["write", "edit", "bash"],
      "paused": false,
      "priority": 10
    },
    {
      "id": "policy-002",
      "scope": {
        "session_type": "team"
      },
      "allowed_tools": ["read", "blackboard", "agent_control"],
      "paused": false,
      "priority": 5
    }
  ]
}
```

## Actions

### policy set

Set or update a policy rule.

**Input**:
- `action`: "policy"
- `policy` (object):
  - `action`: "set"
  - `rule` (object, required):
    - `id` (string, required): Unique policy identifier.
    - `scope` (object, required): Policy scope (tag, session_type, or agent_id).
    - `restricted_tools` (array of string, optional): Tools to restrict.
    - `allowed_tools` (array of string, optional): Tools to allow (whitelist).
    - `paused` (boolean, optional): Whether to pause agents. Default false.
    - `priority` (integer, required): Policy priority.

**Output**:
- Success: `{ llm_output = "Policy set: {id}", policy = {rule_object} }`
- Error: `{ llm_output = "Error: {message}", is_error = true }`

**Validation**:
- `id` must be non-empty.
- `scope` must have exactly one of `tag`, `session_type`, or `agent_id`.
- `restricted_tools` and `allowed_tools` cannot both be non-empty.
- `priority` must be a non-negative integer.

**Conflict resolution**: If multiple policies match an agent, the highest priority wins. If priorities are equal, the most restrictive policy wins (union of restricted_tools, intersection of allowed_tools).

### policy get

Get a specific policy rule.

**Input**:
- `action`: "policy"
- `policy` (object):
  - `action`: "get"
  - `rule_id` (string, required): Policy rule ID.

**Output**:
- Success: `{ llm_output = "{encoded_rule}", policy = {rule_object} }`
- Error: `{ llm_output = "Error: {message}", is_error = true }`

**Validation**:
- `rule_id` must be non-empty and must exist in the policy table.

### policy delete

Delete a policy rule.

**Input**:
- `action`: "policy"
- `policy` (object):
  - `action`: "delete"
  - `rule_id` (string, required): Policy rule ID.

**Output**:
- Success: `{ llm_output = "Policy deleted: {rule_id}" }`
- Error: `{ llm_output = "Error: {message}", is_error = true }`

**Validation**:
- `rule_id` must be non-empty and must exist in the policy table.

### policy list

List all policy rules.

**Input**:
- `action`: "policy"
- `policy` (object):
  - `action`: "list"

**Output**:
- Success: `{ llm_output = "{encoded_rules}", policies = [{rule_objects}] }`
- Error: `{ llm_output = "Error: {message}", is_error = true }`

## Policy Evaluation

When an agent attempts to call a tool, the policy layer evaluates applicable policies:

1. Collect all policies whose scope matches the agent:
   - `tag` match: Agent has the tag (set via session metadata).
   - `session_type` match: Agent session type matches.
   - `agent_id` match: Agent session ID matches exactly.
2. Sort by priority (descending).
3. Resolve conflicts:
   - If only one policy matches, apply it.
   - If multiple policies match, pick the highest priority.
   - If priorities are equal, merge:
     - `restricted_tools`: Union of all restricted tool sets.
     - `allowed_tools`: Intersection of all allowed tool sets.
     - `paused`: True if any policy sets paused = true.
4. Apply the resolved policy:
   - If `paused` is true, reject the tool call with error "agent is paused".
   - If `restricted_tools` is set and the tool is in the list, reject with error "tool restricted by policy".
   - If `allowed_tools` is set and the tool is not in the list, reject with error "tool not allowed by policy".
   - Otherwise, allow the tool call.

## Policy Enforcement Hook

The policy evaluation is hooked into the tool dispatch layer. In the initial implementation, this is done in Lua at the `agent_control` plugin level by intercepting tool calls before forwarding them. Later, a Rust hook in `n00n-agent`'s `tool_dispatch` can add a second enforcement layer for safety-critical restrictions.

### Lua Enforcement (Initial)

The `agent_control` plugin provides a helper function `check_policy(agent_id, tool_name)` that evaluates policies and returns an error if the tool call is restricted. Plugins call this before forwarding tool calls.

### Rust Enforcement (Future)

A Rust hook in `n00n-agent`'s `tool_dispatch` checks policies before executing any tool. This provides a safety-critical enforcement layer that cannot be bypassed by Lua code.

## Error Handling

Policy violations return errors in the format `{ llm_output = "Error: {message}", is_error = true }`. Common errors:

- "agent is paused": Agent is paused by policy.
- "tool restricted by policy": Tool is in the restricted_tools list.
- "tool not allowed by policy": Tool is not in the allowed_tools list (whitelist mode).
- "policy not found": Policy rule ID does not exist.
- "invalid policy scope": Scope must have exactly one of tag, session_type, or agent_id.
- "conflicting tool lists": restricted_tools and allowed_tools cannot both be non-empty.

## Storage

Policies are stored in `state_dir/projects/{pid}/policies/policy.json`. The file is read on policy evaluation and written on policy set/delete. A lockfile (`policy.lock`) ensures atomic updates.

## Integration

The policy layer is used by:
- `agent_control` plugin: Policy management (set, get, delete, list).
- `team` plugin: Check policy before forwarding tool calls to role agents.
- `workflow` plugin: Check policy before executing agent() calls.
- Custom agents: Check policy before tool execution.

The policy layer is registered with audience `main` so only the main agent can manage policies. Subagents are subject to policies but cannot modify them.
