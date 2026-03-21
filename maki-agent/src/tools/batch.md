Executes multiple independent tool calls concurrently to reduce round-trips.

ALWAYS USE BATCH FOR MULTIPLE INDEPENDENT TOOL CALLS.

Payload: [{"tool": "read", "parameters": {"path": "src/main.rs"}}, ...]

Rules:
- 1-25 tool calls per batch
- All calls run in parallel; order NOT guaranteed
- Partial failures do not stop other calls
- Do NOT nest batch inside batch
- Do NOT use for dependent operations or when filtering results (use code_execution)
