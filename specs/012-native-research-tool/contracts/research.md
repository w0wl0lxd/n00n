# Tool Contract: Research

**Tool Name**: `research`

**Plugin**: `plugins/research/init.lua`

---

## Tool Schema

```lua
local schema = {
  type = "object",
  required = { "query" },
  properties = {
    query = {
      type = "string",
      description = "Research question"
    },
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

## Tool Description

```
Orchestrate multi-source research and return a concise, cited report. Routes to built-in tools (websearch, webfetch, codegraph, arbor) and optional MCP tools (arxiv, exa, context7, thoughtbox). Uses a research subagent with strict anti-hallucination rules and citation requirements.
```

---

## Permission Scopes

- `research.subagent` — Required for subagent usage
- `research.web` — Required for web sources
- `research.thoughtbox` — Required for notebook creation

---

## Handler Contract

### Input

- `query` (string, required): Research question
- `sources` (array<string>, optional): Allowed sources
- `depth` (string, optional): Research depth (quick, thorough, exhaustive)
- `output_format` (string, optional): Output format (bullet_summary, structured_json, notebook)
- `citations_required` (boolean, optional): Require citations (default: true)
- `max_sources` (integer, optional): Max sources to query (default: 8)

### Output

```lua
{
  llm_output = string,         -- Synthesized research report
  body = string,               -- UI-rendered body (ToolView.restore())
  is_error = boolean,          -- Error flag
  cost = number,               -- Cost in dollars (optional)
  usage = table,               -- Token usage (optional)
  model = string               -- Model used (optional)
}
```

### Behavior

1. Validate input parameters
2. Map sources to tool names
3. Build strict research system prompt
4. Launch research subagent with limited tool set
5. Return cited report with UI rendering
6. Handle unavailable MCP tools gracefully
7. Create thoughtbox notebook when requested and available

---

## Error Conditions

| Condition | Error Message |
|-----------|---------------|
| Empty query | "error: query is required" |
| Invalid source enum | "error: invalid source: {source}" |
| Invalid depth enum | "error: invalid depth: {depth}" |
| Invalid output_format enum | "error: invalid output_format: {format}" |
| All sources unavailable | "error: no available sources for research" |
| Subagent launch failure | "error: subagent launch failed: {error}" |
