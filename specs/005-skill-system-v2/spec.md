# Feature Specification: Skill System V2

**Feature Branch**: `feat/skill-system-v2`  
**Created**: 2026-07-27  
**Status**: Implemented (PR #172)  
**Input**: User request to review current n00n skill implementation, match competitor capabilities, and one-up with stronger loading/efficiency behavior.

## User Scenarios & Testing

### User Story 1 - Recursive discovery for nested skills (Priority: P1)

As a user in a monorepo, I want skills discovered from nested skill folders so package-local workflows become available without flattening directory layouts.

**Why this priority**: Competitor parity and immediate usability for large repos.

**Independent Test**: Add a nested `SKILL.md` under `.agents/skills/<group>/<skill>/` and verify `skill list` includes it.

**Acceptance Scenarios**:
1. **Given** nested skill directories, **When** `skill` lists skills, **Then** nested skills are discovered.
2. **Given** nested skills, **When** one is loaded by name, **Then** body content is returned.

---

### User Story 2 - Frontmatter policy fields (Priority: P1)

As a skill author, I want `disable-model-invocation` and `paths` frontmatter parsed so skills can be manually gated and path-scoped.

**Why this priority**: Matches modern skill metadata conventions and reduces irrelevant context.

**Independent Test**: Add skill files with each field and verify listing/filtering behavior.

**Acceptance Scenarios**:
1. **Given** `disable-model-invocation: true`, **When** `skill list` runs, **Then** skill is hidden by default.
2. **Given** `include_manual=true`, **When** `skill list` runs, **Then** manual-only skills appear.
3. **Given** `paths` globs, **When** `path` input matches, **Then** skill appears; otherwise it does not.

---

### User Story 3 - Scoped loading ergonomics (Priority: P2)

As an agent/tooling caller, I want list and load behavior to share the same scope filter semantics so wrong-scope skills are not accidentally loaded.

**Why this priority**: Predictable behavior avoids accidental policy bypass and user confusion.

**Independent Test**: For a scoped skill, verify out-of-scope list hides it and out-of-scope load returns not found.

## Edge Cases

- Invalid `paths` frontmatter types are ignored safely.
- Empty/whitespace path tokens are ignored.
- Windows-style and unix-style input paths normalize before matching.
- Skills with missing/invalid frontmatter still load by fallback inferred names.

## Requirements

### Functional Requirements

- **FR-001**: Skill discovery MUST recurse into nested directories under each skill root.
- **FR-002**: `parse_frontmatter` MUST normalize `paths` from either comma string or YAML list into string arrays.
- **FR-003**: `parse_frontmatter` MUST normalize `disable-model-invocation` into boolean `manual_only`.
- **FR-004**: `skill list` MUST hide manual-only skills unless `include_manual=true`.
- **FR-005**: `skill` MUST accept optional `path` input and apply frontmatter `paths` filters to both list and load.
- **FR-006**: Unknown or out-of-scope skill loads MUST preserve existing error contract (`skill not found` style).

### Key Entities

- **Skill metadata**: `name`, `description`, `manual_only`, `paths`, `location`, `scope_root`.
- **Skill roots**: discovered roots from config/global/project directories.
- **Focus path**: optional path parameter used for path-scope matching.

## Success Criteria

- **SC-001**: Nested skill discovery works in integration tests.
- **SC-002**: Manual-only visibility toggling works in integration tests.
- **SC-003**: Path-scoped list/load filtering works in integration tests.
- **SC-004**: Existing skill tests continue passing.
