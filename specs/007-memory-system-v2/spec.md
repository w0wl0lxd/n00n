# Feature Specification: Memory System V2

**Feature Branch**: `feat/memory-system-v2`  
**Created**: 2026-07-27  
**Status**: Draft  
**Input**: Memory is a first-class tool but only supports flat view/write/delete with no search, metadata, recall ranking, or telemetry — unlike competitor agent memory systems and n00n's own skill v2.

## Competitor gap analysis (verified 2026-07-27)

| Capability | Claude Code | Letta | PMB / community plugins | n00n (before) | This PR |
|------------|-------------|-------|-------------------------|---------------|---------|
| Persistent project memory | auto memory + CLAUDE.md | archival memory | SQLite + hooks | flat markdown files | flat files + metadata |
| Semantic / keyword search | on-demand via tools | archival_memory_search | hybrid BM25+vector | none | heuristic keyword ranking (offline) |
| Tags / organization | implicit | tags on insert | tags + graph | none | YAML frontmatter tags/topic |
| Lite vs deep layers | auto memory summaries | memory blocks vs archival | lite pointers + deep fetch | none | `layer: lite\|deep` + prompt injection |
| Session-start injection | CLAUDE.md + auto memory | core memory blocks | hooks auto-recall | file count hint only | lite memory summaries in prompt hint |
| Telemetry | none built-in | API events | status commands | none | JSONL events.jsonl |
| Append without rewrite | N/A | insert | append tools | full write only | `append` command |

## User Scenarios & Testing

### User Story 1 — Search and recall memories (Priority: P1)

As an agent, I want to search project memories by keywords and tags so I can retrieve relevant context without reading every file.

**Independent Test**: Write two memories with distinct tags, run `search` with a query matching one, verify ranked results.

**Acceptance Scenarios**:
1. **Given** memories with tags, **When** `search` runs with matching query, **Then** relevant files rank highest.
2. **Given** `tags` filter, **When** `search` runs, **Then** only matching-tag memories appear.
3. **Given** no matches, **When** `search` runs, **Then** a clear empty result is returned.

### User Story 2 — Structured metadata (Priority: P1)

As a user, I want memory files to carry tags, topic, importance, and layer so the agent can organize and prioritize recall.

**Acceptance Scenarios**:
1. **Given** `write` with tags/topic, **When** file is saved, **Then** YAML frontmatter is written.
2. **Given** existing frontmatter, **When** `view` loads file, **Then** body excludes frontmatter.
3. **Given** `layer: lite`, **When** session starts, **Then** synopsis appears in prompt hint.

### User Story 3 — Append without full rewrite (Priority: P2)

As an agent, I want to append notes to an existing memory without re-sending full content.

**Acceptance Scenarios**:
1. **Given** existing memory, **When** `append` adds text, **Then** content is appended after body with newline separator.
2. **Given** line limit exceeded, **When** `append` runs, **Then** error matches write limit contract.

### User Story 4 — Discovery index and telemetry (Priority: P2, deferred to v2.1)

As an operator, I want indexed discovery and JSONL telemetry for memory operations.

**Status**: Deferred — inline discovery is used in v2; per-project index cache and telemetry ship in v2.1 (see tasks.md T004/T009).

**Acceptance Scenarios** (v2.1):
1. **Given** unchanged memory dir, **When** list/search runs twice, **Then** index cache is reused.
2. **Given** `include_telemetry=true`, **When** search completes, **Then** event is appended to `memories/events.jsonl`.

## Requirements

- **FR-001**: `memory` MUST support `search` command with required `query` and optional `tags`, `path` (focus), `limit`.
- **FR-002**: `memory` MUST support `append` command with `path` and `content`.
- **FR-003**: `parse_frontmatter` MUST extract tags, topic, importance (1-5), layer (lite|deep), synopsis. (`paths` deferred — see contracts/memory_recall.md.)
- **FR-004**: `write` MUST accept optional metadata fields and persist frontmatter.
- **FR-005**: `view` without path MUST list entries with metadata; optional `query` ranks list.
- **FR-006**: Prompt hint MUST inject lite-layer summaries (capped) instead of only file count.
- **FR-007**: ~~Discovery index MUST cache per-project metadata with mtime fingerprint invalidation.~~ **Deferred to v2.1** — v2 uses inline discovery (tasks.md T004).
- **FR-008**: ~~Telemetry MUST append to `projects/{id}/memories/events.jsonl` when `include_telemetry=true`.~~ **Deferred to v2.1** (tasks.md T009).

## Success Criteria

- **SC-001**: Search ranking tests pass in Lua spec and plugin_host.
- **SC-002**: Existing view/write/delete tests continue passing.
- **SC-003**: Frontmatter round-trip tests pass.
- **SC-004**: `just gen-docs` updates memory tool docs.
