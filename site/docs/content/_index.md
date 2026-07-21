+++
title = "n00n Docs"
sort_by = "weight"
+++

# n00n Docs

n00n is a terminal coding agent written in Rust. Point it at a codebase, pick an LLM provider, and it reads, edits, searches, and runs code for you while keeping token usage low.

This page is a map of the docs. Skim it once, then jump to what you need.

## Start here

New to n00n? Two pages get you going:

- [Quick Start](/docs/quick-start/) installs n00n and connects your first provider. Takes a few minutes.
- [Configuration](/docs/configuration/) covers `init.lua`, the small Lua script where all settings live.

## Everyday use

Answers to the "how do I..." questions once n00n is running:

- [Commands](/docs/commands/): everything behind the `/` palette, from `/model` to `/btw`.
- [Keybindings](/docs/keybindings/): move around the TUI without touching the mouse.
- [Tools](/docs/tools/): the full reference for the 20 built-in tools the agent works with.
- [Permissions](/docs/permissions/): decide what the agent may do on its own and when it must ask you first.

## Connecting things

- [Providers](/docs/providers/): Anthropic, OpenAI, Ollama, and friends, plus the weak, medium, and strong model tiers.
- [MCP](/docs/mcp/): plug in external tool servers over stdio or HTTP.

## Extending and embedding

- [Lua API](/docs/lua-api/): write plugins in Lua with an API that mirrors Neovim.
- [Headless Mode](/docs/headless/): run n00n with `--print` in scripts and CI. Output is Claude Code compatible.
- [ACP](/docs/acp/): drive n00n from your editor, like [Zed](https://zed.dev/), over the Agent Client Protocol.

Something missing or wrong? Open an issue on [GitHub](https://github.com/w0wl0lxd/n00n).
