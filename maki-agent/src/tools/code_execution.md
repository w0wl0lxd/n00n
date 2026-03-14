Execute Python code in a sandboxed interpreter. Tools are available as callable functions.

Use for dependent/chained tool calls and filtering/processing results. This dramatically improves performance over sequential tool calls. Do NOT use for independent parallel calls (use batch instead).

Good use cases:
- Chaining dependent calls where output of one feeds into another
- Processing/filtering large tool outputs (aggregate/transform/count)
- Running loops over many items
- Filtering large webfetch / websearch results

Do NOT use for:
- Multiple independent tool calls with no processing (use batch)
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
