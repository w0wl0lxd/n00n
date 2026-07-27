# Agent Orchestration Review

## Worktree
- Path: `/home/w0w/dev/.n00n-worktrees/agent-cli-simplify`
- Branch: `feat/agent-cli-simplify`

## Abstractions and Responsibilities

### Rust Core (n00n-agent)

#### `headless.rs`
- **Responsibility**: Headless (non-interactive) and interactive agent spawning
- **Key types**:
  - `HeadlessParams` / `HeadlessHandle`: One-shot agent runs
  - `InteractiveParams` / `InteractiveHandle`: Long-running interactive sessions
  - `SessionStore`: Session persistence
- **Notes**: Single entry point for both headless and interactive modes. Handles session persistence, tool setup, and agent lifecycle.

#### `agent/run.rs`
- **Responsibility**: Core agent loop (turn management, tool dispatch, streaming)
- **Key types**:
  - `AgentParams`: Static agent configuration (provider, model, permissions, etc.)
  - `AgentRunParams`: Per-run parameters (history, system, tools, event channel)
  - `Agent`: The agent instance with the run loop
- **Notes**: Handles streaming, tool calls, compaction, retries, and cancellation. No orchestration logic beyond single-agent loops.

### Lua API (n00n-lua)

#### `api/agent.rs`
- **Responsibility**: Subagent primitives for Lua plugins
- **Key functions**:
  - `resolve_model()`: Model selection with tier clamping
  - `system_prompt()`: Build system prompts from templates
  - `tools()`: Get tool definitions for a given audience
  - `call_tool()`: Direct tool invocation from Lua
  - `session()`: Create subagent sessions
- **Notes**: Pure primitives. No policy (retries, validation, concurrency) - that lives in plugins.

#### `api/session.rs`
- **Responsibility**: Host session management (list, live, focus, delete, prompt)
- **Key functions**:
  - `list()`, `live()`, `status()`, `current()`, `focus()`, `delete()`, `new()`, `prompt()`, `cancel()`, `set_title()`
- **Notes**: Round-trips to UI event loop. No agent orchestration here.

### Lua Plugins

#### `task/init.lua`
- **Responsibility**: Single isolated subagent with structured output
- **Key features**:
  - Structured output via local `structured_output` tool
  - Schema validation with `n00n.json.schema_validator`
  - Concurrency control via semaphore
  - Progress preview UI
  - Background mode (spawns new session via `n00n.session.new`)
- **Subagent launch path**: `n00n.agent.session()` → `sess:prompt()` → close

#### `workflow/init.lua`
- **Responsibility**: Multi-stage agent orchestration via sandboxed Lua scripts
- **Key features**:
  - Script runtime with globals: `agent()`, `parallel()`, `pipeline()`, `phase()`, `log()`, `inputs`
  - Journaling for resume/replay
  - Structured output via local `structured_output` tool
  - Concurrency limits (per-workflow, aggregate)
  - Progress preview UI
- **Subagent launch path**: `n00n.agent.session()` → `sess:prompt()` → close

#### `team/init.lua`
- **Responsibility**: ALMAS multi-agent SDLC orchestration
- **Key features**:
  - Supervisor planning (structured output)
  - Role-based execution (product_manager, planner, developer, tester, reviewer)
  - Swarm mode (decentralized rounds)
  - Waves mode (plan → implement → validate with gates)
  - Retrieval augmentation
  - Quorum validation
  - Human escalation / resume
  - Background mode
- **Subagent launch path**: Delegates to `roles.run()` → `n00n.agent.session()` → `sess:prompt()` → close

#### `team/roles.lua`
- **Responsibility**: Role catalogue and execution
- **Key features**:
  - Role definitions with tier and system prompts
  - `run()` function that creates subagent sessions
  - Usage/cost tracking
- **Subagent launch path**: `n00n.agent.session()` → `sess:prompt()` → close

#### `agent_control/init.lua`
- **Responsibility**: Control background agents (list, status, message, pause, resume, stop, policy)
- **Key features**:
  - Policy management (set/get/delete/list rules)
  - Session control via `n00n.session.*` API
  - Policy evaluation before actions
- **Notes**: Overlaps with CLI surface - no dedicated CLI commands for agent control.

