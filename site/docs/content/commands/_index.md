+++
title = "Commands"
weight = 5
[extra]
group = "Reference"
+++

# Commands

Type `/` in the input box to open the command palette.

## Built-in commands

n00n has 24 built-in commands. Project, user, and MCP prompt commands are separate.

### Session

| Command | Legacy aliases | Description |
|---------|----------------|-------------|
| `/session:new` | `/new` | Start a new session |
| `/session:list` | `/sessions` | Browse and switch sessions |
| `/session:rename` | `/rename` | Rename the current session |
| `/session:fork` | `/fork` | Fork the current session |

### Model

| Command | Legacy aliases | Description |
|---------|----------------|-------------|
| `/model:pick` | `/model` | Switch model |

### View

| Command | Legacy aliases | Description |
|---------|----------------|-------------|
| `/view:tasks` | `/tasks` | View running and completed work |
| `/view:usage` | `/usage` | View token usage |
| `/view:memory` | `/memory` | View and edit persistent notes |

### Settings

| Command | Legacy aliases | Description |
|---------|----------------|-------------|
| `/settings:theme` | `/theme` | Switch color theme |
| `/settings:mcp` | `/mcp` | Configure MCP servers |
| `/settings:login` | `/login` | Authenticate with a provider |

### Mode

| Command | Legacy aliases | Description |
|---------|----------------|-------------|
| `/mode:no-confirm` | `/yolo` | Toggle permission confirmations |
| `/mode:fast` | `/fast` | Toggle fast mode when supported |
| `/mode:workflow` | `/workflow` | Toggle workflow mode |
| `/mode:thinking` | `/thinking` | Set thinking level |

### Action

| Command | Legacy aliases | Description |
|---------|----------------|-------------|
| `/action:compact` | `/compact`, `/session:compact` | Compact conversation history |
| `/action:queue` | `/queue` | Manage queued prompts |
| `/action:cd` | `/cd` | Change working directory |
| `/action:ask` | `/btw` | Ask a quick question without tools |
| `/action:help` | `/help` | Show context-aware help |
| `/action:reload` | `/reload`, `/session:reload` | Reload plugins and configuration |
| `/action:exit` | `/exit`, `/session:exit` | Exit n00n |
| `/welcome` | — | Show the welcome guide |
| `/team` | — | Configure and run an agent team for a goal |

## Sessions

Sessions run concurrently. `/session:new` starts a fresh session while the old one keeps working in the background, and `/session:list` shows the live status of each (Working, Needs input, Idle) so you can jump between them. When a background session finishes or needs input, n00n flashes a note in the status bar.

## Custom commands

You can define your own slash commands as Markdown files.

### Project commands

Place `.md` files in `.n00n/commands/` in your project root.
They appear in the palette as `/project:<filename>`.

### User commands

Place `.md` files in `~/.config/n00n/commands/`.
They appear in the palette as `/user:<filename>`.

Project commands override user commands with the same name.

`.claude/commands/` directories are also supported for compatibility.

### Metadata

You can add optional metadata at the top of the file between `---` lines to set `name`, `description`, and `argument-hint`:

```markdown
---
description: Review code for issues
argument-hint: <file>
---
Review $ARGUMENTS and suggest improvements.
```

### Arguments

Use `$ARGUMENTS` in the command body. It gets replaced with whatever you type after the command name.

For example, `/project:review main.rs` replaces `$ARGUMENTS` with `main.rs`.