# Implementation Tasks: Native tmux tool

## Milestone 1: Core plugin structure and P1 commands (list, create, destroy)

- [ ] Create `plugins/tmux/` directory
- [ ] Create `plugins/tmux/init.lua` with basic plugin structure
- [ ] Implement tool registration with `n00n.api.register_tool`
- [ ] Define tool schema with command parameter and all supported commands
- [ ] Implement command dispatch table mapping commands to handlers
- [ ] Implement `list_sessions` command handler
  - [ ] Build tmux CLI arguments: `list-sessions -F "#{session_name}|#{session_id}|#{created}|#{last_attached}"`
  - [ ] Invoke via `n00n.fn.jobstart` and `n00n.fn.jobwait`
  - [ ] Parse pipe-delimited output into structured table
  - [ ] Return structured result
- [ ] Implement `list_windows` command handler
  - [ ] Build tmux CLI arguments with session target and format string
  - [ ] Invoke via jobstart/jobwait
  - [ ] Parse output into structured table
  - [ ] Return structured result
- [ ] Implement `list_panes` command handler
  - [ ] Build tmux CLI arguments with window target and format string
  - [ ] Invoke via jobstart/jobwait
  - [ ] Parse output into structured table
  - [ ] Return structured result
- [ ] Implement `new_session` command handler
  - [ ] Build tmux CLI arguments with optional session name
  - [ ] Invoke via jobstart/jobwait
  - [ ] Return confirmation with session ID
- [ ] Implement `kill_session` command handler
  - [ ] Build tmux CLI arguments with session target
  - [ ] Invoke via jobstart/jobwait
  - [ ] Return confirmation
- [ ] Implement `new_window` command handler
  - [ ] Build tmux CLI arguments with session target
  - [ ] Invoke via jobstart/jobwait
  - [ ] Return confirmation with window index
- [ ] Implement `kill_window` command handler
  - [ ] Build tmux CLI arguments with window target
  - [ ] Invoke via jobstart/jobwait
  - [ ] Return confirmation
- [ ] Add basic error handling
  - [ ] Check if tmux is installed (run `tmux -V` and check exit code)
  - [ ] Check if tmux server is running (run `tmux list-sessions` and check exit code)
  - [ ] Return clear error messages for both cases
- [ ] Add "tmux" to `DEFAULT_BUILTINS` in `n00n-config/src/lib.rs`
- [ ] Verify tool appears in agent's default tool set

## Milestone 2: P1 interaction commands (send_keys, capture_pane)

- [ ] Implement `send_keys` command handler
  - [ ] Build tmux CLI arguments with pane target and keys
  - [ ] Invoke via jobstart/jobwait
  - [ ] Return confirmation
- [ ] Implement `capture_pane` command handler
  - [ ] Build tmux CLI arguments with pane target and `-p` flag
  - [ ] Invoke via jobstart/jobwait
  - [ ] Return pane contents as structured text
- [ ] Integrate `output_limits` from `plugins/lib/n00n/output_limits.lua`
  - [ ] Add max_output_lines and max_output_bytes parameters to schema
  - [ ] Call `output_limits.resolve(opts, ctx)` in handlers
  - [ ] Truncate output at limits and include truncation indicator
- [ ] Add timeout parameter support
  - [ ] Add timeout parameter to schema
  - [ ] Pass timeout to `n00n.fn.jobwait`
  - [ ] Handle timeout errors and return clear message
- [ ] Add target validation error handling
  - [ ] Parse tmux error output for invalid targets
  - [ ] Return clear error with the invalid target value

## Milestone 3: P2 pane manipulation (resize, break_pane, join_pane)

- [ ] Implement `resize` command handler
  - [ ] Build tmux CLI arguments with pane target and dimensions
  - [ ] Invoke via jobstart/jobwait
  - [ ] Return confirmation
- [ ] Implement `break_pane` command handler
  - [ ] Build tmux CLI arguments with pane target
  - [ ] Invoke via jobstart/jobwait
  - [ ] Return confirmation with new window index
- [ ] Implement `join_pane` command handler
  - [ ] Build tmux CLI arguments with source and destination targets
  - [ ] Invoke via jobstart/jobwait
  - [ ] Return confirmation

## Milestone 4: P3 escape hatch (run_command) and polish

- [ ] Implement `run_command` command handler
  - [ ] Build tmux CLI arguments with raw command string
  - [ ] Invoke via jobstart/jobwait
  - [ ] Return output or error
- [ ] Implement permission scopes function
  - [ ] Define scopes for each command (tmux.read, tmux.write, tmux.kill)
  - [ ] Return scopes based on command type
  - [ ] Set force_prompt appropriately for kill operations
- [ ] Add platform check for Unix-only support
  - [ ] Detect if running on non-Unix system
  - [ ] Return clear error indicating Unix-only support
- [ ] Refine error messages
  - [ ] Ensure all error messages are clear and actionable
  - [ ] Include suggestions for remediation where applicable
- [ ] Generate documentation via n00n-docgen
  - [ ] Run `just gen-docs` or equivalent
  - [ ] Verify documentation is generated correctly
  - [ ] Check that all commands and parameters are documented

## Testing

- [ ] Write unit tests for command argument building
- [ ] Write unit tests for output parsing
- [ ] Write unit tests for error handling
- [ ] Write unit tests for permission scopes function
- [ ] Write integration tests against real tmux server
  - [ ] Test list commands with multiple sessions/windows/panes
  - [ ] Test create/destroy commands
  - [ ] Test send_keys and capture_pane workflow
  - [ ] Test output limits truncation
  - [ ] Test timeout handling
- [ ] Run `cargo test -p n00n-lua` and verify tests pass
- [ ] Manual verification of tool in agent

## Code Quality

- [ ] Run `cargo fmt --all`
- [ ] Run `cargo check --all`
- [ ] Run `cargo clippy --all --tests -- -D warnings`
- [ ] Follow n00n code style conventions
- [ ] No unsafe code, unwrap, expect, or todo! macros
- [ ] Proper error handling with Result types
- [ ] Descriptive variable and function names

## Documentation

- [ ] Update tool description to position it as a first-tier tool
- [ ] Document all commands and parameters
- [ ] Document permission scopes
- [ ] Document error cases and remediation steps
- [ ] Document Unix-only requirement

## Final Verification

- [ ] All acceptance criteria from spec.md are met
- [ ] All success criteria from plan.md are met
- [ ] No AI attribution in code or commits
- [ ] Clean worktree with only related changes
