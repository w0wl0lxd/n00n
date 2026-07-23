# Blackboard Tool Contract

**Feature**: specs/002-agent-coordination-visibility  
**Date**: 2026-07-23  
**Status**: Phase 1 complete

## Overview

The blackboard tool provides a shared coordination substrate for multi-agent sessions. Agents post observations, claim tasks atomically, and query the blackboard for coordination state. The tool is implemented as a Lua plugin (`plugins/blackboard/init.lua`) and registered via `n00n.api.register_tool`.

## Tool Schema

```lua
{
  name = "blackboard",
  description = "Shared coordination substrate for multi-agent sessions. Post observations, claim tasks atomically, and query coordination state.",
  kind = "execute",
  audiences = { "main", "general_sub" },
  schema = {
    type = "object",
    required = { "action" },
    properties = {
      action = {
        type = "string",
        enum = { "write", "read", "claim_task", "release_task", "update_task", "query" },
        description = "Blackboard action.",
      },
      post = {
        type = "object",
        description = "Post data for write action.",
        properties = {
          id = { type = "string", description = "Unique post identifier (optional, auto-generated if omitted)." },
          type = { type = "string", enum = { "observation", "claim", "status", "escalation" }, description = "Post type." },
          content = { type = "string", description = "Post content." },
          tags = { type = "array", items = { type = "string" }, description = "Tags for filtering." },
          task_id = { type = "string", description = "Associated task ID." },
        },
        required = { "type", "content" },
      },
      task_id = {
        type = "string",
        description = "Task ID for claim/release/update actions.",
      },
      claim = {
        type = "object",
        description = "Claim data for claim_task action.",
        properties = {
          task_id = { type = "string", description = "Task ID to claim." },
          expires_in = { type = "integer", description = "Claim TTL in seconds (default 300)." },
        },
        required = { "task_id" },
      },
      query = {
        type = "object",
        description = "Query parameters for query action.",
        properties = {
          type = { type = "string", enum = { "observation", "claim", "status", "escalation" }, description = "Filter by post type." },
          task_id = { type = "string", description = "Filter by task ID." },
          tags = { type = "array", items = { type = "string" }, description = "Filter by tags (any match)." },
          agent_id = { type = "string", description = "Filter by agent ID." },
          limit = { type = "integer", description = "Maximum results (default 100)." },
        },
      },
    },
  },
}
```

## Actions

### write

Post a new entry to the blackboard.

**Input**:
- `action`: "write"
- `post` (object):
  - `id` (string, optional): Unique post identifier. Auto-generated if omitted.
  - `type` (string, required): Post type - "observation", "claim", "status", or "escalation".
  - `content` (string, required): Post content.
  - `tags` (array of string, optional): Tags for filtering.
  - `task_id` (string, optional): Associated task ID.

**Output**:
- Success: `{ llm_output = "Post written: {id}", post_id = "{id}" }`
- Error: `{ llm_output = "Error: {message}", is_error = true }`

**Validation**:
- `type` must be one of the allowed values.
- `content` must be non-empty.
- If `task_id` is present, it must be non-empty.
- If `id` is omitted, a UUID is generated.

**Atomicity**: Write is append-only to the JSONL file. File lock ensures atomic append.

### read

Read a specific post by ID.

**Input**:
- `action`: "read"
- `post_id` (string, required): Post ID to read.

**Output**:
- Success: `{ llm_output = "{encoded_post}", post = {post_object} }`
- Error: `{ llm_output = "Error: {message}", is_error = true }`

**Validation**:
- `post_id` must be non-empty and must exist in the blackboard.

### claim_task

Atomically claim a task.

**Input**:
- `action`: "claim_task"
- `claim` (object):
  - `task_id` (string, required): Task ID to claim.
  - `expires_in` (integer, optional): Claim TTL in seconds. Default 300.

**Output**:
- Success: `{ llm_output = "Task claimed: {task_id}", claim = {claim_object} }`
- Conflict (already claimed): `{ llm_output = "Task already claimed by {agent_id}", is_error = true }`
- Error: `{ llm_output = "Error: {message}", is_error = true }`

**Validation**:
- `task_id` must be non-empty.
- `expires_in` must be positive if present.

**Atomicity**: Claim uses file lock to check existing claims and write the new claim atomically. If another agent holds the claim, the operation fails with a conflict error.

### release_task

Release a held task claim.

**Input**:
- `action`: "release_task"
- `task_id` (string, required): Task ID to release.

**Output**:
- Success: `{ llm_output = "Task released: {task_id}" }`
- Error: `{ llm_output = "Error: {message}", is_error = true }`

**Validation**:
- `task_id` must be non-empty.
- The calling agent must hold the claim.

**Atomicity**: Release uses file lock to update the claim status atomically.

### update_task

Update a task claim status (e.g., mark as done or failed).

**Input**:
- `action`: "update_task"
- `task_id` (string, required): Task ID to update.
- `status` (string, required): New status - "done" or "failed".

**Output**:
- Success: `{ llm_output = "Task updated: {task_id} -> {status}" }`
- Error: `{ llm_output = "Error: {message}", is_error = true }`

**Validation**:
- `task_id` must be non-empty.
- `status` must be "done" or "failed".
- The calling agent must hold the claim.

**Atomicity**: Update uses file lock to change the claim status atomically.

### query

Query the blackboard for posts matching filters.

**Input**:
- `action`: "query"
- `query` (object, optional):
  - `type` (string, optional): Filter by post type.
  - `task_id` (string, optional): Filter by task ID.
  - `tags` (array of string, optional): Filter by tags (any match).
  - `agent_id` (string, optional): Filter by agent ID.
  - `limit` (integer, optional): Maximum results. Default 100.

**Output**:
- Success: `{ llm_output = "{encoded_results}", results = [{post_objects}] }`
- Error: `{ llm_output = "Error: {message}", is_error = true }`

**Validation**:
- If `type` is present, it must be one of the allowed values.
- If `limit` is present, it must be positive.
- Results are returned in chronological order (newest first).

**Performance**: Query scans the JSONL file and applies filters. For large blackboards, a cached index may be added in a future iteration.

## Error Handling

All actions return errors in the format `{ llm_output = "Error: {message}", is_error = true }`. Common errors:

- "blackboard unavailable": Storage directory cannot be accessed.
- "invalid post type": Type is not one of the allowed values.
- "task already claimed": Another agent holds the claim.
- "claim not found": Task ID does not exist or is not claimed by the calling agent.
- "invalid query parameters": Query filters are malformed.

## Storage

Posts are stored in `state_dir/projects/{pid}/blackboard/posts.jsonl` as append-only JSONL lines. Each line is a JSON object representing a post. A lockfile (`posts.lock`) ensures atomic operations.

## Integration

The blackboard tool is used by:
- `team` plugin: Post step status, claim tasks, query for coordination.
- `workflow` plugin: Post progress, claim resources.
- Custom agents: Post observations, query for cross-session state.

The tool is registered with audiences `main` and `general_sub` so both the main agent and subagents can access it.
