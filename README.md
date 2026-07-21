# n00n

An AI coding agent built for minimal context-token use and a fast, transparent terminal experience.

n00n is a memory safe, clippy strict rewritten fork of [maki](https://github.com/tontinton/maki) with its own direction: lossless [TOON](https://github.com/w0wl0lxd/tooned) re-encoding of tool-call data

## Why n00n

- Context efficiency first. n00n spends tokens on the work, not on repeating your codebase back to the model.
- Native Rust TUI. No JavaScript runtime. Immediate startup, smooth at 60 FPS, light on memory.
- You stay in control. Token counts, costs, and requested permissions are shown, not buried behind "the model got smarter."

## Features

### Context efficiency

- `index` tool - tree-sitter parses supported languages into a compact skeleton (imports, type defs, function signatures with exact line ranges), so the model reads structure first and only the lines it needs.
- `code_execution` tool - a sandboxed interpreter (monty) that exposes every other tool as an async function. The model filters, summarizes, and pipes data inside the sandbox, so intermediate results never reach your context window. Bounded by time and memory.
- `tooned` - lossless conversion of JSON-shaped tool data to TOON when it is smaller, cutting token usage on structured payloads (API responses, config files, db rows).
- `task` tool - subagents pick a model tier (weak / medium / strong) per job, like haiku / sonnet / opus.
- Concise system prompt, tool descriptions, and examples - tuned to avoid bloating context.
- Optional [rtk](https://github.com/rtk-ai/rtk) integration to compress bash output. Disable with `--no-rtk`.

### User experience

- Fast, native TUI with ratatui. No JS; even the splash animation uses SIMD.
- Neovim-like Lua plugins - bring your own or use the [builtins](https://github.com/w0wl0lxd/n00n/tree/main/plugins). See the [Lua API reference](https://github.com/w0wl0lxd/n00n/docs/lua-api/).
- Nothing hidden - token count, cost, and requested permissions are shown, not buried.
- Sensible permissions - tree-sitter parses bash so `git diff && rm -rf /` is understood as `git *` and `rm *`, not `git *`. Disable with `--yolo`.
- SSRF protection on `webfetch`.
- `memory` tool for long-term context, managed via `/memory`.
- Subagent visibility - each subagent has its own chat window, switch with `/tasks` (Ctrl-X) or Ctrl-N/P.
- Fuzzy search (Ctrl-F), `/btw` to run a command with history, rewind on Escape-Escape, image attachments, 26 themes, session resume.
- Skills & MCPs, plan mode, `!` / `!!` bash, `/cd`.
- `--print --output-format stream-json` for headless use; output is Claude Code-compatible.

## Supported providers

- Anthropic - `ANTHROPIC_API_KEY` only (using OAuth is against TOS). Bedrock supported via `CLAUDE_CODE_USE_BEDROCK=1`.
- OpenAI - `OPENAI_API_KEY` and OAuth via `n00n auth login openai`.
- Google - `GEMINI_API_KEY`.
- Copilot - `GH_COPILOT_TOKEN` or an existing GitHub Copilot sign-in at `~/.config/github-copilot/`.
- Ollama - `OLLAMA_HOST` for local (e.g. `http://localhost:11434`), or `OLLAMA_API_KEY` for cloud.
- llama.cpp - `LLAMA_CPP_HOST` (e.g. `http://localhost:8080`), optionally `LLAMA_CPP_API_KEY`.
- Mistral - `MISTRAL_API_KEY`.
- Z.AI - `ZHIPU_API_KEY`.
- DeepSeek - `DEEPSEEK_API_KEY`.
- OpenRouter - `OPENROUTER_API_KEY`.
- Synthetic - `SYNTHETIC_API_KEY`.

**Dynamic providers** - drop an executable script into `~/.config/n00n/providers/` to add custom providers or proxies. See [docs](https://github.com/w0wl0lxd/n00n/docs/providers/#dynamic-providers) for details.

## Installation

### Living on the edge (main branch)

```sh
cargo install --locked --git https://github.com/w0wl0lxd/n00n.git n00n
```

### With Nix

```sh
nix run github:w0wl0lxd/n00n
```

Or download a pre-built binary from [GitHub Releases](https://github.com/w0wl0lxd/n00n/releases/latest).

## ACP

Run `n00n acp` or configure your ACP supporting editor to use n00n, e.g. in [Zed](https://zed.dev/)'s `settings.json`:

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

## Documentation

More info at the [official docs](https://github.com/w0wl0lxd/n00n/docs).

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for the workflow: pre-commit hooks, Conventional Commits, the PR template, and `changelog.d` fragments.

> DISCLAIMER: a large share of n00n's code was written by n00n itself, guided by humans. It is not as polished as fully hand-crafted code, but it is not slop or vibe-coded either. I think projects should be honest about their use of AI in this era.
