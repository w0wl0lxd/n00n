# Changelog

All notable user-facing changes are documented in this file. Entries are
generated from `changelog.d/` fragments at release time via `just changelog`.
See `changelog.d/README.md` for the fragment convention.




## [0.5.0] - 2026-07-28

### Added

- Added a `devin` provider for Agent Client Protocol access via the devin acp subprocess.
- Added support for overriding the devin ACP CLI command per provider via `base_url` (e.g. `devin2`).
- Add display names and pricing for Devin ACP private models.
- Add `name: Option<String>` field to `ModelInfo` for human-readable model names
- Set `info.name` from Devin's `SessionConfigSelectOption.name` in `list_models`
- Add heuristics for `MODEL_PRIVATE_*` models:
-   - Context window: 200k for Claude 4.5 family, 400k for GPT-5.1 family
-   - Max output tokens: 64k for Claude 4.5, 128k for GPT-5.1
-   - Pricing from Devin docs (e.g., Claude Haiku 4.5: $1/$5 per million tokens)
-   - Mark all `MODEL_PRIVATE_*` as non-free and promo/preview
- Update model picker UI to display discovered names instead of raw IDs
- Update all `ModelInfo` constructors across the workspace to include `name: None`
- Added the `n00n agent` CLI for running one-shot prompts and managing long-lived background agents via Unix sockets (`run`, `message`, `list`, `status`, `pause`, `resume`, `stop`). `n00n agent run` now supports `--goal`, `--team-mode`, `--max-agents`, `--waves`, `--workflow-inputs`, and `--task-description` to drive team, workflow, and task mode directly.
- Plan mode can use explicitly read-only MCP and research tools while blocking code execution, mutating shell commands, and writes outside the active plan.
- Emit CacheHealth events for Anthropic, OpenRouter, Mistral, and Google providers so the TUI cache validity timer works across vendors.
- Wire ThinkingConfig into OpenAI responses body for local and custom providers.
- Complete the remaining `n00n.treesitter` API stubs: `get_node`, `language.add`, and `query.get`, with bundled highlight queries for Lua, Rust, and Python.
- Index plugin now supports Astro, Containerfile, CSS, HCL, JSON, Make, SCSS, Svelte, and Vue files.
- Added `tool_search` and `load_namespace` built-in tools, plus registry support for deferred plugin loading and namespace-based activation, reducing the active prompt toolset by default.Added blackboard plugin for multi-agent coordination, agent control plane with policy enforcement, wave dispatch with validation gates, human escalation support, and checkpointing for run lifecycle management.
- Add ALMAS coordination execution: wave dispatch with validation gates, checkpoint persistence, policy enforcement, and live context enrichment.
- Copilot discovered models now report their own metadata, tier, and supported reasoning effort, which is used when resolving weak, medium, and strong model tiers.
- Add `n00n agent list` and `status` with stable `--json` output, normalized state labels, `--all`, and `--cwd` filtering for scripting.
- `n00n-arbor` crate providing graph-based code analysis via the Arbor CLI (Anandb71/arbor). New `arbor` builtin tool supports callers, callees, map, diff, query, and status commands.
- Built-in `codegraph` tool for cross-file structural exploration, call-path analysis, and blast-radius impact checks using a pre-indexed semantic codegraph (powered by [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph)). Use `codegraph explore <query>` at the CLI or the built-in `codegraph` tool in agent sessions.
- Add gpt-5.3-codex-spark model to OpenAI provider.
- Add Cursor provider backed by `cursor-agent` with streaming NDJSON, session resume, and static model registry.
- Implement proper ACP content-block and tool-call handling in Devin provider.
- Add is_free and is_promo fields to ModelInfo for Devin free/promo markers.
- Add display names and pricing for Devin ACP private models.
- Add `name: Option<String>` field to `ModelInfo` for human-readable model names
- Set `info.name` from Devin's `SessionConfigSelectOption.name` in `list_models`
- Add heuristics for `MODEL_PRIVATE_*` models:
-   - Context window: 200k for Claude 4.5 family, 400k for GPT-5.1 family
-   - Max output tokens: 64k for Claude 4.5, 128k for GPT-5.1
-   - Pricing from Devin docs (e.g., Claude Haiku 4.5: $1/$5 per million tokens)
-   - Mark all `MODEL_PRIVATE_*` as non-free and promo/preview
- Update model picker UI to display discovered names instead of raw IDs
- Update all `ModelInfo` constructors across the workspace to include `name: None`
- Agent control plane: `n00n-daemon`, scoped agent tools, TUI daemon.sock registration, #129 control/steer, and #134 background worker socks under unified `n00n agent` CLI.
- Added ChatGPT/Codex plan usage tracking to the OpenAI provider by fetching rate limits and credits from the ChatGPT backend API.
- Tool-call views now show todo items, telemetry, raw input, output state, and image metadata, with collapsible details for long content.
- Added ALMAS team-control improvements: Sprint Agent role, planner acceptance criteria/effort fields, workflow saga compensations, Summary Agent retrieval, telemetry events, human escalation with resumable runs, and pause/resume actions in agent_control.
- Added support for Claude Code-style PreCompact and PostCompact command hooks in n00n-agent's conversation compaction.
- Session titles now describe their first task, and the session picker groups sub-tasks under their parent session.
- Added clickable inline five-line activity previews for task and team cards, with sanitized tool summaries and explicit full-session navigation through the Ctrl+X picker.
- Made submitted messages appear before startup work and added a safe short Escape window to return unsent text and images for editing.
- Added a `System` cache-block type with `CacheControl` support across providers and the agent, enabling static session context (rules, AGENTS.md, CLAUDE.md, etc.) to participate in provider prompt-caching APIs.
- Token-profile CI gate for cold-start tool schemas and system prompt; fails nextest when committed baselines regress beyond absolute deltas.
- Added privacy-safe task and team telemetry with conserved cache-aware token usage and per-model cost accumulation across supervisors, roles, quorum validators, swarms, compaction, and charged failures.

