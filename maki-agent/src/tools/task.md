Launch an autonomous subagent to perform tasks independently.

Subagent types (set via `subagent_type`):
- `research` (default): Read-only tools (bash, read, index, glob, grep, webfetch, batch, code_execution). For codebase exploration, searching across files, or gathering context.
- `general`: Full tool access (bash, read, index, write, edit, multiedit, glob, grep, webfetch, batch, code_execution). For delegating implementation work - writing code, making edits, or executing multi-step changes.

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
- Reading multiple files (use batch or code_execution; subagent output returns to main context, no savings, just latency)
- Searching for a specific string (use grep directly)
- Simple glob lookups (use glob directly)
- Tasks requiring user interaction or clarification

Usage notes:
1. Launch multiple tasks concurrently when possible by calling this tool multiple times in the same response.
2. The agent's result is not visible to the user. Summarize it in your response.
3. Each invocation starts a fresh conversation with no access to your history. Your prompt is its ONLY context.
4. Clearly state what information the agent should return.
5. Inline any known context (type definitions, signatures, patterns, code snippets) directly into the prompt - don't make the subagent rediscover what you already know. Especially important for parallel tasks sharing context: embed it in each prompt.
6. **Output economy**: The subagent's entire final response is injected into your context. Tell it to return concise summaries with file:line references - not full file contents or large code blocks. Verbose subagent output wastes YOUR token budget.
7. **Context hygiene**: The subagent's exploration stays out of your context. Use task to offload large searches, multi-file reads, or implementation that would otherwise bloat your conversation.
