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

**Batch semantics**:

- If a tool batch contains one or more `skill` calls, dispatch runs those `skill` calls first (sequentially, in provider order), updates policy after each successful `skill` result, then dispatches all other tools.
- Non-`skill` calls in that same batch are therefore evaluated against the final active policy produced by the `skill` phase, regardless of original call ordering in the provider message.

**Scope**: Applies in the same batch after `skill` phase completion and to all later turns.