### Changed

- Bash now requires a brief justification before running broad or unbounded commands, including chained and piped commands, while keeping simple command scopes focused.Removed the hard-coded 24/32 agent-call ceilings in `team` and `workflow`; `max_agents`/`max_agents_per_run` are now user-configurable with no hard maximum. Added a shared `n00n.guard` runaway detector that enforces call budgets, wall-clock timeouts, repeated-prompt loops, and consecutive subagent errors.
- Refactored `plugins/team` to launch subagents through the shared `n00n.subagent` helper, consolidating structured-output and model-tier handling with `task` and `workflow`.
- Split ChatGPT/Codex Coding Plan into a dedicated `codex` provider with OAuth device flow (`n00n auth login codex`), while `openai` keeps full context windows and API-key authentication.
- Resolved remaining strict clippy warnings in n00n-ui and restored the add-strict-lint-rules branch to a green `cargo clippy --all --tests` and `cargo nextest run --workspace` state.Removed remaining `#[allow(clippy::...)]` attributes from `n00n-agent` by extracting helpers and refactoring hot paths.
- Switch library fallback warnings to structured tracing instead of ad-hoc string formatting.
- CI now runs lint and test jobs on macOS and no longer installs ripgrep in Ubuntu test and coverage jobs.
- Improved agent guidance to prefer token-efficient code search and Thoughtbox for non-trivial reasoning.
- Removed additional `#[allow(clippy::...)]` attributes from small crates and refactored `n00n-storage` `append` into helpers.Update SWE-1.7 context window to 262K tokens.
- # Tool preference guidance
- Prompt and instruction guidance now steer agents toward token-efficient, pre-indexed exploration tools. `codegraph` and `arbor` are included in the native efficient-tools list, system/general/research prompts describe the `codegraph`/`arbor`/`index` ladder before `grep`/`read`, and `AGENTS.md` adds a "Token-efficient exploration" section covering `rtk` and `tooned`.

### Fixed

