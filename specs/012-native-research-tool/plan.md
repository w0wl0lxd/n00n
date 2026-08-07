# Implementation Plan: Native Research Tool

**Branch**: `012-native-research-tool` | **Date**: 2026-08-03 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/012-native-research-tool/spec.md`

---

## Summary

This feature adds a pure Lua plugin `plugins/research/init.lua` that orchestrates multi-source research using `subagent.launch()`. The tool accepts a research query, optional source filters, depth, output format, and citation requirements. It routes to built-in tools (websearch, webfetch, codegraph, arbor) and optional MCP tools (arxiv, exa, context7, thoughtbox), launches a research subagent with a strict system prompt and limited tool set, and returns a cited, structured report. The implementation follows existing patterns from `fusion` and `task` plugins, uses `ToolView` for UI rendering, `output_limits` for token efficiency, and respects permission scopes.

---

## Technical Context

**Language/Version**: Lua 5.1 (Luau runtime in n00n-lua)

**Primary Dependencies**:
- `n00n.subagent` — subagent launch helper (existing)
- `n00n.tool_view` — UI rendering (existing)
- `n00n.output_limits` — token truncation (existing)
- `n00n.structured_output` — JSON schema validation (existing)
- Built-in tools: `websearch`, `webfetch`, `codegraph`, `arbor` (existing)
- MCP tools: `arxiv`, `exa`, `context7`, `thoughtbox` (optional, external)

**Storage**: No new persistent storage. Optional notebook creation uses thoughtbox MCP.

**Testing**: `cargo test -p n00n-lua`, Lua plugin tests in `plugins/research/tests/` or `n00n-lua/tests/real_plugins_restore.rs`.

**Target Platform**: Linux primary; macOS secondary.

**Project Type**: CLI/TUI agent with built-in Lua plugins.

**Performance Goals**:
- Research tool calls should use ≤50% of the tokens compared to manual multi-tool chains for equivalent queries.
- Subagent launch overhead should be ≤2 seconds.

**Constraints**:
- No new Rust crates or dependencies.
- Pure Lua implementation following existing plugin patterns.
- Tool definition token count must not increase significantly.
- No bundled API keys or cloud providers.
- Permission scopes must be enforced.

**Scale/Scope**: Single new Lua plugin (`plugins/research/init.lua`), no Rust changes, optional integration with existing MCP tools.

---

## Constitution Check

*The project constitution is defined in `AGENTS.md`. The following gates apply before implementation:*

|| Gate | Status | Notes ||
||------|--------|-------||
|| No new `unsafe` without review | Pass | Pure Lua implementation, no Rust changes. ||
|| `cargo clippy --all --tests -- -D warnings` | TBD | Must pass before PR (no Rust changes expected). ||
|| `cargo deny check` | TBD | Must pass (no new dependencies). ||
|| No silent `.ok()` / default fallbacks | Pass | Errors from subagent launch and tool calls will be propagated. ||
|| TDD / failing test first | Pass | Each phase starts with a failing test. ||
|| DRY/SRP | Pass | Single plugin with clear responsibilities (validation, routing, orchestration). ||
|| No bundled credentials or cloud providers | Pass | MCP tools are optional and user-configured. ||

---

## Project Structure

### Documentation (this feature)

```text
specs/012-native-research-tool/
├── plan.md              # This file
├── research.md          # Existing research document
├── spec.md              # User-facing specification
├── data-model.md        # Entity and schema contracts
├── quickstart.md        # Validation guide
├── contracts/           # Tool schemas and API contracts
│   └── research.md
└── checklists/
    └── requirements.md
```

### Source Code (repository root)

```text
plugins/
└── research/
    ├── init.lua         # Main plugin implementation
    └── tests/
        └── spec.lua     # Lua plugin tests

n00n-lua/src/loader.rs  # Update BUNDLED_PLUGINS list
n00n-config/src/lib.rs  # Update DEFAULT_BUILTINS list (optional)
n00n-agent/src/prompt.rs # Update NATIVE_EFFICIENT_TOOLS list (optional)
```

**Structure Decision**: Single Lua plugin following the pattern of `fusion` and `task`. No new Rust crates. The plugin is self-contained and uses existing n00n Lua APIs.

---

## Complexity Tracking

No constitution violations expected. The implementation is pure Lua and follows established patterns. Binary size impact is negligible (single Lua plugin). Feature flags are not needed since the plugin can be disabled via config if desired.

---

## Module Responsibilities

### `plugins/research/init.lua`

Main plugin implementation.

- **Validation**: Validate input parameters (query, sources, depth, output_format, citations_required, max_sources).
- **Source mapping**: Map source names to tool names (web → websearch/webfetch, arxiv → arxiv MCP, etc.).
- **System prompt**: Build strict research system prompt with anti-hallucination rules and citation requirements.
- **Subagent launch**: Call `subagent.launch()` with `subagent_type = "research"`, `only_tools`, `except_tools`, `system_append`, and optional `output_schema`.
- **Output handling**: Return result with `ToolView.restore()` for UI, cost/usage tracking.
- **Graceful degradation**: Handle unavailable MCP tools, empty results, and conflicting citations.
- **Notebook creation**: Create thoughtbox notebook when `output_format = "notebook"` and thoughtbox MCP is available.

---

## Tool Schema

The tool schema follows the proposal in issue #237:

```lua
local schema = {
  type = "object",
  required = { "query" },
  properties = {
    query = { type = "string", description = "Research question" },
    sources = {
      type = "array",
      items = {
        type = "string",
        enum = { "web", "arxiv", "exa", "context7", "codegraph", "arbor" }
      },
      description = "Allowed sources (default: all available)"
    },
    depth = {
      type = "string",
      enum = { "quick", "thorough", "exhaustive" },
      default = "thorough",
      description = "Research depth"
    },
    output_format = {
      type = "string",
      enum = { "bullet_summary", "structured_json", "notebook" },
      default = "bullet_summary",
      description = "Output format"
    },
    citations_required = {
      type = "boolean",
      default = true,
      description = "Require source citations"
    },
    max_sources = {
      type = "integer",
      default = 8,
      description = "Max sources to query"
    }
  }
}
```

---

## Source to Tool Mapping

| Source | Tool(s) | Type |
|--------|---------|------|
| `web` | `websearch`, `webfetch` | Built-in |
| `arxiv` | `arxiv` MCP tool | Optional MCP |
| `exa` | `exa` MCP tool | Optional MCP |
| `context7` | `context7` MCP tool | Optional MCP |
| `codegraph` | `codegraph` | Built-in |
| `arbor` | `arbor` | Built-in |
| `thoughtbox` | `thoughtbox` MCP tool | Optional MCP (for notebook) |

---

## Subagent System Prompt

The subagent receives a strict system prompt appended via `system_append`:

```
You are a research subagent. Your role is to investigate the query using only the allowed tools and return a concise, cited report.

