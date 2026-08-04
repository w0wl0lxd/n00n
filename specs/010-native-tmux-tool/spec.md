# Feature Specification: Native tmux tool

**Feature Branch**: `010-native-tmux-tool`

**Created**: 2026-08-03

**Status**: Draft

**Input**: User description: "Add a tmux built-in tool so agents can manage persistent terminal sessions, windows, and panes without ad-hoc bash tmux ... calls and fragile text parsing."

---

## User Scenarios & Testing

### User Story 1 - List and inspect tmux sessions, windows, and panes (Priority: P1)

A user managing long-running terminal workflows wants to list existing tmux sessions, windows, and panes with structured output, so that they can inspect the current state without manual tmux CLI parsing.

**Why this priority**: Listing is the foundational operation for any tmux workflow. Without it, users cannot discover what sessions exist or navigate the tmux hierarchy. This is the minimum viable feature for tmux interaction.

**Independent Test**: Can be fully tested by invoking the tmux tool with list_sessions, list_windows, and list_panes commands on a running tmux server and verifying structured JSON output matches the actual tmux state.

**Acceptance Scenarios**:

1. **Given** a running tmux server with multiple sessions, **When** the user invokes the tmux tool with command `list_sessions`, **Then** the tool returns a structured list of session names, IDs, and metadata.
2. **Given** a specific session, **When** the user invokes the tmux tool with command `list_windows` and a session target, **Then** the tool returns a structured list of window indices, names, and layout information.
3. **Given** a specific window, **When** the user invokes the tmux tool with command `list_panes` and a window target, **Then** the tool returns a structured list of pane indices, PIDs, and current working directories.
4. **Given** no running tmux server, **When** the user invokes any list command, **Then** the tool returns a clear error indicating that tmux is not running or not installed.

---

### User Story 2 - Create and destroy tmux sessions and windows (Priority: P1)

A user wants to create new tmux sessions for isolated workflows and create additional windows within sessions, so that they can organize long-running tasks without manual tmux commands.

**Why this priority**: Session and window creation are core tmux operations. Without them, users cannot set up new workflows or organize work. This is required for basic tmux management.

**Independent Test**: Can be fully tested by invoking new_session and new_window commands and verifying the sessions and windows are created with the specified names and configurations.

**Acceptance Scenarios**:

1. **Given** a running tmux server, **When** the user invokes the tmux tool with command `new_session` and a session name, **Then** the tool creates the session and returns confirmation with the session ID.
2. **Given** a running tmux server, **When** the user invokes the tmux tool with command `new_session` without a name, **Then** the tool creates a session with a default name and returns confirmation.
3. **Given** an existing session, **When** the user invokes the tmux tool with command `new_window` and a session target, **Then** the tool creates a new window in that session and returns the window index.
4. **Given** an existing session, **When** the user invokes the tmux tool with command `kill_session` and a session target, **Then** the tool destroys the session and returns confirmation.
5. **Given** an existing window, **When** the user invokes the tmux tool with command `kill_window` and a window target, **Then** the tool destroys the window and returns confirmation.

---

### User Story 3 - Send keys and capture pane output (Priority: P1)

A user wants to send commands to tmux panes and capture their output, so that they can interact with long-running processes and retrieve results without manual terminal switching.

**Why this priority**: Sending keys and capturing output are the primary interaction mechanisms with tmux panes. Without them, users cannot execute commands or retrieve results from tmux sessions. This is essential for automation workflows.

**Independent Test**: Can be fully tested by invoking send_keys with a command, waiting for execution, then invoking capture_pane and verifying the output contains the expected results.

**Acceptance Scenarios**:

