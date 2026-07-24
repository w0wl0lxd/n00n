# Research: Agent Coordination Execution

**Date**: 2026-07-23

## Sources

- `codegraph` exploration of `plugins/team/init.lua`, `plugins/agent_control/init.lua`, `plugins/blackboard/init.lua`, `n00n-lua/src/api/agent.rs`, `n00n-lua/src/api/session.rs`.
- `exa` search for "ALMAS multi-agent software engineering SPOQ wave dispatch planning implementation validation".
- `web_search` for "ALMAS Tawosi ASE 2025 SPOQ wave multi-agent".
- `thoughtbox` decision frame synthesizing findings.

## Key Findings

### SPOQ Wave Dispatch

SPOQ (Specialist Orchestrated Queuing) models task dependencies as a DAG and computes execution waves via topological sort. Tasks within a wave execute in parallel; waves execute sequentially. It applies dual validation gates: one before execution (plan quality) and one after execution (code quality), each scored 0-100 with a 95% aggregate threshold. For n00n, we can implement a simplified sequential-wave dispatch (no parallel subagent spawning in this iteration) with role-based waves and a single LLM validation gate between waves.

### ALMAS Roles

ALMAS aligns agents with agile roles: product_manager, sprint, planner, developer, tester, reviewer. Acceptance criteria are used for test generation and review. The previous `team` prompt already assigns these roles, so wave grouping can use them directly.

### TEA Checkpoints

Test Execution Agent / checkpointer patterns persist state after each step, allow resume with human input, and load previous checkpoints. `plugins/lib/n00n/checkpoint.lua` already provides `save`, `load`, `list`, `latest`. The follow-up wires this into `team` after each wave and on `resume`.

### n00n API Observations

- `n00n.agent.call_tool(ctx, name, input)` is async and requires a `ctx` (LuaCtx).
- `n00n.session.live()` returns live sessions; `n00n.session.cancel(id)` and `n00n.session.prompt(message, {session = id})` support pause/resume.
- `n00n.json.schema_validator` can validate structured planner output.
- Policy enforcement is best implemented at the Lua call site by wrapping `n00n.agent.call_tool` because the tool dispatch core is not being modified.

## Decisions

1. Implement sequential wave dispatch (plan → implement → validate) rather than parallel subagent dispatch to stay within the existing `team` session model.
2. Use a simple PASS/FAIL validation gate with textual explanation; scoring can be added later.
3. Enforce policies by intercepting `n00n.agent.call_tool` inside Lua plugins that need enforcement.
4. Reuse `plugins/lib/n00n/checkpoint.lua` and `blackboard` without adding new Rust code.
