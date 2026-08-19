# Contract: RTK Bash Rewriting

**Tool**: `bash`  
**Plugin**: `plugins/bash/init.lua`

## Purpose

This is not a tool contract but a configuration and prompt contract for RTK bash rewriting. RTK (Rewrite Tool Kit) compresses verbose shell output to reduce token usage.

## Configuration

### Availability Detection

- RTK availability is checked via `rtk --version` with a 10s timeout.
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
- Commands outside the managed-command set

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
2. Call `rtk rewrite <command>` with a 10s timeout.
3. If rewrite succeeds (exit code 0 or 3), use the rewritten command.
4. Exit code 1 means no specialized rewrite is available. Use an allowlisted RTK fallback where one is safe.
5. Reject a managed command when it cannot be safely rewritten or proxied.
6. Execute unmanaged commands unchanged.

### When RTK is Unavailable

1. Run the original command unchanged.
2. No error is raised.

### Session Caching

- RTK availability is checked once per session.
- The result is stored in a session-local variable.
- Subsequent bash calls use the cached value.

## Error Handling

- If the RTK binary is absent, run the original command unchanged.
- If the availability check fails or times out, reject managed commands and leave unmanaged commands unchanged.
- If rewriting fails or times out, reject managed commands unless an allowlisted fallback is available.
- Treat only rewrite exit code 1 as "no specialized rewrite". Other unexpected failures are not safe fallback signals.

## Notes

This contract documents the existing RTK integration in the bash plugin and the hardening improvements planned in this spec.
