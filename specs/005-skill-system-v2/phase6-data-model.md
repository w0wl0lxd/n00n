# Data Model: Skill System V2 Phase 6

## ActiveSkillPolicy (Rust + JSON state)

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `name` | string | yes | Active skill name |
| `allowed_tools` | string[] | no | Allowlist mode |
| `disallowed_tools` | string[] | no | Denylist mode |

**Rules**: `allowed_tools` and `disallowed_tools` are mutually exclusive in skill frontmatter validation; runtime uses whichever is set on the loaded skill.

## SkillStep (frontmatter `steps`)

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `name` | string | yes | Step label |
| `section` | string | no | Markdown `##` heading to load |
| `tools` | string[] | no | Suggested tool intents |

## SkillTelemetryEvent (JSONL)

| Field | Type | Required |
|-------|------|----------|
| `timestamp` | integer | yes |
| `event` | string | yes (`list`, `load`, `plan`) |
| `skill_name` | string | no |
| `data` | object | no |

## GraphRankSignals

| Signal | Bonus | Condition |
|--------|-------|-----------|
| `arbor_indexed` | +5 | `n00n.arbor.available()` and status OK |
| `codegraph_indexed` | +3 | `.codegraph/` directory exists |
