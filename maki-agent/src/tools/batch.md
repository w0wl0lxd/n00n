Executes multiple independent tool calls concurrently to reduce round-trips.

ALWAYS USE THE BATCH TOOL WHEN YOU HAVE MULTIPLE INDEPENDENT TOOL CALLS. This dramatically improves performance.

Payload format:
[{"tool": "read", "parameters": {"path": "src/main.rs"}}, {"tool": "grep", "parameters": {"pattern": "TODO"}}]

Rules:
- 1-25 tool calls per batch
- All calls run in parallel; order NOT guaranteed
- Partial failures do not stop other calls
- Do NOT nest batch inside batch

Good use cases:
- Reading multiple files
- grep + glob + read combos
- Multiple bash commands
- Multi-part edits on same or different files

When NOT to use:
- Operations depending on prior tool output (e.g. write then read same file)
- Ordered stateful mutations where sequence matters
- When results need filtering, aggregation, or conditional logic (use code_execution instead)
