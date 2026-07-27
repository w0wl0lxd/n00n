# Quickstart: Skill System V2 Phase 6

## Hard policy enforcement

1. Create `.agents/skills/safe-review/SKILL.md`:

```yaml
---
name: safe-review
description: Read-only review workflow
allowed-tools: read, grep, index
---
# Safe review
Use read and grep only.
```

2. In an agent session, load the skill: `skill(name="safe-review")`.
3. Subsequent `bash` calls are rejected by the agent with a skill-policy error.

## Graph-informed ranking

```json
{"list": true, "path": "src/api/agent.rs", "rank": true, "graph_rank": true}
```

Skills with matching `paths` and an indexed project receive a graph bonus in the score prefix.

## Telemetry

```json
{"list": true, "include_telemetry": true}
```

Events append to `~/.local/state/n00n/projects/<project-id>/skills/events.jsonl`.

## Structured execution plan

```yaml
---
name: ship-feature
description: Ship a feature safely
steps:
  - name: Research
    section: Research
    tools: [read, codegraph]
  - name: Implement
    section: Implementation
    tools: [edit, bash]
---
```

```json
{"name": "ship-feature", "plan": true}
```
