Launch an autonomous research agent to explore the codebase.

The research agent has read-only tools (bash, read, glob, grep, webfetch) and runs independently with its own conversation. Use it for tasks that require broad codebase exploration, searching across many files, or gathering context that would be expensive to do inline.

When to use:
- Exploring unfamiliar parts of the codebase
- Searching for patterns across many files
- Gathering context about architecture or conventions
- Answering questions about how something works

When NOT to use:
- Reading a specific known file (use read directly)
- Searching for a specific string (use grep directly)
- Simple glob lookups (use glob directly)

Usage notes:
1. Launch multiple tasks concurrently when possible by calling this tool multiple times in the same response.
2. The agent's result is not visible to the user. Summarize it in your response.
3. Each invocation starts a fresh conversation. Your prompt should be detailed and self-contained.
4. Clearly state what information the agent should return.