- Fix hang when models emit only reasoning without final text.
- Capture thinking deltas in Devin's `handle_session_update` and append to `thinking` buffer
- Build assistant message with both `Thinking` and `Text` content blocks
- Remove invalid `reasoning_content` field from OpenAI compat message conversion
- Add nudge logic in agent when assistant produces only reasoning without text
- Add system prompt instruction to always end with a user-facing final answer
- Fixed session durability so queued work survives agent respawns and context resets, all active sessions are saved periodically, shutdown allows persistence to drain, and transient storage-write failures retain snapshots for bounded retries.
- Remove the bunny mascot, show activity in the status bar, and make usage costs and cache savings easier to read.
- Bash, task, and workflow tools now show live progress in the UI while they run.
- Restored sessions now recover active conversation context and require approval before replaying full history when an OpenAI continuation is unavailable.
- Fixed file tool correctness: JSON escaping in tool search, UTF-8-aware read truncation, grep error propagation, and glob/fs error handling.
- Halt agent turns on subagent failure, retry transient OpenAI 500 responses only before output is emitted, propagate agent socket and stream errors, improve Windows daemon lock liveness, and fix SSE non-retryability after partial output.
- Fixed file tool correctness: streaming `read_lines` for offset/limit reads, UTF-8-safe truncation, grep quote stripping, and long-line handling in read output.
- Fixed a startup hang on Wayland caused by synchronous clipboard initialization; `n00n` now initializes `arboard` in a background thread and falls back to OSC52 until the native backend is ready.
- Broadened terminal image protocol detection so pasted and model-provided images render in Kitty, Ghostty, WezTerm, iTerm2, Rio, Foot, and more, without a stdin probe.
- Fixed session persistence so long-running chats no longer repeat the full transcript on every save or exhaust memory while reopening oversized legacy logs.
- Resolved clippy warnings, test compilation errors, and progress notification coalescing after merging the open PR backlog.Updated subagent test expectations to match the no-answer-fallback and steering-unavailable wording changes introduced by merged subagent PRs.Restored the done tool for task subagents when no output_schema is set, so plain tasks can signal completion with a final answer.
- Fixed generated commands documentation missing its TOML frontmatter closing delimiter.
- Resolved n00n compile and test regressions after merging upstream maki changes.Fixed wave retries accumulating feedback across attempts, blackboard query sorting order, and validation gate false positives.
- Fixed policy enforcement in agent_control to reuse n00n.policy.evaluate_policy
- Fixed wave execution to track failures and return paused state correctly
- Fixed checkpoint validation to prevent path traversal and control characters
- Fixed blackboard ID validation and list_claims boolean validation
- Fixed live_context.snapshot to validate ctx and bound blackboard queries
- Fixed validation gate to accept PASS, PASS., PASS:, and PASSED
- Fixed Windows-only dead-code lints in the truecolor compatibility module and regenerated the Lua API docs for `max_args`.
- Agent control messages now steer active sessions with a dedicated control role, and paused team runs resume from the failed step with validated team metadata and preserved run state.
- Fixed recursive chat compactions and truncation actions so restored sessions retain nested history and expandable tool output without panics or stale rows.
- Fixed pre-commit formatting and repository spell checks so valid staged Rust and Lua changes are no longer rejected by temporary-file artifacts or intentional test fixtures.
- Fixed Codex task-agent disconnects and token amplification by reusing Responses WebSockets safely, preventing post-send request replays, capping Coding Plan context correctly, and bounding orchestration fan-out.
- Allow custom Devin providers to use CLI credentials when no API key is configured.
- Correct Devin free-preview heuristic so only standard SWE-1.7 variants are marked free, not the paid Lightning tier.Fixed CodeGraph and Arbor cards so previews update atomically and expand consistently without viewport jitter.
- Preserved full-resolution images by default and added bounded lossless tiling and cropping for oversized screenshots.
- Fixed Lua plugin compatibility with mlua 0.12 while preserving existing plugin behavior.
- Nix packages now embed runtime library paths so copied `n00n` binaries run without wrapper-provided environment variables.
- Fix Nix binary wrapping on macOS by using `DYLD_LIBRARY_PATH` instead of `LD_LIBRARY_PATH`, and scope wrapping to the computed package binary path.
- Fixed OpenAI Responses disconnect handling so sent requests are never replayed, stale WebSockets are replaced before writes, and ambiguous stream failures retain safe delivery details.
- Fixed OpenAI session continuity by serializing OAuth refreshes across processes, preserving valid credentials on refresh failures, keeping ephemeral subagent chains in memory, and replacing stale Responses WebSockets before sending.
- Use the current 272K served context window consistently for OpenAI coding-plan and Codex models.
- Pass Devin API keys through ACP initialize/authenticate metadata and harden the terminal-bench-2.1 harness for local providers.
- Propagate rejected agent control responses as errors and use real Windows PID liveness in daemon lock checks.
- Fixed bash tool RTK auto-rewrite coverage: `cargo` commands with `--`, `cargo nextest run`, `head -n N`, and read-only `git` subcommands (`remote`, `config`, `tag`, etc.) are now routed through `rtk` when possible. Updated prompts and AGENTS.md to stop claiming `jq`/`yq` are rewritten.
- Fixed strict workspace lint failures while preserving the established `n00nId` public API.
- Fixed team wave validation resolving model tiers as literal model IDs, which caused repeated validation failures and exhausted agent-call budgets.
- Fix hang when models emit only reasoning without final text.
- Add `thinking: Arc<AsyncMutex<String>>` field to Devin's `DevinInner`
- Capture thinking deltas in `handle_session_update` and append to `thinking` buffer
- Build assistant message with both `Thinking` and `Text` content blocks
- Remove invalid `reasoning_content` field from OpenAI compat message conversion
- Add nudge logic in agent when assistant produces only reasoning without text
- Add system prompt instruction to always end with a user-facing final answer
- Use `cortexkit-tree-sitter-scss` so the SCSS grammar compiles on Windows with MSVC.

### Security

- Hardened background agent state management by validating `agent_id` against path traversal and setting state directory and Unix socket permissions to owner-only.

### Performance

- Improved session startup by making session-header scanning faster and cwd index handling more robust.
- Improved tool registry lookups with a HashMap-backed snapshot instead of linear scans.
- Reduced token use by trimming tool JSON schemas and result payloads.
- Improved OpenAI prompt cache routing and Google explicit cache reuse, billing, and token accounting.
- Reduced per-turn token overhead by compressing system/subagent prompts, tightening default output limits, and improving `dynamic_tool_size` observability. Prompt templates are smaller, tool-output line/byte defaults are lower, and the token-measurement binary now reports per-tool and per-prompt costs.Use model-aware tiktoken vocabularies (cl100k/o200k) for context-size estimation, choosing o200k for GPT-4o/GPT-4.1/GPT-5/o-series models. Adjust Anthropic cache breakpoints by conversation length so short sessions pay for fewer cache writes and long sessions cache more prefixes.
- Reduced the `skill` tool definition size by moving skill enumeration behind a `list` parameter, and added tiktoken-based token accounting for messages and tool definitions to the agent context window.

## [Unreleased]
