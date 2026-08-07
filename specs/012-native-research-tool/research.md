# Native research tool — Research

## Summary

The `research` tool should be a pure Lua plugin (`plugins/research/init.lua`) that uses `subagent.launch()` to orchestrate multi-source research with a strict system prompt, limited tool set, and structured output. It follows existing patterns from `fusion`, `task`, and source tools (`websearch`, `webfetch`, `codegraph`, `arbor`).

## Evidence

- **Issue #237** proposes: `query`, `sources` array, `depth`, `output_format`, `citations_required`
- **Fusion pattern** (`plugins/fusion/init.lua:111-129`): Uses `subagent.launch()` with `subagent_type`, `model_tier`, `audience`, `except_tools`, `system_append`
- **Task pattern** (`plugins/task/init.lua:165-177`): Uses `subagent.launch()` with `output_schema` for structured output, `preview` for UI
- **Subagent API** (`plugins/lib/n00n/subagent.lua:35-244`): Full parameter set including `only_tools`, `except_tools`, `budget`, `fail_on_pricing_error`
- **Source tools**: `websearch` (init.lua:29-48), `webfetch` (init.lua:80-94), `codegraph` (init.lua:16-71) all use `ToolView`, `output_limits`, `header`/`restore`, `permission_scopes`
- **Blackboard** (`plugins/blackboard/init.lua:526-600`): Shared state via posts/claims for multi-agent coordination
- **Tool registration** (`n00n-lua/src/api/agent.rs:636-737`): `n00n.agent.session()` accepts `tools`, `local_tools`, `audience`, `mode`, `include_mcp`, `except`

## Map

**Entry points:**
- `plugins/research/init.lua` — new plugin following `fusion`/`task` pattern
- `n00n.api.register_tool()` — tool registration with schema, handler, permission_scopes

**Key symbols / files:**
- `subagent.launch()` — `plugins/lib/n00n/subagent.lua:35`
- `n00n.agent.tools()` — `n00n-lua/src/api/agent.rs:453`
- `ToolView.restore()` — UI rendering pattern from all source tools
- `output_limits.resolve()` — token efficiency from `websearch`/`webfetch`
- `blackboard` — optional shared state for multi-step findings

**Call / data flow:**
1. User calls `research` tool with query, sources, depth, output_format
2. Handler validates input, resolves allowed tools from `sources` array
3. Handler calls `subagent.launch()` with:
   - `subagent_type = "research"`
   - `only_tools` = filtered source tools
   - `system_append` = strict research prompt (anti-hallucination, citation rules)
   - `output_schema` = report schema if `output_format != "bullet_summary"`
4. Subagent uses source tools (websearch, webfetch, codegraph, arbor, context7, exa, arxiv)
5. Handler returns `{ llm_output, cost?, usage?, model? }` with ToolView body

## Recommended Tool Schema

```lua
local schema = {
  type = "object",
  required = { "query" },
  properties = {
    query = { type = "string", description = "Research question" },
    sources = {
      type = "array",
      items = { type = "string", enum = { "web", "arxiv", "exa", "context7", "codegraph", "arbor" } },
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

## Handler Flow

1. **Validate input** — check query non-empty, sources enum values
2. **Map sources to tools**:
   - `web` → `websearch`, `webfetch`
   - `arxiv` → arxiv MCP tool
   - `exa` → exa MCP tool
   - `context7` → context7 MCP tool
   - `codegraph` → `codegraph`
   - `arbor` → `arbor`
3. **Build system prompt** (appended via `system_append`):
   - Anti-hallucination rules
   - Citation format requirements (file:line, URL, arXiv id)
   - Token budget warnings
   - Conciseness mandates
4. **Call `subagent.launch()`** with:
   - `subagent_type = "research"`
   - `only_tools` = mapped tool names
   - `include_mcp = true` (for arxiv/exa/context7)
   - `except_tools = { "fusion_delegate", "task", "team", "workflow", "agent_control", "blackboard" }`
   - `output_schema` = report schema if `output_format == "structured_json"`
5. **Return result** with `ToolView.restore()` for UI, cost/usage tracking

## Subagent Prompt Design

```
You are a research subagent. Your role is to investigate the query using only the allowed tools and return a concise, cited report.

## Rules
- Use file:line for code citations, URLs for web sources, arXiv IDs for papers.
- Never invent file paths, function names, APIs, or paper titles.
- Cross-check claims against ≥2 independent sources when non-obvious.
- Prefer primary sources (source code, official docs, arXiv) over blog summaries.
- Keep output under 1000 words; use bullets/tables over prose walls.
- If a source returns no results, say "not found" with the tool+query tried.
```

## Dependencies and Permission Scopes

**Dependencies:**
- `n00n.subagent` — subagent launch helper
- `n00n.tool_view` — UI rendering
- `n00n.output_limits` — token truncation
- `n00n.structured_output` — JSON schema validation (for structured_json format)
- Source tools: `websearch`, `webfetch`, `codegraph`, `arbor` (built-in)
- MCP tools: arxiv, exa, context7 (optional, via `include_mcp = true`)

**Permission scopes:**
- `research.web` — for websearch/webfetch (delegates to existing `query`/`url` scopes)
- `research.subagent` — for subagent usage (new scope)
- `research.thoughtbox` — for notebook creation (optional, delegates to thoughtbox scope)

**Config options:**
```lua
opts = n00n.api.register_options({
  default_depth = { default = "thorough", desc = "Default research depth" },
  default_format = { default = "bullet_summary", desc = "Default output format" },
  max_sources = { default = 8, min = 1, desc = "Max sources per query" },
  timeout_secs = { default = 300, min = 60, desc = "Research timeout" },
})
```

## Top 3 User Stories

**P1: Single-source quick lookup**
- User asks a factual question with known source type (e.g., "What does function X do?")
- Tool routes to codegraph/arbor, returns bullet summary with file:line citations
- Independent test: Can answer codebase questions without web access

**P2: Multi-source synthesis**
- User asks a cross-domain question (e.g., "Compare async frameworks in Rust")
- Tool queries web, arxiv, context7; returns structured JSON with comparison table
- Independent test: Can synthesize 3+ sources into coherent comparison

**P3: Notebook creation**
- User requests `output_format=notebook` for complex research
- Tool creates thoughtbox notebook with sections per source, final synthesis
- Independent test: Can export research session to persistent notebook

## Risks and Open Questions

**Risks:**
- **Token bloat**: Subagent may call too many sources or return verbose output. Mitigation: `max_sources` cap, `output_limits.resolve()`, strict system prompt.
- **Citation hallucination**: Model may invent file paths or URLs. Mitigation: Anti-hallucination rules, cross-check requirement, validation against tool results.
- **MCP dependency**: arxiv/exa/context7 require MCP servers. Mitigation: Graceful degradation when MCP unavailable, fallback to built-in tools only.
- **Permission scope complexity**: New `research.*` scopes may confuse users. Mitigation: Clear error messages, delegate to existing scopes where possible.

**Open questions:**
- Should `research` support `background` mode like `task`?
- Should intermediate findings be stored in blackboard for multi-step research?
- How to handle conflicting information from multiple sources?
- Should `notebook` format require thoughtbox MCP or create local markdown?