1. **Given** a running tmux session with an active pane, **When** the user invokes the tmux tool with command `send_keys`, a pane target, and keys, **Then** the tool sends the keys to the pane and returns confirmation.
2. **Given** a pane with recent command output, **When** the user invokes the tmux tool with command `capture_pane` and a pane target, **Then** the tool returns the pane contents as structured text.
3. **Given** a pane with large output, **When** the user invokes capture_pane with output_limit options, **Then** the tool returns truncated output respecting the limits.
4. **Given** a pane target that does not exist, **When** the user invokes send_keys or capture_pane, **Then** the tool returns a clear error indicating the invalid target.

---

### User Story 4 - Pane manipulation (resize, break, join) (Priority: P2)

A user wants to resize panes, break panes into new windows, and join panes from other windows, so that they can reorganize their terminal layout dynamically.

**Why this priority**: Pane manipulation is useful for layout customization but is not required for basic tmux workflows. This is a secondary feature that enhances usability.

**Independent Test**: Can be fully tested by invoking resize, break_pane, and join_pane commands and verifying the pane layout changes match the requested operations.

**Acceptance Scenarios**:

1. **Given** a tmux session with multiple panes, **When** the user invokes the tmux tool with command `resize` and dimensions, **Then** the tool resizes the target pane and returns confirmation.
2. **Given** a pane in a window, **When** the user invokes the tmux tool with command `break_pane` and a pane target, **Then** the tool moves the pane to a new window and returns the new window index.
3. **Given** two windows in a session, **When** the user invokes the tmux tool with command `join_pane` with source and destination targets, **Then** the tool moves the source pane to the destination window and returns confirmation.

---

### User Story 5 - Run tmux commands directly (Priority: P3)

A user wants to run arbitrary tmux commands through the tool for advanced operations not covered by the core commands, so that they have full access to tmux capabilities without dropping to bash.

**Why this priority**: Direct command execution provides escape-hatch access to tmux features not explicitly supported. This is a convenience feature for power users and is not required for the MVP.

**Independent Test**: Can be fully tested by invoking run_command with various tmux commands and verifying the output matches direct tmux CLI execution.

**Acceptance Scenarios**:

1. **Given** a running tmux server, **When** the user invokes the tmux tool with command `run_command` and a raw tmux command, **Then** the tool executes the command and returns the output.
2. **Given** an invalid tmux command, **When** the user invokes run_command, **Then** the tool returns the error output from tmux.

---

### Edge Cases

- What happens when tmux is not installed on the system? The tool returns a clear error indicating that tmux is required and not found on PATH.
- What happens when the tmux server is not running? The tool returns a clear error indicating that no tmux server is running and suggesting the user start one with `tmux new-session`.
- What happens when a target selector (session/window/pane) does not exist? The tool returns a clear error indicating the invalid target with the selector value.
- What happens when a command times out? The tool respects the timeout parameter, terminates the tmux process, and returns a timeout error.
- What happens when output exceeds the output limit? The tool truncates the output at max_lines or max_bytes and includes a truncation indicator.
- What happens on Windows or other non-Unix systems? The tool returns a clear error indicating that tmux is Unix-only and not supported on the current platform.
- What happens when permission scopes are denied? The tool returns a permission error indicating which scope was required and denied (tmux.read, tmux.write, or tmux.kill).
- What happens when multiple concurrent tmux operations are invoked? Each operation is independent; the tool does not serialize tmux operations but relies on tmux's own concurrency handling.

---

## Requirements

### Functional Requirements

