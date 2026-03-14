Execute Python code in a sandboxed interpreter. Tools are available as callable functions.

ALWAYS USE THE CODE_EXECUTION TOOL WHEN YOU HAVE MULTIPLE DEPENDENT TOOL CALLS. This dramatically improves performance.

Use this to reduce token usage and latency by:
- Processing large tool outputs in code (filter/aggregate/transform) instead of returning them to the conversation
- Chaining dependent tool calls where intermediate results don't need reasoning
- Running loops over many items (batch file checks, multi-file search, bulk operations)
- Performing computation on tool outputs (counting, sorting, formatting, deduplication)
- Filtering large webfetch / websearch results

Do NOT use for:
- Simple single-tool calls
- When you need to reason about intermediate results

IMPORTANT:
- All tools are async. You MUST `await` every tool call: `result = await read(path='file.txt')`
- All tools return strings (their formatted output), NOT structured Python objects. Parse the string output yourself (split on newlines, etc).
- Use `asyncio.gather()` for concurrent tool calls: `a, b = await asyncio.gather(read(path='a'), read(path='b'))`
- Available libs: re, asyncio, sys, os

Limitations:
- No imports, no classes, no filesystem/network access (fully sandboxed)
- 30 second timeout (configurable via `timeout` parameter)
