# Data Model: Native Research Tool

**Feature**: Native Research Tool (012)

**Purpose**: Define entity schemas and data contracts for the research tool

---

## ResearchQuery

The input structure for the research tool.

```lua
{
  query = string,              -- Required: Research question
  sources = array<string>,     -- Optional: Allowed sources (web, arxiv, exa, context7, codegraph, arbor)
  depth = string,              -- Optional: quick, thorough (default), exhaustive
  output_format = string,     -- Optional: bullet_summary (default), structured_json, notebook
  citations_required = boolean, -- Optional: true (default)
  max_sources = integer        -- Optional: 8 (default)
}
```

---

## SourceMapping

The mapping from source names to tool names.

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

## ResearchReport

The output structure returned by the research tool.

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

---

## Citation

A source reference within the research report.

```lua
{
  type = string,               -- file:line, URL, or arXiv ID
  source = string,             -- Source tool name
  evidence = string,          -- Evidence text or identifier
  context = string            -- Surrounding context (optional)
}
```

---

## Notebook

A thoughtbox notebook structure (when output_format = "notebook").

```lua
{
  title = string,              -- Notebook title
  sections = array<Section>,   -- Source sections
  synthesis = string           -- Final synthesis
}

Section = {
  source = string,             -- Source name
  findings = string,           -- Findings from this source
  citations = array<Citation>   -- Citations from this source
}
```

---

## StructuredReport

The output structure when output_format = "structured_json".

```lua
{
  summary = string,            -- Executive summary
  findings = array<Finding>,  -- Individual findings
  sources = array<Source>,     -- Sources consulted
  citations = array<Citation>  -- All citations
}

Finding = {
  claim = string,              -- Claim or fact
  evidence = string,           -- Supporting evidence
  citations = array<Citation>  -- Citations for this claim
}

Source = {
  name = string,               -- Source name
  status = string,             -- available, unavailable, no_results
  queries = array<string>       -- Queries attempted
}
```
