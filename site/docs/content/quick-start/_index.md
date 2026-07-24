+++
title = "Quick Start"
weight = 1
[extra]
group = "Getting Started"
+++

# Quick Start

Install n00n, connect a provider, and run your first session. Takes a few minutes.

## Install

### Linux / macOS

```sh
# Download and read the script first (don't blindly trust shell scripts).
curl -fsSL https://maki.sh/install.sh -o install.sh
cat install.sh

# Then run.
chmod +x install.sh && sh install.sh
```

One-liner:

```sh
curl -fsSL https://maki.sh/install.sh | sh
```

### Windows (PowerShell)

```powershell
# Download and read the script first (don't blindly trust remote scripts).
irm https://maki.sh/install.ps1 -OutFile install.ps1
Get-Content install.ps1

# Then run.
.\install.ps1
```

One-liner:

```powershell
irm https://maki.sh/install.ps1 | iex
```

### Windows (Git Bash)

```sh
curl -fsSL https://maki.sh/install.sh | sh
```

Both install to `%LOCALAPPDATA%\maki` and add it to your user PATH. Override with `MAKI_INSTALL_DIR` / `$env:MAKI_INSTALL_DIR`.

> **Note for PowerShell users:** The bash tool requires Git for Windows or WSL.
> The installer will prompt you to install Git for Windows via `winget` if bash is
> not found. Or install it yourself: `winget install --id Git.Git -e --source winget`.

### Living on the edge (main branch)

```sh
cargo install --locked --git https://github.com/w0wl0lxd/n00n.git n00n
```

### With Nix

```sh
nix run github:w0wl0lxd/n00n
```

Or download a pre-built binary from [GitHub Releases](https://github.com/w0wl0lxd/n00n/releases/latest).

## API Keys

Export a key for at least one provider (e.g. `ANTHROPIC_API_KEY`). Some providers support OAuth login instead via `n00n auth login <provider>`.

See [Providers](/docs/providers/) for the full list of supported providers, environment variables, and setup instructions.

## Run

From your project directory:

```bash
n00n
```

Type a prompt, press **Enter**, and the agent starts working.

## Keybindings

These are the defaults. Plugins and `init.lua` can rebind most of them with `n00n.keymap.set`; see [Keybindings](../keybindings/) for precedence and caveats.

- **Newline in input**: \\+Enter, Ctrl+J, or Alt+Enter
- **Scroll output**: Ctrl+U / Ctrl+D (half page)
- **Cancel streaming**: Esc Esc
- **Rewind (when idle)**: Esc Esc
- **Insert file path**: type `@` after whitespace to open the file picker (Esc leaves a literal `@`)
- **Quit**: Ctrl+C
- **All keybindings**: Ctrl+H

## Choosing a Model

Set a default in your config:

```lua
-- ~/.config/n00n/init.lua
n00n.setup({
    provider = {
        default_model = "anthropic/claude-sonnet-4-20250514",
    },
})
```

Switch models mid-session with the `/model` command.

## Project Configuration

Add a `.n00n/` directory to your project root for per-project settings:

```
.n00n/
├── init.lua           # Overrides global config
├── permissions.toml   # Permission rules
├── mcp.toml           # MCP server config
└── commands/          # Custom slash commands (.md files)
AGENTS.md              # Loaded into agent context automatically
AGENTS.local.md        # Personal per-project instructions (gitignored)
```

n00n also recognizes `CLAUDE.md`, `COPILOT.md`, `.cursorrules`, `CONVENTIONS.md`, `GEMINI.md`, and others as instruction files (first match wins).

`AGENTS.md` is loaded at the start of every session. Put coding conventions, repo quirks & gotchas, or off-limits directories in here. n00n will automatically load instruction files inside subdirs when doing a `read` in the subdir.

See [Configuration](/docs/configuration/) for all options.
