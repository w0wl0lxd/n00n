# n00n

> n00n is an opinionated experimental fork of [maki](https://github.com/tontinton/maki): currently testing lossless [TOON](https://github.com/w0wl0lxd/tooned)
re-encoding of tool-call data, and [ALMAS](https://arxiv.org/abs/2510.03463) agent orchestration, and a strict clippy re-write

## Quick start

```sh
cargo install --locked --git https://github.com/w0wl0lxd/n00n.git n00n

cd your-project && n00n
```

With Nix: `nix run github:w0wl0lxd/n00n`  
Or grab a binary from [releases](https://github.com/w0wl0lxd/n00n/releases/latest).

Type a prompt and press **Enter** — the agent reads, edits, searches, and runs code. n00n also has inline images! (for supported terminals)

<img width="937" height="987" alt="image" src="https://github.com/user-attachments/assets/5c33e266-ebee-44a4-8a89-5ffd25a3b7ab" />

## Why n00n

**Context efficiency first.** n00n spends tokens on the work, not on repeating your codebase back
to the model. The `index` tool uses tree-sitter to produce a compact skeleton (imports, type defs,
function signatures with exact line ranges) — the model reads structure first and only the lines it
needs. 

**Native Rust TUI.** No JavaScript runtime, no Electron, no 500 MB install. Immediate startup,
smooth at 60 FPS, light on memory. Even the splash animation uses SIMD.

**You stay in control.** Token counts, costs, and requested permissions are shown, not buried behind
"the model got smarter."

## Features

### Context efficiency

- **`index`** — tree-sitter parses supported languages into a compact skeleton, so the model reads
  structure first and only the lines it needs. Saves ~165 tok/turn on reads.
- **`code_execution`** — sandboxed interpreter (monty) that exposes every other tool as an async
  function. The model filters, summarizes, and pipes data inside the sandbox; intermediate results
  never reach your context window. Bounded by time and memory.
- **`tooned`** — lossless conversion of JSON-shaped tool data to
  [TOON](https://github.com/w0wl0lxd/tooned) when it is smaller, cutting token usage on structured
  payloads (API responses, config files, DB rows).
- **`toon-lsp`** — interact with TOON-compatible data at a symbol level via
  [toon-lsp](https://github.com/w0wl0lxd/toon-lsp).
- **`task`** — subagents pick a model tier (weak / medium / strong) per job, like haiku / sonnet / opus.
- **`team`** — [ALMAS](https://arxiv.org/abs/2510.03463)-based team workflow for sub-agent loop engineering.
- Concise system prompt, tool descriptions, and examples — tuned to avoid bloating context.
- Optional [rtk](https://github.com/rtk-ai/rtk) integration to compress bash output (~50% savings).
  Disable with `--no-rtk`.

### User experience

- Fast, native TUI with [ratatui](https://ratatui.rs). No JS; even the splash animation uses SIMD.
- Neovim-like Lua plugins — bring your own or use the
  [builtins](https://github.com/w0wl0lxd/n00n/tree/main/plugins). See the
  [Lua API reference](https://github.com/w0wl0lxd/n00n/docs/lua-api/).
- Nothing hidden — token count, cost, and requested permissions are shown, not buried.
- Sensible permissions — when the agent runs `git diff && rm -rf /`, tree-sitter parses the bash
  and understands the real commands are `git *` and `rm *`, not a single `git *`. Disable with `--yolo`.
- SSRF protection on `webfetch` calls.
- `memory` tool for long-term context, managed via `/memory`.
- Subagent visibility — each subagent gets its own chat window, switch with `/tasks` (Ctrl-X) or
  Ctrl-N/P.
- Fuzzy search (Ctrl-F), `/btw` to run a command with chat history, rewind on Escape-Escape, image
  attachments, 26 themes, session resume.
- Skills & MCPs, plan mode, `!` / `!!` bash, `/cd`.
- `--print --output-format stream-json` for headless use; output is Claude Code-compatible.

## Supported providers

| Provider | Auth | Notes |
|---|---|---|
| Anthropic | `ANTHROPIC_API_KEY` | OAuth is against TOS. Bedrock via `CLAUDE_CODE_USE_BEDROCK=1` |
| OpenAI | `OPENAI_API_KEY` or `n00n auth login openai` | OAuth supported |
| Google | `GEMINI_API_KEY` | |
| Copilot | `GH_COPILOT_TOKEN` or `~/.config/github-copilot/` | Existing GitHub Copilot sign-in |
| Ollama | `OLLAMA_HOST` | Local: `http://localhost:11434`. Cloud: `OLLAMA_API_KEY` |
| llama.cpp | `LLAMA_CPP_HOST` | e.g. `http://localhost:8080`. Optional `LLAMA_CPP_API_KEY` |
| Mistral | `MISTRAL_API_KEY` | |
| Z.AI | `ZHIPU_API_KEY` | |
| DeepSeek | `DEEPSEEK_API_KEY` | |
| OpenRouter | `OPENROUTER_API_KEY` | |
| Synthetic | `SYNTHETIC_API_KEY` | |

**Dynamic providers** — drop an executable script into `~/.config/n00n/providers/` to add custom
providers or proxies. See [docs](https://github.com/w0wl0lxd/n00n/docs/providers/) for details.

## Project configuration

n00n reads per-project settings from `.n00n/` in your project root:

```
.n00n/
├── init.lua           # Overrides global config
├── permissions.toml   # Permission rules
├── mcp.toml           # MCP server config
└── commands/          # Custom slash commands (.md files)
AGENTS.md              # Loaded into agent context automatically
AGENTS.local.md        # Personal per-project instructions (gitignored)
```

n00n also recognizes `CLAUDE.md`, `COPILOT.md`, `.cursorrules`, `CONVENTIONS.md`, `GEMINI.md`,
and other instruction files (first match wins). Instruction files inside subdirectories are loaded
automatically when reading in that subdir.

## ACP (editor integration)

Run `n00n acp` or configure your ACP-supporting editor:

<details>
<summary>Zed settings.json</summary>

```json
"agent_servers": {
  "n00n": {
    "default_config_options": {
      "model": "deepseek/deepseek-v4-flash"
    },
    "type": "custom",
    "command": "n00n",
    "args": ["acp"],
    "env": {}
  }
}
```
</details>

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for the workflow: pre-commit hooks, Conventional Commits,
the PR template, and `changelog.d` fragments.

> DISCLAIMER: a large share of n00n's code was written by n00n itself, guided by humans. It is not
> as polished as fully hand-crafted code, but it is not slop or vibe-coded either. I think projects
> should be honest about their use of AI in this era.
