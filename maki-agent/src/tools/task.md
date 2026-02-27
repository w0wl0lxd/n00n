Launch an autonomous subagent to perform tasks independently.

Subagent types (set via `subagent_type`):
- `research` (default): Read-only tools (bash, read, glob, grep, webfetch). For codebase exploration, searching across files, or gathering context.
- `general`: Full tool access (bash, read, write, edit, multiedit, glob, grep, webfetch). For delegating implementation work — writing code, making edits, or executing multi-step changes.

When to use `research`:
- Exploring unfamiliar parts of the codebase
- Searching for patterns across many files
- Gathering context about architecture or conventions
- Answering questions about how something works

When to use `general`:
- Delegating a self-contained implementation task
- Making changes across multiple files in parallel
- Performing refactors or migrations that can be described precisely

When NOT to use:
- Reading a specific known file (use read directly)
- Searching for a specific string (use grep directly)
- Simple glob lookups (use glob directly)
- Tasks requiring user interaction or clarification

Usage notes:
1. Launch multiple tasks concurrently when possible by calling this tool multiple times in the same response.
2. The agent's result is not visible to the user. Summarize it in your response.
3. Each invocation starts a fresh conversation. Your prompt should be detailed and self-contained.
4. Clearly state what information the agent should return.
