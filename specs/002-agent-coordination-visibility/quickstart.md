# Quickstart: ALMAS coordination visibility validation

**Feature**: specs/002-agent-coordination-visibility  
**Date**: 2026-07-23  
**Status**: Phase 1 complete

## Overview

This document provides validation commands and a manual smoke test to verify the ALMAS coordination visibility implementation. Run these commands after implementation to ensure the feature works correctly.

## Automated Validation

### 1. Build and lint checks

```bash
# Check workspace compiles
cargo check --all

# Run workspace lints
cargo clippy --all --tests -- -D warnings

# Run workspace tests
cargo nextest run --workspace
```

**Expected output**: All commands exit with code 0. No warnings or errors.

### 2. Lua plugin syntax check

```bash
# Check Lua syntax for new plugins (requires stylua or luac)
stylua --check plugins/blackboard/init.lua
stylua --check plugins/team/waves.lua
stylua --check plugins/team/validation.lua
stylua --check plugins/lib/n00n/live_context.lua
stylua --check plugins/lib/n00n/checkpoint.lua
```

**Expected output**: All files pass syntax check. No errors.

### 3. Integration tests (if implemented)

If integration tests are added for the blackboard, policy, or wave dispatch:

```bash
# Run integration tests for coordination features
cargo nextest run --workspace --bin n00n --test-threads=1
```

**Expected output**: All integration tests pass.

## Manual Smoke Test

### Prerequisites

- n00n is built and installed.
- A test project directory exists (e.g., `/tmp/n00n-test`).
- The test project has a git repository (for project ID resolution).

### Step 1: Start two concurrent team runs

```bash
# Terminal 1
cd /tmp/n00n-test
n00n team --goal "implement a simple function" --mode autonomous --background

# Terminal 2
cd /tmp/n00n-test
n00n team --goal "write tests for the function" --mode autonomous --background
```

**Expected output**: Both commands return an `agent_id` indicating the background session started.

### Step 2: Post to the blackboard

```bash
# In Terminal 1 (or via n00n session)
n00n session.prompt "Use the blackboard tool to post an observation about the current task."
```

**Expected output**: The agent calls the blackboard tool with action "write" and posts an observation. The tool returns a post ID.

### Step 3: Query the blackboard from the other session

```bash
# In Terminal 2
n00n session.prompt "Use the blackboard tool to query for recent observations."
```

**Expected output**: The agent calls the blackboard tool with action "query" and retrieves the observation posted by the first session.

### Step 4: Verify live context

```bash
# In a new terminal
n00n session.prompt "Use the live_context module to list all active sessions."
```

**Expected output**: The live_context module returns a snapshot showing both team sessions with their status, last activity, and active task IDs.

### Step 5: Pause and resume an agent

```bash
# Pause the first agent
n00n agent_control --action pause --agent_id <agent_id_from_step_1>

# Verify status
n00n agent_control --action status --agent_id <agent_id_from_step_1>

# Resume
n00n agent_control --action resume --agent_id <agent_id_from_step_1> --message "continue"
```

**Expected output**: 
- Pause returns `{ paused: true }`.
- Status shows `status: "paused"`.
- Resume returns `{ resumed: true }` and the agent continues execution.

### Step 6: Test policy enforcement

```bash
# Set a policy restricting write tools for background agents
n00n agent_control --action policy --policy.action set --policy.rule '{
  "id": "no-write-background",
  "scope": { "tag": "background" },
  "restricted_tools": ["write", "edit"],
  "paused": false,
  "priority": 10
}'

# Verify the policy is set
n00n agent_control --action policy --policy.action list
```

**Expected output**: 
- Policy set returns `{ policy: { ... } }`.
- Policy list shows the new rule.

### Step 7: Verify checkpoint creation

```bash
# After a team run completes, check for checkpoints
ls -la ~/.local/state/n00n/projects/<project_id>/runs/<run_id>/checkpoints/
```

**Expected output**: Checkpoint JSON files exist for each wave completion and run done.

## Validation Checklist

- [ ] cargo check --all passes
- [ ] cargo clippy --all --tests -- -D warnings passes
- [ ] cargo nextest run --workspace passes
- [ ] Lua files pass syntax check
- [ ] Two concurrent team runs can post to blackboard
- [ ] Blackboard queries return posts from other sessions
- [ ] Live context lists all active sessions
- [ ] Agent pause/resume works correctly
- [ ] Policy enforcement blocks restricted tools
- [ ] Checkpoints are created at lifecycle points

## Troubleshooting

### Blackboard unavailable

If the blackboard tool returns "blackboard unavailable", check that the state directory exists and is writable:

```bash
ls -la ~/.local/state/n00n/projects/<project_id>/blackboard/
```

### Policy not enforced

If policy enforcement is not working, verify that the agent's session metadata includes the tag or session_type that matches the policy scope. Check the policy table:

```bash
cat ~/.local/state/n00n/projects/<project_id>/policies/policy.json
```

### Checkpoints not created

If checkpoints are missing, verify that the run directory exists and that the checkpoint module is integrated into the team/workflow plugins:

```bash
ls -la ~/.local/state/n00n/projects/<project_id>/runs/<run_id>/
```

## Next Steps

After validation passes, the feature is ready for integration testing and user acceptance testing. Refer to the spec.md for detailed acceptance scenarios and success criteria.
