Execute Python code in a sandboxed interpreter. Tools are available as callable functions.

This is your most token-efficient tool. Use it as your default approach when:
- You need 2+ tool calls, especially with dependencies between them
- Processing/filtering large tool outputs instead of returning them to the conversation
- Running loops over many items (multi-file search, bulk reads/edits)
- Performing computation on tool outputs (counting, sorting, deduplication)
- Search-then-read or search-then-edit patterns
- Filtering webfetch/websearch results

Only call tools directly for simple single-tool calls where you need the full unprocessed output.

All tools return strings, NOT structured Python objects. Parse with split/etc.

Limitations: no imports, no classes, no filesystem/network access, 30s timeout.