- **FR-001**: The tmux tool MUST be implemented as a pure Lua plugin at `plugins/tmux/init.lua`.
- **FR-002**: The tmux tool MUST use `n00n.fn.jobstart` and `n00n.fn.jobwait` to invoke the tmux CLI.
- **FR-003**: The tmux tool MUST support the commands: `list_sessions`, `list_windows`, `list_panes`, `new_session`, `kill_session`, `new_window`, `kill_window`, `send_keys`, `capture_pane`, `run_command`, `resize`, `break_pane`, `join_pane`.
- **FR-004**: The tmux tool MUST parse tmux output into structured JSON/tables using tmux's `-F` format strings.
- **FR-005**: The tmux tool MUST support target selectors following tmux syntax: `session_name`, `session_name:window_index`, `session_name:window_index.pane_index`.
- **FR-006**: The tmux tool MUST respect `output_limits` (max_output_lines, max_output_bytes) via `plugins/lib/n00n/output_limits.lua`.
- **FR-007**: The tmux tool MUST support a `timeout` parameter for command execution.
- **FR-008**: The tmux tool MUST define permission scopes: `tmux.read` for list/capture operations, `tmux.write` for send_keys/run_command/resize/break_pane/join_pane, `tmux.kill` for kill_session/kill_window.
- **FR-009**: The tmux tool MUST return a clear error when tmux is not installed or not on PATH.
- **FR-010**: The tmux tool MUST return a clear error when the tmux server is not running.
- **FR-011**: The tmux tool MUST return a clear error when a target selector does not exist.
- **FR-012**: The tmux tool MUST be registered via `n00n.api.register_tool` with schema, handler, permission_scopes, header, and restore functions.
- **FR-013**: The tmux tool MUST be added to `DEFAULT_BUILTINS` in `n00n-config/src/lib.rs`.
- **FR-014**: The tmux tool MUST generate documentation via `n00n-docgen`.
- **FR-015**: The tmux tool MUST handle Unix-only platforms gracefully with a clear error on non-Unix systems.

### Key Entities

- **TmuxSession**: A tmux session with attributes: session_name, session_id, created, last_attached, windows (list).
- **TmuxWindow**: A tmux window with attributes: window_index, window_name, window_id, layout, panes (list).
- **TmuxPane**: A tmux pane with attributes: pane_index, pane_id, pane_pid, pane_current_path, pane_current_command.
- **TmuxTarget**: A selector string following tmux syntax: `session_name[:window_index[.pane_index]]`.
- **TmuxCommand**: One of the supported commands: list_sessions, list_windows, list_panes, new_session, kill_session, new_window, kill_window, send_keys, capture_pane, run_command, resize, break_pane, join_pane.
- **PermissionScope**: One of: tmux.read, tmux.write, tmux.kill.

---

## Success Criteria

### Measurable Outcomes

- **SC-001**: The tmux tool returns correct structured output for list_sessions, list_windows, and list_panes on a running tmux server in 100% of test cases.
- **SC-002**: The tmux tool successfully creates and destroys sessions and windows with the specified names and targets in 100% of test cases.
- **SC-003**: The tmux tool successfully sends keys to panes and captures pane output with the expected content in 100% of test cases.
- **SC-004**: The tmux tool respects output_limits and truncates output at max_lines or max_bytes when exceeded.
- **SC-005**: The tmux tool returns clear, actionable errors for tmux not installed, server not running, and invalid targets.
- **SC-006**: The tmux tool is registered in DEFAULT_BUILTINS and appears in the agent's default tool set.
- **SC-007**: The tmux tool documentation is generated by n00n-docgen and includes all commands, parameters, and permission scopes.
- **SC-008**: `cargo test -p n00n-lua` passes with the tmux plugin tests.
- **SC-009**: The tmux tool does not introduce any unsafe code or unwrap/expect usage in the Lua plugin.

---

## Assumptions

- The tmux CLI is installed on the user's system and available on PATH for Unix-like systems.
- The tmux server is running when list/create/destroy operations are invoked; the tool does not auto-start the server.
- tmux target selectors follow the standard tmux syntax (session_name:window_index.pane_index).
- The tmux CLI version is 3.0 or later, which supports the `-F` format strings used for structured output.
- The tool is Unix-only; Windows support is out of scope for v1.
- The tool does not require tmux control mode (`-C`) for real-time streaming in v1; this can be added later as a Rust helper if needed.
- The tool does not manage tmux server lifecycle (start/stop); users manage the server externally.
- Output limits use the shared `plugins/lib/n00n/output_limits.lua` utilities.
