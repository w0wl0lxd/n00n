+++
title = "Commands"
weight = 5
[extra]
group = "Reference"
+++

# Commands

Type `/` in the input box to open the command palette.

## Built-in commands

n00n has 23 built-in commands. Project, user, and MCP prompt commands are separate.

### Session

| Command | Description |
|---------|-------------|
| `/session:new` | Start a new session |
| `/session:compact` | Compact conversation history |
| `/session:exit` | Exit n00n |
| `/session:reload` | Reload plugins and configuration |
| `/rename` | Rename the current session |
| `/sessions` | Browse and switch sessions |

### Model

| Command | Description |
|---------|-------------|
| `/model:pick` | Switch model |

### View

| Command | Description |
|---------|-------------|
| `/view:tasks` | View running and completed work |
| `/view:usage` | View token usage |
| `/memory` | View, edit, and delete memory files |

### Settings

| Command | Description |
|---------|-------------|
| `/settings:theme` | Switch color theme |
| `/settings:mcp` | Configure MCP servers |
| `/settings:login` | Authenticate with a provider |

### Mode

| Command | Description |
|---------|-------------|
| `/mode:no-confirm` | Toggle permission confirmations |
| `/mode:fast` | Toggle fast mode when supported |
| `/mode:workflow` | Toggle workflow mode |
| `/mode:thinking` | Set thinking level |

### Action

| Command | Description |
|---------|-------------|
| `/action:queue` | Manage queued prompts |
| `/action:cd` | Change working directory |
| `/action:ask` | Ask a quick question without tools |
| `/action:help` | Show context-aware help |
| `/welcome` | Show the welcome guide |
| `/team` | Configure and run an agent team for a goal |

## Sessions

Sessions run concurrently. `/new` starts a fresh session while the old one keeps working in the background, and `/sessions` shows the live status of each (working, needs input, idle) so you can jump between them. When a background session finishes or needs input, n00n flashes a note in the status bar.

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