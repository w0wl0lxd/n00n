Mirror Neovim's Lua API namespaces (n00n.uv = vim.uv, n00n.fs = vim.fs, n00n.treesitter = vim.treesitter).
Keep function signatures identical so plugins can be copy-pasted between Neovim and n00n.
Only exception is the UI API, neovim's has baggage.

## Design

Our goal is to let plugin authors have as much freedom as possible, so API design should focus on simple primitives that can be combined.

## Error convention

Fallible runtime operations return the pair (value, err) and never throw.
Throwing is reserved for programmer errors, like passing a number where a string belongs.

Tool handlers fail with `{ llm_output = msg, is_error = true }`; a plain string is always success (only `is_error` flags the result as an error to the provider).