#### `blackboard/init.lua`
- **Responsibility**: Shared coordination substrate for multi-agent sessions
- **Key features**:
  - Post observations, claim tasks atomically, query state
  - Project-scoped persistence
- **Notes**: Coordination primitive, not orchestration.

#### `batch/init.lua`
- **Responsibility**: Concurrent tool dispatch
- **Key features**:
  - Parallel tool execution with UI composition
  - State persistence for restore
- **Notes**: Tool-level concurrency, not agent orchestration.

### CLI (src/cli.rs, src/cmd/)

#### `cli.rs`
- **Responsibility**: CLI argument parsing
- **Current commands**:
  - `auth` (login/logout/status)
  - `models`
  - `index`
  - `mcp` (auth/logout)
  - `update` / `rollback`
  - `acp` (Agent Client Protocol server)
  - `prompt` (show system prompt or tools)
- **Notes**: No `agent` subcommand tree. No agent control commands.

#### `cmd/mod.rs`
- **Responsibility**: Command dispatch
- **Notes**: Delegates to subcmd implementations.

#### `cmd/subcmd.rs`
- **Responsibility**: Subcommand implementations
- **Notes**: Auth, models, index, mcp, prompt implementations.

## Duplication and Overcomplication

### 1. Structured Output / Schema Validation (MAJOR DUPLICATION)

**Duplicated constants (task vs workflow):**
- `STRUCTURED_OUTPUT_NAME` = "structured_output"
- `STRUCTURED_OUTPUT_DESCRIPTION`
- `STRUCTURED_OUTPUT_ACK`
- `STRUCTURED_OUTPUT_PROMPT_SUFFIX` / `STRUCTURED_OUTPUT_SUFFIX`
- `MAX_STRUCTURED_RETRIES` = 1
- `MAX_SCHEMA_ERRORS` = 3
- `MAX_SCHEMA_BYTES` = 32 * 1024
- `MAX_SCHEMA_DEPTH` = 16
- `SCHEMA_ROOT_ERROR`
- `SCHEMA_COMPILE_ERROR`
- `SCHEMA_SIZE_ERROR`
- `SCHEMA_DEPTH_ERROR`
- `STRUCTURED_MISSING_ERROR`
- `STRUCTURED_INVALID_ERROR`
- `NUDGE_MISSING`
- `INVALID_INPUT_PREFIX`

**Duplicated functions (task vs workflow):**
- `schema_within_depth(value, depth)` - identical implementation
- `bounded_errors(errors)` - identical implementation

**Duplicated logic (task vs workflow):**
- Schema validation flow (compile → size check → depth check → validator creation)
- Local tool registration for `structured_output`
- Retry loop with nudge for missing structured_output call
- Error message formatting

**Impact**: ~80 lines of duplicated code across two files. Any fix or enhancement must be made in both places.

### 2. Subagent Launch Path (MODERATE DUPLICATION)

**task/init.lua:**
```lua
local sess, sess_err = n00n.agent.session(ctx, {
  model_spec = model.spec,
  system = system,
  tools = tool_defs,
  local_tools = local_tools,
  audience = audience,
  name = input.description,
})
local result, err = sess:prompt(message)
sess:close()
```

**workflow/init.lua (agent() function):**
```lua
local sess, sess_err = n00n.agent.session(ctx, {
  model_spec = model.spec,
  system = system,
  tools = tool_defs,
  local_tools = local_tools,
  audience = audience,
  name = label,
})
local prompt_result, prompt_err = sess:prompt(message)
sess:close()
```

**team/roles.lua (run() function):**
```lua
local sess, serr = n00n.agent.session(ctx, {
  model_spec = model.spec,
  system = r.system,
  tools = tools,
  audience = "general_sub",
  name = role,
  thinking = opts.thinking,
})
local res, rerr = sess:prompt(prompt)
sess:close()
```

**Impact**: Three near-identical subagent launch patterns. Differences:
- `task` uses `input.description` for name
- `workflow` uses `label` for name
- `roles` uses `role` for name and adds `thinking` parameter
- `roles` hardcodes audience to "general_sub"

### 3. Model Resolution and Tool Setup (MODERATE DUPLICATION)

