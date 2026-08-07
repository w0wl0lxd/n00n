# Quick Validation Guide: Native Research Tool

**Feature**: Native Research Tool (012)

**Purpose**: End-to-end validation steps for the research tool

---

## Prerequisites

- n00n built with the research plugin
- Built-in tools available: websearch, webfetch, codegraph, arbor
- Optional: MCP tools configured (arxiv, exa, context7, thoughtbox)

---

## Validation Steps

### 1. Plugin Registration

```bash
# Start n00n and verify the research tool is listed
n00n
# In the agent session, ask: "List available tools"
# Verify "research" is in the tool list
```

**Expected**: The `research` tool is listed in available tools.

---

### 2. Single-Source Quick Lookup (US1)

```bash
# In n00n, invoke research with a codebase question
research(query="What does the subagent.launch function do?", sources=["codegraph"])
```

**Expected**: Returns a bullet summary with file:line citations from codegraph.

---

### 3. Multi-Source Synthesis (US2)

```bash
# In n00n, invoke research with multiple sources
research(query="Compare async frameworks in Rust", sources=["web", "context7"], output_format="structured_json")
```

**Expected**: Returns structured JSON with comparison data and per-source citations.

---

### 4. Notebook Creation (US3)

```bash
# In n00n, invoke research with notebook format (requires thoughtbox MCP)
research(query="Explain the architecture of n00n", output_format="notebook")
```

**Expected**: Creates a thoughtbox notebook with source sections and synthesis.

---

### 5. Graceful Degradation (US4)

```bash
# Configure n00n without MCP tools, then invoke research
research(query="Latest Rust async runtime benchmarks", sources=["arxiv", "web"])
```

**Expected**: Uses web source, reports arxiv as unavailable, returns results with degradation notice.

---

### 6. Input Validation

```bash
# Test invalid inputs
research(query="")  # Empty query
research(query="test", sources=["invalid_source"])  # Invalid source
research(query="test", depth="invalid_depth")  # Invalid depth
```

**Expected**: Returns clear validation errors without launching the subagent.

---

### 7. Permission Enforcement

```bash
# Test permission checks (requires permission system)
# Configure n00n without research.subagent permission
research(query="test")
```

**Expected**: Returns permission denied error.

---

## Test Suite

Run the Lua plugin tests:

```bash
cargo test -p n00n-lua
```

**Expected**: All research plugin tests pass.

---

## Performance Validation

Measure token efficiency:

```bash
# Manual multi-tool chain (baseline)
websearch(query="test")
webfetch(url="...")
codegraph(query="test")

# Research tool (optimized)
research(query="test", sources=["web", "codegraph"])
```

**Expected**: Research tool uses ≤50% of the tokens compared to manual chain.

---

## Smoke Test Summary

- [ ] Plugin registered
- [ ] Single-source lookup works
- [ ] Multi-source synthesis works
- [ ] Notebook creation works (if thoughtbox available)
- [ ] Graceful degradation works
- [ ] Input validation works
- [ ] Permission enforcement works
- [ ] Test suite passes
- [ ] Token efficiency target met
