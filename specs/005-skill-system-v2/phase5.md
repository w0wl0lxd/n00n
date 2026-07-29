# Phase 5: Skill Policy Enforcement and Context Routing

**Status**: In progress  
**Depends on**: Phase 4 (progressive loading, tool policy frontmatter)

## User Stories

### US1 - Skill policy enforcement envelope (P1)

As an agent running under an active skill, I want tool calls checked against the skill's `allowed-tools` / `disallowed-tools` policy so scoped workflows cannot silently bypass restrictions.

**Acceptance**:
1. Loading a skill with tool policy returns `state.active_skill` containing the policy envelope.
2. `skill_policy.evaluate` rejects disallowed tools and non-allowlisted tools.
3. `skill_policy.call_tool` blocks before delegating to `n00n.agent.call_tool`.
4. Load output includes an `instructions` block documenting the active policy.

### US2 - Context-aware skill ranking (P2)

As an agent with a file in focus, I want skills ranked by relevance so the best skill surfaces first without loading full bodies.

**Acceptance**:
1. `list=true` with `rank=true` and `path` sorts skills by relevance score.
2. Frontmatter `tags` participate in ranking.
3. Higher-scoring skills appear first in `<available_skills>`.

### US3 - Skill execution plan extraction (P2)

As an agent evaluating a skill, I want a lightweight plan outline before loading the full body.

**Acceptance**:
1. `plan=true` returns markdown section/step outline without full body.
2. Plan mode works with cached discovery metadata only.

## Out of scope (future)

- Rust-level agent loop interception for skill policy (requires agent changes).
- Live arbor/codegraph queries during ranking (too slow for list path).
- Skill quality scoring telemetry loop.
