# Implementation Plan: Native tmux tool

## Problem

Agents currently need to use ad-hoc `bash` calls with manual tmux CLI parsing to manage persistent terminal sessions, windows, and panes. This approach is fragile, error-prone, and lacks structured outputs. There is no native tmux tool in n00n, forcing users to drop to bash for tmux operations.

## Goals

1. Provide a native tmux tool that agents can use to manage tmux sessions, windows, and panes without bash.
2. Return structured outputs (JSON/tables) for tmux operations instead of raw text.
3. Support safe scoped operations with permission scopes (tmux.read, tmux.write, tmux.kill).
4. Follow the existing n00n plugin pattern (pure Lua plugin using `n00n.fn.jobstart`).
5. Handle error cases gracefully (tmux not installed, server not running, invalid targets).

## Architecture

### Component Structure

```
plugins/tmux/
├── init.lua              # Main plugin with tool registration and command handlers
└── tests/                # Integration tests (optional, if feasible)

n00n-config/src/lib.rs    # Add "tmux" to DEFAULT_BUILTINS
```

### Data Flow

1. **Tool Registration**: Plugin loads → `n00n.api.register_tool` → `ToolRegistry::register` with `ToolSource::Lua { plugin: "tmux" }`
2. **Command Execution**: Tool handler → `n00n.fn.jobstart("tmux ...")` → `n00n.fn.jobwait(id, timeout)` → parse stdout
3. **Permission Check**: Permission scopes function → returns `{ scopes = { "tmux.read" | "tmux.write" | "tmux.kill" }, force_prompt = false/true }`
4. **Output Limits**: `output_limits.resolve(opts, ctx)` → truncates at max_lines/max_bytes
5. **Error Handling**: Check tmux availability, server status, target validity → return structured errors

### Command Dispatch Pattern

Follow the pattern from `plugins/arbor/init.lua`:
- Command dispatch table mapping command names to handler functions
- Each handler builds the tmux CLI arguments, invokes via jobstart/jobwait, parses output
- Shared utility functions for error checking, output parsing, and target validation

### Output Parsing

Use tmux's `-F` format strings for structured output:
- `list-sessions -F "#{session_name}|#{session_id}|#{created}|#{last_attached}"`
- `list-windows -F "#{window_index}|#{window_name}|#{window_id}|#{layout}"`
- `list-panes -F "#{pane_index}|#{pane_id}|#{pane_pid}|#{pane_current_path}|#{pane_current_command}"`
- Parse pipe-delimited output into Lua tables/JSON

### Permission Scopes

- `tmux.read`: list_sessions, list_windows, list_panes, capture_pane
- `tmux.write`: send_keys, run_command, resize, break_pane, join_pane
- `tmux.kill`: kill_session, kill_window

## Milestones

### Milestone 1: Core plugin structure and P1 commands (list, create, destroy)
- Create `plugins/tmux/init.lua` with tool registration
- Implement list_sessions, list_windows, list_panes
- Implement new_session, kill_session, new_window, kill_window
- Add basic error handling (tmux not installed, server not running)
- Add to DEFAULT_BUILTINS

### Milestone 2: P1 interaction commands (send_keys, capture_pane)
- Implement send_keys command
- Implement capture_pane command
- Add output_limits integration
- Add timeout parameter support

### Milestone 3: P2 pane manipulation (resize, break_pane, join_pane)
- Implement resize command
- Implement break_pane command
- Implement join_pane command

### Milestone 4: P3 escape hatch (run_command) and polish
- Implement run_command command
- Add comprehensive error messages
- Add permission scopes function
- Generate documentation via n00n-docgen
- Write tests

## Testing Strategy

### Unit Tests (Lua)
- Test command argument building for each command
- Test output parsing for each command
- Test error handling (tmux not found, server not running, invalid targets)
- Test permission scopes function

### Integration Tests
- Test against a real tmux server in a controlled environment
- Test end-to-end workflows (create session, send keys, capture output, destroy session)
- Test output limits truncation
- Test timeout handling

### Manual Verification
- Verify tool appears in agent's default tool set
- Verify documentation is generated correctly
- Verify permission prompts work as expected

### Test Location
- Follow the pattern from `n00n-lua/tests/real_plugins_restore.rs` if possible
- Alternatively, add tests in `plugins/tmux/tests/` if the test framework supports plugin tests

## Dependencies

### Internal
- `n00n-lua/src/api/tool.rs` - `register_tool` function
- `n00n-lua/src/api/fn.rs` - `jobstart`/`jobwait` functions
- `plugins/lib/n00n/output_limits.lua` - Output limit utilities
- `n00n-config/src/lib.rs` - DEFAULT_BUILTINS list
- `n00n-docgen` - Documentation generation

### External
- tmux CLI (version 3.0 or later) - must be installed on the user's system
- Unix-like operating system (tmux is Unix-only)

## Risks

### Risk 1: tmux not installed or server not running
- **Mitigation**: Clear error messages indicating the issue and suggesting remediation (install tmux, start server with `tmux new-session`)

### Risk 2: Target selector parsing complexity
- **Mitigation**: Follow tmux's standard syntax exactly; pass selectors directly to tmux CLI and let tmux handle validation; return tmux's error output if invalid

### Risk 3: Output parsing fragility
- **Mitigation**: Use tmux's `-F` format strings with pipe delimiters; handle missing fields gracefully; validate output structure before returning

### Risk 4: Cross-platform support
- **Mitigation**: Explicitly document Unix-only support; return clear error on non-Unix systems; Windows support is out of scope for v1

### Risk 5: Concurrent operations
- **Mitigation**: Rely on tmux's own concurrency handling; do not attempt to serialize operations in the plugin

### Risk 6: Test flakiness due to tmux server state
- **Mitigation**: Use a dedicated test tmux server or mock tmux CLI for unit tests; integration tests should set up and tear down a clean tmux environment

## Acceptance Criteria

1. The tmux tool is registered and appears in the agent's default tool set.
2. All P1 commands (list_sessions, list_windows, list_panes, new_session, kill_session, new_window, kill_window, send_keys, capture_pane) work correctly against a real tmux server.
3. All P2 commands (resize, break_pane, join_pane) work correctly.
4. The run_command escape hatch works correctly.
5. Error handling covers: tmux not installed, server not running, invalid targets, timeouts.
6. Output limits are respected and truncation is indicated.
7. Permission scopes are defined and enforced correctly.
8. Documentation is generated via n00n-docgen.
9. Tests pass for the tmux plugin.
10. The tool follows the existing n00n plugin pattern and code style conventions.
