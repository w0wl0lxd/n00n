# Feature Specification: Native Tools Audit

**Feature Branch**: `014-native-tools-audit`

**Created**: 2026-08-03

**Status**: Draft

**Input**: GitHub issue #239: "Audit and expand other ad-hoc CLI/API native tool opportunities"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Audit methodology documentation (Priority: P1)

A project maintainer wants a systematic approach to identify ad-hoc CLI/API calls that should become native tools, so that the tool set expands purposefully based on evidence rather than guesswork.

**Why this priority**: A documented audit methodology provides the foundation for all future native tool decisions. Without it, tool additions are ad-hoc and may miss high-leverage opportunities or waste effort on low-value candidates.

**Independent Test**: Can be fully tested by applying the documented methodology to the n00n codebase and verifying it produces a ranked candidate table with token-savings estimates.

**Acceptance Scenarios**:

1. **Given** the audit methodology document, **When** a maintainer searches AGENTS.md and skill files for CLI patterns, **Then** they can categorize findings by frequency and token cost using the provided framework.
2. **Given** the candidate evaluation rubric, **When** a maintainer assesses a potential tool, **Then** they can assign a feasibility score and priority based on the documented criteria.
3. **Given** the ranked backlog, **When** a maintainer reviews the top 3-5 candidates, **Then** each has a one-page design note and a recommended priority.

---

### User Story 2 - Ranked candidate backlog (Priority: P1)

A project maintainer wants a prioritized list of the next native tools to implement, with token-savings estimates and design notes, so that implementation effort focuses on the highest-leverage opportunities.

**Why this priority**: The ranked backlog directly informs the implementation roadmap. Without it, maintainers cannot make data-driven decisions about which tools to build next.

**Independent Test**: Can be fully tested by reviewing the candidate table in research.md and verifying it includes frequency, token cost, feasibility, and priority for each candidate.

**Acceptance Scenarios**:

1. **Given** the candidate table, **When** a maintainer reviews the cargo entry, **Then** they see frequency (High), token cost (High), feasibility (Rust crate or Lua with JSON parsing), and priority (High).
2. **Given** the top 3-5 recommended tools, **When** a maintainer reads the design notes, **Then** each includes a proposed implementation approach, tool schema, and expected token savings.
3. **Given** the ranked backlog, **When** a maintainer considers the docker/podman entry, **Then** they see it is deprioritized (Low) due to minimal usage in n00n workflows.

---

### User Story 3 - Follow-up issue recommendations (Priority: P2)

A project maintainer wants concrete next steps after the audit, so that the findings translate into actionable work without additional planning overhead.

**Why this priority**: Follow-up issues bridge the gap between research and implementation. Without them, the audit risks becoming a one-time document rather than an ongoing process.

**Independent Test**: Can be fully tested by verifying that GitHub issues exist for the top-priority candidates with references back to the audit.

**Acceptance Scenarios**:

1. **Given** the audit findings, **When** a maintainer reviews the follow-up recommendations, **Then** they see at least one issue created for the highest-priority candidate (cargo).
2. **Given** the follow-up issues, **When** a maintainer opens the cargo tool issue, **Then** it references the audit (issue #239), includes the design note from research.md, and links to epic #240.
3. **Given** the lower-priority candidates, **When** a maintainer reviews the recommendations, **Then** they see these are deferred to future audits rather than immediately tracked as issues.

---

### Edge Cases

- What happens when a candidate tool has no clear structured-output alternative? The audit marks it as low feasibility and recommends RTK hardening instead of native implementation.
- What happens when session transcript data is unavailable for frequency analysis? The audit relies on codebase evidence (AGENTS.md, skill files, justfile) and notes the limitation.
- What happens when a candidate tool is already tracked in another epic (e.g., github/gix in #236)? The audit excludes it from the ranked backlog and references the existing work.
- What happens when token-savings estimates cannot be quantified without session data? The audit provides qualitative estimates based on RTK compression claims and marks the need for empirical validation.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The audit methodology MUST document a systematic approach for identifying ad-hoc CLI/API patterns in AGENTS.md, skill files, and session transcripts.
- **FR-002**: The audit methodology MUST include a rubric for categorizing candidates by frequency, token cost, feasibility, and priority.
- **FR-003**: The candidate table MUST include columns for tool name, current ad-hoc call pattern, frequency, token cost, feasibility, and priority.
- **FR-004**: The top 3-5 recommended tools MUST each have a one-page design note with implementation approach, tool schema, and expected token savings.
- **FR-005**: The audit MUST recommend at least one follow-up GitHub issue for the highest-priority candidate.
- **FR-006**: The audit MUST exclude candidates already tracked in active epics (e.g., github/gix in #236, tmux in #240).
- **FR-007**: The audit MUST document risks and open questions for each top candidate (e.g., cargo_metadata limitations, justfile parsing complexity).
- **FR-008**: The audit MUST provide token-savings estimates based on RTK compression claims or qualitative assessment when session data is unavailable.

### Key Entities

- **Audit Methodology**: A documented process for identifying and evaluating ad-hoc CLI/API patterns as candidates for native tools.
- **Candidate Table**: A ranked list of potential native tools with attributes: tool name, current pattern, frequency, token cost, feasibility, priority.
- **Design Note**: A one-page document for a top candidate describing implementation approach, tool schema, and expected token savings.
- **Follow-up Issue**: A GitHub issue created for a top-priority candidate that references the audit and includes the design note.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: The audit methodology is documented in research.md with clear steps for identifying and evaluating candidates.
- **SC-002**: The candidate table includes at least 7 candidates (cargo, just, docs, docker/podman, nix, ssh/remote, npm/pip) with frequency, token cost, feasibility, and priority assigned.
- **SC-003**: The top 3-5 recommended tools each have a one-page design note in research.md.
- **SC-004**: At least one follow-up GitHub issue is created for the highest-priority candidate (cargo).
- **SC-005**: The audit excludes candidates already tracked in active epics (github/gix, tmux).
- **SC-006**: Token-savings estimates are provided for each top candidate, with a note on whether they are empirical or qualitative.

## Assumptions

- Session transcript data may not be available for frequency analysis; the audit relies on codebase evidence as a proxy.
- Token-savings estimates are based on RTK compression claims (60-90%) until empirical session data is available.
- The audit focuses on tools that appear in n00n workflows; tools used only in external projects are out of scope.
- Native tool implementation is deferred for tools already tracked in active epics; the audit focuses on remaining opportunities.
- The audit does not implement code; it produces documentation and follow-up issues for future implementation phases.
