# Contract: Skill Policy Enforcement

## Agent dispatch gate

**Location**: `n00n-agent/src/agent/tool_dispatch.rs` — before `invocation.execute`

**Input**: `tool_name: &str`, `ctx.active_skill_policy: Option<ActiveSkillPolicy>`

**Behavior**:
- If no active policy → allow (delegate to existing checks).
- If `disallowed_tools` contains normalized tool name → reject with `tool {name} is disallowed by active skill policy`.
- If `allowed_tools` non-empty and tool not in list → reject with `tool {name} is not allowed by active skill policy`.
- `skill` tool itself is always allowed (agent must load/clear skills).

**Tool name normalization**: lowercase; `-` → `_`.

## Policy lifecycle

**Set**: `ToolDoneEvent` where `tool == "skill"`, `!is_error`, `state.active_skill` is object.

**Clear**: `ToolDoneEvent` where `tool == "skill"`, `!is_error`, `state.active_skill` absent or null.

**Scope**: Applies from the turn after the skill tool completes (subsequent `tool_dispatch::run` calls in later turns and later parallel batch items if policy was set in a prior turn).