**Pattern repeated in task, workflow, and roles:**
```lua
local model, model_err = n00n.agent.resolve_model(ctx, { spec = ..., tier = ... })
local audience = subagent_type == "research" and "research_sub" or "general_sub"
local prompt_id = subagent_type == "research" and "research" or "general"
local system, system_err = n00n.agent.system_prompt(ctx, { prompt_id = prompt_id, instructions = true })
local tool_defs, tools_err = n00n.agent.tools(ctx, { audience = audience, spec = model.spec })
```

**Impact**: Same 4-5 line sequence in three places. Only variation is audience selection logic.

### 4. Cost/Usage Tracking (MINOR DUPLICATION)

**task/init.lua:**
```lua
local function attach_cost(r)
  if r and not r.cost and r.input_tokens and r.output_tokens then
    local cost, _ = n00n.agent.usage_cost(model.spec, r.input_tokens, r.output_tokens, r)
    r.cost = cost
  end
end
```

**team/roles.lua:**
```lua
M.metrics = usage.price
-- Later:
local measured_usage, cost, metrics_err = M.metrics(model.spec, res)
```

**Impact**: Different approaches to the same problem. Task attaches cost inline; roles uses a separate module.

### 5. Agent Control vs CLI Surface (MISSING INTEGRATION)

**agent_control tool** provides:
- `list`, `status`, `message`, `pause`, `resume`, `stop`, `policy` actions

**CLI** has:
- No `n00n agent` subcommand tree
- No way to control background agents from CLI
- Users must invoke agent_control through the LLM interface

**Impact**: Agent control is only accessible via tool calls, not as a first-class CLI command.

### 6. Team as Reimplementation of Workflow (ARCHITECTURAL OVERLAP)

**team/init.lua** implements:
- Sequential step execution (like workflow scripts)
- Parallel execution (swarm mode, like workflow `parallel()`)
- State persistence (memory plugin, like workflow journaling)
- Progress tracking (ActivityPreview, like workflow progress)

**workflow/init.lua** provides:
- Script-based orchestration
- Parallel/pipeline primitives
- Journaling for resume

**Impact**: Team could be expressed as a workflow template/script with additional plugins (blackboard, retrieval, quorum). Currently reimplements orchestration primitives.

## Simplification Opportunities

### High Priority

1. **Extract shared structured-output helper module**
   - Create `plugins/structured_output.lua` with:
     - Constants (STRUCTURED_OUTPUT_*, MAX_*, SCHEMA_*, NUDGE_*, INVALID_INPUT_PREFIX)
     - `schema_within_depth()`
     - `bounded_errors()`
     - `make_structured_tool(schema)` - returns local tool spec
     - `run_with_structured_output(sess, message, validator, max_retries)` - handles prompt + retry loop
   - Replace duplicated code in task and workflow

2. **Unify subagent launch path**
   - Create shared helper in `n00n-lua` or new plugin:
     - `launch_subagent(ctx, opts)` where opts includes:
       - `model_spec`, `system`, `tools`, `local_tools`, `audience`, `name`, `thinking`
       - Returns `{ result, err, cost, usage }`
   - Replace three near-identical launch sequences

3. **Add CLI agent control commands**
   - Add `n00n agent` subcommand tree:
     - `n00n agent list` - list background agents
     - `n00n agent status <id>` - show agent status
     - `n00n agent message <id> <text>` - send steering message
     - `n00n agent pause <id>` - pause agent
     - `n00n agent resume <id>` - resume agent
     - `n00n agent stop <id>` - stop agent
     - `n00n agent policy ...` - policy management
   - Map to existing agent_control tool actions

### Medium Priority

4. **Extract model/tool setup helper**
   - Create `setup_subagent_env(ctx, subagent_type, model_spec, model_tier)` helper
   - Returns `{ model, audience, system, tools }`
   - Replace repeated 4-5 line sequences

5. **Unify cost/usage tracking**
   - Standardize on one approach (likely the roles/usage module pattern)
   - Apply consistently across task, workflow, team

6. **Consider team as workflow template**
   - Express team as a workflow script + specialized plugins
   - Reduces team to composition rather than reimplementation
   - May require workflow enhancements (state persistence beyond journaling)

### Low Priority

7. **CLI session management**
   - Add `n00n session` subcommand for direct session control
   - Maps to `n00n.session.*` Lua API
   - Useful for automation without LLM

## Proposed Design (Phase 2)

See final message for design proposal.
