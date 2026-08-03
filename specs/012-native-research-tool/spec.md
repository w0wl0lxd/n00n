# Feature Specification: Native Research Tool

**Feature Branch**: `012-native-research-tool`

**Created**: 2026-08-03

**Status**: Draft

**Input**: GitHub issue #237: "Add a `research` built-in tool that orchestrates multi-source research and returns a concise, structured report, replacing ad-hoc chains of `websearch`, `webfetch`, `codegraph`, etc."

---

## User Scenarios & Testing

### User Story 1 - Single-source quick lookup (Priority: P1)

A user asks a factual question with a known source type (e.g., "What does function X do?") and wants a concise answer with proper citations without manually calling multiple tools.

**Why this priority**: This is the most common research pattern. A single tool that routes to the right source and returns a cited answer reduces token burn and manual orchestration overhead.

**Independent Test**: Invoke `research` with a codebase question and `sources = ["codegraph"]` on a fixture repository. Verify the tool returns a bullet summary with file:line citations without calling web tools.

**Acceptance Scenarios**:

1. **Given** a user asks a codebase question with `sources = ["codegraph"]`, **When** the user invokes `research`, **Then** the tool routes to codegraph and returns a bullet summary with file:line citations.
2. **Given** a user asks a documentation question with `sources = ["context7"]`, **When** the user invokes `research`, **Then** the tool queries context7 and returns a cited summary with library documentation references.
3. **Given** a user asks a question with no sources specified, **When** the user invokes `research`, **Then** the tool uses all available sources and returns a multi-source cited summary.

---

### User Story 2 - Multi-source synthesis (Priority: P1)

A user asks a cross-domain question (e.g., "Compare async frameworks in Rust") and wants a structured comparison synthesized from web, arxiv, and context7 sources.

**Why this priority**: Multi-source research is currently manual and token-inefficient. A single tool that orchestrates synthesis across sources with proper citations reduces agent burden and improves answer quality.

**Independent Test**: Invoke `research` with a comparative question, `sources = ["web", "arxiv", "context7"]`, and `output_format = "structured_json"`. Verify the tool returns a structured comparison table with citations from each source.

**Acceptance Scenarios**:

1. **Given** a user asks a comparative question with multiple sources, **When** the user invokes `research` with `output_format = "structured_json"`, **Then** the tool returns structured JSON with comparison data and per-source citations.
2. **Given** a user asks a research question with `depth = "thorough"`, **When** the user invokes `research`, **Then** the tool queries up to 8 sources and synthesizes findings into a coherent report.
3. **Given** a user asks a question with `citations_required = true`, **When** the user invokes `research`, **Then** every claim in the output includes a source citation (file:line, URL, or arXiv ID).

---

### User Story 3 - Notebook creation (Priority: P2)

A user requests `output_format = "notebook"` for complex research and wants the findings persisted as a thoughtbox notebook for future reference.

**Why this priority**: Complex research often needs to be revisited. Persisting findings as a notebook enables knowledge reuse and supports multi-session research workflows.

**Independent Test**: Invoke `research` with a complex question and `output_format = "notebook"`. Verify the tool creates a thoughtbox notebook with sections per source and a final synthesis.

**Acceptance Scenarios**:

1. **Given** a user requests `output_format = "notebook"`, **When** the user invokes `research`, **Then** the tool creates a thoughtbox notebook with source sections and synthesis.
2. **Given** a user requests notebook format, **When** thoughtbox MCP is unavailable, **Then** the tool falls back to bullet_summary and reports the limitation.
3. **Given** a notebook is created, **When** the user queries the notebook later, **Then** the findings are retrievable with proper citations intact.

---

### User Story 4 - Graceful degradation (Priority: P2)

A user requests research with sources that are unavailable (e.g., arxiv MCP not configured) and wants the tool to use available sources instead of failing.

**Why this priority**: MCP tools are optional. The research tool should degrade gracefully and use available built-in tools when MCP sources are unavailable.

**Independent Test**: Configure n00n without arxiv/exa/context7 MCP servers. Invoke `research` with `sources = ["arxiv", "web", "codegraph"]`. Verify the tool uses web and codegraph and reports arxiv as unavailable.

**Acceptance Scenarios**:

1. **Given** a user requests research with an unavailable MCP source, **When** the user invokes `research`, **Then** the tool uses available sources and reports which sources were skipped.
2. **Given** a user requests research with only unavailable sources, **When** the user invokes `research`, **Then** the tool returns an error listing unavailable sources and suggesting alternatives.
3. **Given** a user requests research with mixed available/unavailable sources, **When** the user invokes `research`, **Then** the tool proceeds with available sources and includes a degradation notice in the output.

---

### Edge Cases

- What happens when a source returns no results? The tool reports "not found" with the tool and query tried, and continues with other sources.
- What happens when all sources return no results? The tool returns a clear "no results found" message with the queries attempted.
- What happens when citations conflict across sources? The tool reports the conflict with per-source citations and does not arbitrarily resolve.
- What happens when the query is empty or invalid? The tool returns a validation error without launching the subagent.
- What happens when the subagent exceeds the token budget? The tool respects output_limits and truncates with a clear truncation notice.
- What happens when thoughtbox notebook creation fails? The tool falls back to bullet_summary and reports the error.

