# Contract: RTK Bash Rewriting

**Tool**: `bash`  
**Plugin**: `plugins/bash/init.lua`

## Purpose

This is not a tool contract but a configuration and prompt contract for RTK bash rewriting. RTK (Rewrite Tool Kit) compresses verbose shell output to reduce token usage.

## Configuration

### Availability Detection

- RTK availability is checked via `rtk --version` with a 2s timeout.
- Availability is cached per session to avoid repeated spawns.
- Config option `no_rtk` in CLI/config disables RTK rewriting.

### Command Rewriting

The following commands are rewritten through `rtk` when available:
- git (status, diff, log, branch, remote, etc.)
- cargo (test, build, check, clippy, etc.)
- rg, grep
- find, ls, cat, head, tail
- gh (GitHub CLI)
- Other commands supported by rtk

The following commands pass through unchanged:
- jq
- yq
- Commands with unsupported flags (e.g., find with -exec, -delete)

## Prompt Contract

### Tool Usage Hint

The bash plugin registers the following prompt hint:

```
- Reserve `bash` for system CLI (git, cargo, rg, grep, gh, find, ls, builds, tests). Auto-rewrites via `rtk` when installed. Do NOT use `bash` for file modifications.
```

### Efficient Tools

The agent's `NATIVE_EFFICIENT_TOOLS` list should recommend rtk-wrapped bash for verbose shell commands.

## Behavior

### When RTK is Available

1. Normalize the command (e.g., `head -n N` to `head -N`).
2. Call `rtk rewrite <command>` with a 2s timeout.
3. If rewrite succeeds (exit code 0 or 3), use the rewritten command.
4. If rewrite fails for unsupported commands, fall back to `rtk git` for git subcommands or run unchanged.
5. Execute the rewritten or original command.

### When RTK is Unavailable

1. Run the original command unchanged.
2. No error is raised.

### Session Caching

- RTK availability is checked once per session.
- The result is stored in a session-local variable.
- Subsequent bash calls use the cached value.

## Error Handling

- If rtk version check times out, assume rtk is unavailable.
- If rtk rewrite times out, run the original command unchanged.
- If rtk rewrite returns an unexpected error, run the original command unchanged.

## Notes

This contract documents the existing RTK integration in the bash plugin and the hardening improvements planned in this spec.
