Execute Python code in a sandboxed interpreter. Tools are available as callable functions.

Use for dependent/chained tool calls and filtering/processing results.
Good use case is filtering on web tool results.

- All tools are async: `result = await read(path='file.txt')`
- Tools return strings, not Python objects. Parse output yourself.
- Use `asyncio.gather()` for concurrent calls within one execution.
- Available libs: re, asyncio, sys, os.
- No imports, no classes, no filesystem/network access.
- 30 second timeout (configurable via `timeout` parameter).
- Avoid calling another tool when no transformation of its output is performed.