## Rules
- Use file:line for code citations, URLs for web sources, arXiv IDs for papers.
- Never invent file paths, function names, APIs, or paper titles.
- Cross-check claims against ≥2 independent sources when non-obvious.
- Prefer primary sources (source code, official docs, arXiv) over blog summaries.
- Keep output under 1000 words; use bullets/tables over prose walls.
- If a source returns no results, say "not found" with the tool+query tried.
- Respect the depth setting: quick (1-2 sources), thorough (4-8 sources), exhaustive (all available).
- When citations are required, every claim must include a source reference.
```

---

## Permission Scopes

The tool defines three permission scopes:

- `research.subagent` — Required for subagent usage (new scope).
- `research.web` — Required for web sources (delegates to existing `query` scope).
- `research.thoughtbox` — Required for notebook creation (delegates to thoughtbox scope).

These scopes are enforced by the n00n agent framework. The tool handler checks permissions before launching the subagent.

---

## Phased Roadmap

### Phase 0: Validation Spikes

1. **Subagent API verification**: Create a test plugin that calls `subagent.launch()` with `only_tools`, `except_tools`, and `system_append` to verify the API works as documented.
2. **Source tool availability**: Verify that `websearch`, `webfetch`, `codegraph`, and `arbor` are available as built-in tools.
3. **MCP tool availability**: Check if arxiv, exa, context7, and thoughtbox MCP tools are available in a test environment.
4. **ToolView pattern**: Verify `ToolView.restore()` usage by inspecting existing plugins (websearch, webfetch, codegraph).

### Phase 1: Plugin Scaffolding

1. Create `plugins/research/` directory structure.
2. Create `plugins/research/init.lua` with tool registration, schema, and empty handler.
3. Add `research` to `n00n-lua/src/loader.rs` `BUNDLED_PLUGINS` list.
4. Run `cargo check -p n00n-lua` to verify the plugin loads without errors.
5. Add basic test in `plugins/research/tests/spec.lua` that the tool is registered.

### Phase 2: Input Validation and Source Mapping

1. Implement input validation in the handler (query non-empty, sources enum values, depth enum values, output_format enum values).
2. Implement source to tool mapping function.
3. Add tests for validation errors (empty query, invalid source, invalid depth, invalid output_format).
4. Add tests for source mapping (web → websearch/webfetch, arxiv → arxiv MCP, etc.).

### Phase 3: System Prompt and Subagent Launch

1. Implement system prompt builder with anti-hallucination rules and citation requirements.
2. Implement subagent launch call with `subagent_type = "research"`, `only_tools`, `except_tools`, and `system_append`.
3. Add test that the subagent is launched with the correct parameters (mock subagent.launch).
4. Add test that recursive delegation tools are excluded.

### Phase 4: Output Handling and UI

1. Implement output handling with `ToolView.restore()` for UI rendering.
2. Implement cost/usage tracking in the return value.
3. Add test that output is correctly formatted for UI.
4. Add test that cost/usage metadata is returned.

### Phase 5: Graceful Degradation

1. Implement MCP tool availability check.
2. Implement graceful degradation when MCP tools are unavailable (use built-in tools, report skipped sources).
3. Implement handling of empty results from individual sources.
4. Add tests for graceful degradation scenarios.

### Phase 6: Notebook Creation

1. Implement notebook creation when `output_format = "notebook"` and thoughtbox MCP is available.
2. Implement fallback to bullet_summary when thoughtbox MCP is unavailable.
3. Add test for notebook creation.
4. Add test for notebook fallback.

### Phase 7: Permission Scopes

1. Add permission scopes to tool registration (`research.subagent`, `research.web`, `research.thoughtbox`).
2. Implement permission checks in the handler.
3. Add tests for permission enforcement.

### Phase 8: Integration and Validation

1. Update tool description to position research as a first-tier tool.
2. Add `research` to `NATIVE_EFFICIENT_TOOLS` list in `n00n-agent/src/prompt.rs` (optional).
3. Run full test suite: `cargo test -p n00n-lua`.
4. Run clippy: `cargo clippy --all --tests -- -D warnings`.
5. Run deny: `cargo deny check`.
6. Create integration test fixture with a small repo for single-source and multi-source tests.
7. Measure token efficiency against manual multi-tool chains.
8. Draft PR with performance comparison.

---

## Quick Validation Guide

See [quickstart.md](quickstart.md) for end-to-end validation steps.