---

## Requirements

### Functional Requirements

- **FR-001**: The `research` tool MUST accept a `query` parameter (required) and return a cited research report.
- **FR-002**: The `research` tool MUST accept a `sources` array (optional) with values: `web`, `arxiv`, `exa`, `context7`, `codegraph`, `arbor`, `thoughtbox`.
- **FR-003**: The `research` tool MUST accept a `depth` parameter (optional) with values: `quick`, `thorough` (default), `exhaustive`.
- **FR-004**: The `research` tool MUST accept an `output_format` parameter (optional) with values: `bullet_summary` (default), `structured_json`, `notebook`.
- **FR-005**: The `research` tool MUST accept a `citations_required` parameter (optional, default: true).
- **FR-006**: The `research` tool MUST accept a `max_sources` parameter (optional, default: 8).
- **FR-007**: The tool MUST route `web` source to `websearch` and `webfetch` tools.
- **FR-008**: The tool MUST route `arxiv`, `exa`, and `context7` sources to their respective MCP tools when available.
- **FR-009**: The tool MUST route `codegraph` and `arbor` sources to their built-in tools.
- **FR-010**: The tool MUST use `subagent.launch()` with `subagent_type = "research"` and a strict system prompt.
- **FR-011**: The tool MUST restrict the subagent's tool set to only the mapped source tools.
- **FR-012**: The tool MUST exclude recursive delegation tools (`fusion_delegate`, `task`, `team`, `workflow`, `agent_control`, `blackboard`) from the subagent.
- **FR-013**: The tool MUST return file:line citations for codegraph/arbor sources, URLs for web sources, and arXiv IDs for papers.
- **FR-014**: The tool MUST respect `output_limits` and truncate output with a clear notice when limits are exceeded.
- **FR-015**: The tool MUST support `output_format = "notebook"` by creating a thoughtbox notebook when the MCP tool is available.
- **FR-016**: The tool MUST degrade gracefully when MCP sources are unavailable, using built-in tools and reporting skipped sources.
- **FR-017**: The tool MUST validate input before launching the subagent and return clear errors for invalid parameters.
- **FR-018**: The tool MUST handle empty or missing results from individual sources without crashing.
- **FR-019**: The tool MUST require the `research.subagent` permission scope for subagent usage.
- **FR-020**: The tool MUST require the `research.web` permission scope for web sources (delegates to existing `query` scope).
- **FR-021**: The tool MUST require the `research.thoughtbox` permission scope for notebook creation (delegates to thoughtbox scope).

### Key Entities

- **ResearchQuery**: The user's research question and configuration (query, sources, depth, output_format, citations_required, max_sources).
- **SourceMapping**: The mapping from source names to tool names (web → websearch/webfetch, arxiv → arxiv MCP, etc.).
- **ResearchSubagent**: The subagent launched with a strict system prompt, limited tool set, and optional output schema.
- **Citation**: A source reference with type (file:line, URL, arXiv ID) and evidence.
- **ResearchReport**: The output structure containing synthesized findings, citations, and metadata (cost, usage, model).
- **Notebook**: A thoughtbox notebook structure with sections per source and final synthesis (when output_format = "notebook").

---

## Success Criteria

### Measurable Outcomes

- **SC-001**: Single-source lookup (US1) returns correct cited answers in 100% of test cases on a fixture repository.
- **SC-002**: Multi-source synthesis (US2) returns structured JSON with correct citations in 100% of test cases.
- **SC-003**: Notebook creation (US3) creates a valid thoughtbox notebook when the MCP tool is available.
- **SC-004**: Graceful degradation (US4) uses available sources and reports skipped sources correctly when MCP tools are unavailable.
- **SC-005**: The tool respects `output_limits` and truncates output without crashing when limits are exceeded.
- **SC-006**: The tool validates input and returns clear errors for invalid parameters without launching the subagent.
- **SC-007**: The tool handles empty results from individual sources and continues with other sources.
- **SC-008**: Permission scopes are enforced correctly: `research.subagent`, `research.web`, `research.thoughtbox`.
- **SC-009**: The subagent is correctly restricted to only the mapped source tools and excludes recursive delegation tools.
- **SC-010**: Token efficiency: research tool calls use ≤50% of the tokens compared to manual multi-tool chains for equivalent queries.

---

## Assumptions

- The `subagent.launch()` API in `plugins/lib/n00n/subagent.lua` supports `only_tools`, `except_tools`, `system_append`, and `output_schema` parameters.
- MCP tools (arxiv, exa, context7, thoughtbox) are optional and may not be configured.
- Built-in tools (websearch, webfetch, codegraph, arbor) are always available.
- The thoughtbox MCP tool supports notebook creation when available.
- Permission scopes are enforced by the n00n agent framework.
- The `output_limits` module provides token/line budget resolution.
- The `ToolView` module provides UI rendering for tool results.
