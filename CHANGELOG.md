# Changelog

All notable user-facing changes are documented in this file. Entries are
generated from `changelog.d/` fragments at release time via `just changelog`.
See `changelog.d/README.md` for the fragment convention.


## [0.6.2] - 2026-08-20

### Added

- Added unit tests for `PluginHost::set_search_config` in `n00n-lua`.

### Fixed

- Fix post-0.6.1 review comments: remove heading markers inside changelog list items and deduplicate repeated block, handle CRLF and bullet prefixes correctly in build-changelog.sh, and replace em-dash in provider docs per tone rules.
- Prevent the built-in GitHub tool from crashing when a pull request or issue has a missing or non-string body.
- Return an empty Git history when callers request zero commits.
- Prevented large streamed `code_execution` output from panicking when the interpreter retains a shorter, UTF-8-truncated stdout buffer. Missing provider credentials are now treated as expected setup state instead of repeated warning-level failures.
- Keep the TUI responsive while tools and question windows are active by avoiding synchronous plugin-state capture from live session saves.
- Fix flaky `pre_execution_callback_timeout_starts_after_dispatch` test on Windows by increasing timeouts to 5s/2s.
- Fix Lua API docs build with Zola 0.23: wrap generated markdown in Tera raw block to prevent `{#` and `{{` from being parsed as template syntax, which broke `zola build` on CI after the 0.23 upgrade.
- Fix tool token analysis script to handle current zstd-compressed JSONL session format at `~/.local/state/n00n/sessions/*.jsonl` in addition to legacy `~/.n00n/sessions/*.json`.

### Performance

- Optimized string interpolation overhead in `unified_text` diff rendering loop.

### Docs

- Added unit test coverage for `git::diff` in `n00n-git`.
- Added unit test coverage for `branches` function in `n00n-git`.
- Added unit tests for git blame operation in `n00n-git`.

## [0.6.1] - 2026-08-19

### Added

- Add native GitHub commands for creating pull requests and posting issue comments.
- Lua plugins can now own background jobs. `n00n.fn.jobstart` accepts `owner = "plugin"`, which keeps a job running after the call that started it returns, until the plugin unloads or reloads. Task-owned jobs keep their old lifetime, only the owning task or plugin can stop or wait on a job, and a timed-out `jobwait` no longer strands the job's remaining events.
- The usage modal now shows price metrics per model. Costs are attributed to the model that actually ran the turn, so a compaction pass billed to a different model is no longer reported under the main model. Fast-tier rates and their cache multipliers are resolved in one place, which keeps the cost and cache-savings figures consistent with each other.
- Added capability gates for Codex prompt-cache options so unsupported OpenAI-compatible endpoints do not receive cache-only request fields.
- Add n00n-smell crate with persistent code-smell scanner (TODO/FIXME/HACK comments, placeholder phrases) and Lua API.
- Add native git and GitHub tooling via new n00n-git crate (gix-based) and Lua plugins.
- n00n-git crate: git status, log, diff, blame, add, commit, checkout, conflicts subcommands
- plugins/git: Lua plugin wrapping n00n-git binary with permission-scoped operations
- plugins/github: Lua plugin for GitHub REST API (issues, PRs, repo metadata, comments)
- Token sources: GITHUB_TOKEN env var, optional token parameter, or gh CLI fallback
- Rate limit detection with retry-after header parsing
- Permission scopes: git.read, git.write, github.read, github.write
- Add native web extraction core to n00n-search crate with URL policy validation, DNS pinning, and bounded fetching.
- Documented OpenCode Go (`opencode/opencode-go/<model_id>`), a separate flat-subscription tier and models.dev catalog entry from OpenCode Zen with its own base URL (`https://opencode.ai/zen/go/v1`) and curated model set, reached through the same `OPENCODE_API_KEY`. Added live, key-gated test coverage for the Opencode provider covering Zen model discovery, a streamed completion, a tool call, and a Go completion; these skip cleanly (reported as ignored, not passed) when no key is configured, so CI stays honest about what it has and has not exercised.
- Per-model file input support flags: static model tables carry a `files` flag (regular OpenAI GPT-5.6 model entries support files; coding-plan/Codex entries remain disabled with `files` set to `false`), discovery can report file support, and overrides still take precedence.
- Skill system v2: recursive discovery, frontmatter policy, agent enforcement, search ranking, and structured plans.
- Memory tool v2: keyword search, YAML frontmatter metadata, append, and lite-layer session recall hints.
- Added native Cursor `AgentService/Run` spike (HTTP/2 Connect via reqwest duplex streaming) with IDE auth, discovery, checkpoints, checksum headers, and a live Auto/`default` pong path gated by `N00N_CURSOR_LIVE_TESTS=1`.
- Added lead-owned beta Fusion orchestration (`--fusion` / `always_fusion` / `[agent.fusion]`): a lead agent plans, delegates execution to a conservative sidekick via `fusion_delegate`, and reviews the result. Fusion remains off by default, delegation is lead-directed, and per-lane cost stats are reported on done events.
- Added first-tier explore tooling with `explore` intent routing, expanded `arbor`/`codegraph`/`semblem` commands, native/CLI fallbacks, and `rtk`/`bash` hardening.
- Added a native tmux tool for managing sessions, windows, and panes.
- Added host-owned, root- and session-scoped Lua plugin state with bounded snapshots, lifecycle restoration, and cleanup.
- Sessions now restore plugin state and todo lists across restarts in UI, SDK, ACP, print, and agent modes.
- Added secure local Firecrawl backends for web search and fetch, with bounded output and clear provenance.
- Add in-memory Arbor `graph.json` indexing in `n00n-arbor` with symbol/file/id lookup, caller/callee queries, and shortest-path tracing for explore-tool integration.
- Refresh Arbor graph indexes when status reports stale state and validate graph.json shape (including node holes) before building in-memory indexes.Route Arbor callers/callees/trace_path through the in-memory graph.json index when available, falling back to Arbor CLI subprocess calls for map/diff/query/status.Add qualified-name/file/kind symbol resolution, calls-only edge traversal, and richer graph node metadata for Arbor graph queries.Added `parallel_tool_calls` support to OpenAI-compatible, OpenAI Responses, and Codex WebSocket request bodies. Models that support it can now emit multiple tool calls in a single turn, which n00n executes in parallel and returns in one follow-up, reducing round-trips and repeated input-token costs. Provider support is controlled by a new `supports_parallel_tool_calls` flag in `n00n-providers` TOML configs.Add `n00n-codegraph` with a native `n00n.codegraph` Lua API and refactor the codegraph plugin to use it for explore calls with timeout handling.Add `rusqlite` with bundled SQLite and query `.codegraph/codegraph.db` in-process for explore lookups, with CLI fallback when the database is missing or unreadable.
- Enable the unified `explore` built-in by default, update agent prompts and AGENTS.md guidance, and add a `just explore-health` recipe for local index checks.
- Add explore plugin coverage to shared restore-card integration tests and regenerate tool docs after explore integration.
- Add a unified `explore` tool that routes queries to `index`, `arbor`, or `codegraph` based on intent, with optional per-session result caching.
- Add native Devin provider implementation using Connect protocol over gRPC-Web.
- The new implementation:
- Reads credentials from `~/.local/share/devin/credentials.toml` or `WINDSURF_API_KEY`/`DEVIN_API_KEY` env
- Implements `GetUserJwt` exchange to obtain user JWT
- Implements `GetChatMessageRequest` encoder and `GetChatMessageResponse` decoder using hand-rolled protobuf
- Implements streaming response parser for Connect frames with gzip decompression
- Emits ProviderEvents: `TextDelta`, `ThinkingDelta`, `ToolUseStart`/`ToolUseDelta`/`ToolUseEnd`, `Done`/`Error`
- Supports tool definitions in requests and tool-call streaming in responses
- Adds a full Devin model catalog from the live CLI model list
- Resolves display model ids to wire uids via `GetCliModelConfigs`
- Allows provider-only model specs like `n00n -m devin` by selecting the provider default
- Replaces the ACP-based `devin` provider with a native HTTP implementation that calls Devin's gRPC-Web API directly.
- Add OpenAI Chat Completions message-level prompt cache breakpoints for gpt-5.6+ models.
- Migrate supported non-Codex OpenAI models to Responses API with safe Chat Completions fallback. API-key requests are intentionally stateless: they use `store: false` and send full history on every turn, so provider-side response storage is not enabled.
- Extended non-Codex OpenAI Responses API with July 2026 model features: explicit prompt-cache options and breakpoints, reasoning mode and context, service-tier fast, safety identifiers, moderation, built-in tool conversion, and output item parsing for gpt-5.5/5.6.
- Added native `semblem` built-in with `n00n-search` BM25 indexing and `n00n-semble` Lua bindings.
- Added per-model `thinking_dialect`, `thinking_fields`, and `body_override` config for dynamic providers (script `models`/`info`) and custom providers (`providers.toml`), letting a model declare where thinking values go in the request body and shape the body with `defaults`/`replace`/`filter` after the provider's own setup.
- Show prompt-cache hit percentages in the status bar and token usage panel for every provider that reports cached token usage.
- Validate model identifiers against the configured model catalog instead of parsing any `provider/model` string, so an unconfigured or unavailable model is now rejected with a clear error when choosing or loading a model. Restored sessions keep working: the saved model resolves through the catalog first and falls back to the static tables when the provider is currently unconfigured, preserving session continuity. Tools also gain canonical names (`read` is now `read_file`, `bash` is now `run_shell`, and similar); the previous names keep working as deprecated aliases and are normalized in permission rules.

### Changed

- Reduced tool-output update overhead under load and capped batch fan-out at four calls.
- Bundle git and smell commands into the n00n executable, route built-in agent tools through their in-process libraries, and ship installations and release archives with one binary. Git add and ordinary commits now use gix 0.86 in process with index locking, protected-path validation, file/directory collision handling, filesystem-mode compatibility, and conservative rejection of unsupported index, signing, and hook states; checkout retains the system Git compatibility path for safe porcelain semantics.
- Raised the `memory` plugin's directory cap from 50 KB to 1 MB, and the limit error now names the largest entries so you know what to delete.
- **mise:** Relaxed zola version pin from 0.19.2 to latest.
- n00n now builds on the pinned nightly-2026-08-14 Rust toolchain instead of stable. CI toolchain steps were switched to nightly, the MSRV job was removed since the pinned-nightly policy has no minimum supported version, and the `rust-version` field was dropped from all workspace manifests accordingly.
- Kept single-object tool arguments visible inline, truncated tool-name labels by display width, and unified secret redaction across the UI, activity descriptions, and logs.
- Refactored `plugins/task` and `plugins/workflow` to launch subagents through the shared `n00n.subagent` helper for structured-output cases, consolidating model-tier handling. Team submodules (validation, quorum, summary) retain direct `n00n.agent.session` calls for test compatibility with mock contexts.
- Demote expected runtime log messages to reduce noise from tool parameters, glob walks, MCP tool descriptions, provider credentials, and cancelled streams.Demote OpenAI response-chain fallback and provider retry logs to `info`.Log `HistoryReplayRequired` and cancelled agent states at appropriate levels and reduce provider manifest noise.Group child sessions by research, planning, review, and orchestration role in the session picker.

### Fixed

- Fixed discovered nested model specs being rejected when selected and kept bursty shell output responsive without spawning timer processes.
- Fixed the "Auto-compacting..." card staying stuck in the transcript forever when auto-compaction failed; it now shows the failure reason and the session continues normally.
- Fixed the bash tool's rtk rewrite resolving the wrong git subcommand when global options like `--no-optional-locks` precede it, fixed the unbounded-command guardrail rejecting already-bounded commands such as `git log --oneline -20` and `rg --max-depth 1`, made rejection messages name the bounding flag that would have worked, let a compound segment rtk has no rewrite for run unchanged instead of failing the whole command, and capped the streamed command-output accumulator so a runaway process cannot grow it without bound.
- Expanded default RTK enforcement to every supported command family and made simple managed commands fall back through `rtk proxy` when no specialized rewrite exists, while still rejecting nested, obfuscated, or policy-violating bypass attempts.
- Fixed `batch` tool calls under a near-exhausted subagent deadline getting killed mid-flight by the watchdog interrupt; children now settle with a clear "insufficient time remaining" error instead.
- Box the WebSocketAttemptError payload so provider request failures carry a smaller Result error variant and the workspace can drop the result_large_err clippy allow.
- Fixed the `codegraph` backend: the project path is now passed with `--path` instead of as an extra positional, so `node`, `query`, `callers`, `callees`, `impact` and `files` no longer fail with "too many arguments" and `explore` no longer folds the path into the search query. Graph queries now read the `edges.source` and `edges.target` columns the index actually uses, and `explore` falls back to keyword search instead of erroring when its chosen backend is unavailable.
- Codebase tasks now prioritize the schema-visible `explore_code`, `index_file`, `map_codegraph`, and `search_text` tools instead of falling back to broad reads and shell searches.
- Recover automatically when the OpenAI Codex provider rejects a request because its response chain was not found, instead of leaving the session stuck on repeated `previous_response_not_found` errors.
- Send `store: false` on OpenAI Codex WebSocket requests. The Coding Plan endpoint rejects `store: true` with a 400 "Store must be set to false" error, so continuation requests now also disable server-side response storage.
- Persist plugin state at exact transcript compaction revisions so resumed and rewound sessions restore matching state.
- Replaced the diff preview's unbounded `math.huge` line cap with a large finite one, avoiding a `NaN` from modulo-by-infinity and unbounded buffer growth on a pathological diff, while still never truncating a real diff.
- `grep`'s "not supported" hint now also calls out backreferences (`\1`, `\k<name>`), not just look-around, matching the other common PCRE habit that trips up Rust regex.
- Update `h2` to fix unbounded empty DATA frame buffering.
- Prevented large streamed `code_execution` output from panicking when the interpreter retains a shorter, UTF-8-truncated stdout buffer. Missing provider credentials are now treated as expected setup state instead of repeated warning-level failures.
- Streamed complete interpreter stdout lines immediately, including lines produced by bulk writes, without replaying output.
- Fixed the `kill_job_terminates_long_running_child` test, which still called the
- pre-ownership `JobStore` API and broke the build on `main`. Plugin-owned jobs
- added an `owner` argument to `start` and `task_id`/`plugin` arguments to `kill`
- and `take_receiver`; the restored kill-coverage test was written against the old
- signatures, so the two changes merged cleanly as text but did not compile
- together.
- Fix the JobStore kill unit test to use the current start/kill/take_receiver call signatures.
- Restore the job-kill unit test dropped during the cargo wrapper fix, so shell job termination is covered again.
- Fixed MCP servers being silently disabled when their name matched a built-in tool (e.g. a server named `codegraph`); published tool names are already namespaced per server, so the collision was never real.
- fix(agent): make MCP transport shutdown non-blocking
- Deduplicated consecutive identical stderr lines re-logged from an MCP child process, so a child stuck retrying (e.g. connection refused) no longer floods the log one line per retry.
- Fixed Nix builds after the Monty dependency upgrade and refreshed the flake inputs.
- OpenAI response chains now reset when the system prompt changes, preventing a continuation from reusing stale instructions.
- Fixed deferred Fusion availability, plugin-state persistence around saves and rewinds, provider history conversion, bundled tool limits, and RTK fallback sanitization.
- Generate client-side idempotency keys for provider POST requests. When a request fails after being sent (RequestSent error), retry with the same key so the provider can deduplicate. Makes transport failures retryable without duplicate output or charges.
- Made Anthropic, Bedrock, Google, and OpenAI-compatible provider streams (covering OpenRouter, Mistral, DeepSeek, Zai, Copilot, custom, and other OpenAI-compatible providers) return a retryable error instead of silently returning a partial response when the stream ends before its terminator (`[DONE]`, `message_stop`, a Gemini `finishReason`, or an OpenAI-compatible `finish_reason`), hardened Bedrock eventstream frame-length parsing against malformed lengths, and fixed a Devin stream-read failure being misclassified as non-retryable.
- Redacted tool arguments in provider and agent logs so API keys and other credentials no longer appear in cleartext log output.
- Redacted secret-shaped values (JWTs, provider tokens, AWS access key ids, Bearer headers) in log output even when the key name is not a known secret key, and demoted routine warning/error log noise to info level so real warnings stay visible.
- Harden Cursor Run paced-body heartbeat timing assertion against CI load flakes.
- Ensure the arbor graph index is fresh before native queries and return nil for empty native result tables so the CLI fallback is used.
- Fixed local provider discovery to be silent when unconfigured, updated Devin model pricing, and made custom providers inherit model metadata from their base protocol manifest.
- Fixed session resume and plan context handling, reduced log noise from missing compaction hooks, unconfigured providers, and MCP name conflicts, and made auto-compaction continue on provider errors instead of stopping the session.
- Fixed CodeRabbit review findings from merged PRs #199 and #200: shared StoredMode mapping, propagated auto-compaction cancellation and plan-read failures, initialized UI plan_path from restored sessions, and made swallowed Lua/storage diagnostics visible.
- Also fixed subagent mode selection (`n00n.agent.session` now accepts `mode`) and enabled deferred tools (`batch`, `agent_control`, `view_image`) by default so recent features are not silently disabled.
- Fixed Arbor map and diff failures when results contain numeric fields.
- Stopped raw tool-call JSON from obscuring each tool's purpose-built header and output rendering.
- Retry OpenAI `server_is_overloaded` capacity errors instead of suppressing them, and make `RequestSent` overloads retryable so the agent loop and `team`/`workflow` budgets do not give up on transient provider-side failures.
- Refund agent-call budget slots for transient provider/transport errors in the shared `n00n.guard` runaway detector, so `team` and `workflow` runs do not exhaust their budgets on temporary outages.
- Raise the default `team` agent-call budget from 16 to 24, matching the `workflow` default and giving wave validation retries enough headroom.Fixed expanded tool arguments staying collapsed, broadened secret argument redaction, and kept one-line argument previews expandable.
- Tolerate corrupt `ToolTelemetry` and `StoredFusionUsage` fields when loading sessions.Improve built-in tool schemas to reduce model-side parse failures.
- Fixed model fallback so the last selected model is used by default, and only falls back to recent/auto-detected providers when that model is unavailable. Codex is now shown as a saved provider in the login picker when its OAuth tokens are present.Fixed a plugin host test to surface session tool-definition errors as tool errors.
- Allow null and empty object as empty tool list in n00n.agent.session.
- ACP server: validate request IDs and handle invalid UTF-8 on stdin. Plugin permissions: default to deny instead of allow. TUI: check for terminal before starting interactive UI.
- Fixed missing `Tier` import in `n00n-config` tests and regenerated docs.
- Removed unused `MockProvider` helper to satisfy clippy.
- Fix log-audit-session-write-amplification dirty follow-up.
- Fix review-correctness dirty follow-up.
- Fix session-task-cursor-crashes dirty follow-up.
- Addressed subagent delivery policy dirty follow-up in cleanup pass.
- Bumped the generated tool-definition byte budget to account for new built-in tools.
- Hardened agent session recovery, OpenAI response continuation, UI-only deletion, and lineage handling to prevent lost progress and runaway subagent loops.Harden session reads and process shutdown against signal and crash conditions.
- Restored multi-JSON-per-line parsing for ACP stdin input.
- Build commands run through configured shell wrappers instead of bypassing them through rtk.
- Bounded model-emitted tool calls so excess work queues instead of starting every command at once. Cheap reads use a wider lane, while process-backed tools share an eight-call limit and nested agents share a four-call limit.
- Finish side-question streams when providers complete without emitting a terminal event.
- Prevent cancellation and timeout interrupts from aborting batch, task, team, and workflow cleanup.
- Removed the bare `gpt-5.6` alias from the Codex model catalog; it is a model family, not a selectable model.Prevent automatic replay of accepted Codex requests, honor full-history replay protection, and default enabled Fusion sessions to a Sol lead with Luna Max coding delegates.
- Fix auto-compaction to handle `server_is_overloaded` errors, pre-truncate history to fit within the model's context window before streaming, and raise the default PreCompact hook timeout to 60s.
- Ignore non-protobuf end-stream frames in the Cursor provider instead of aborting the turn.
- Stop duplicating a failed tool's full output below its collapsed snapshot; only show the error if its last line is not already visible in the snapshot tail.
- Register `swe-1-7-max` and `swe-1-7-medium` as the canonical Devin model ids with `swe-1-7` and dot-prefixed variants (`swe-1.7`/`swe-1.7-max`/`swe-1.7-medium`) as aliases, and correct the `swe-1-7` family context window to `262_144` tokens.
- Validate the `devin`/`devin2` `base_url` before using it for authentication, falling back to the configured API server when it is missing or is not an `http://`/`https://` URL. This fixes `failed to build auth request: invalid format` errors when a provider name like `devin2` is configured.
- Map Devin gRPC `ModelUsageStats` to `TokenUsage` correctly, treating `input_tokens` as the total prompt and the cache fields as additive details. Invalid cache breakdowns are ignored instead of changing the meaning of `input_tokens` based on counter magnitudes.
- Report the full resumed conversation size in the post-compaction `TurnComplete` event using the active model tokenizer, continuation prompt, and tool definitions, so the context meter no longer drops sharply after compaction.
- Sync the discovered model context window from the model slot to every session, fixing multi-session discovery that could leave the UI using the default 128k window.
- Fix Devin token tracking by preserving the reported `ModelUsageStats.input_tokens` value while keeping `cache_read_tokens` and `cache_write_tokens` in their own categories.
- Demote expected `glob` permission-denied walk errors from `warn` to `debug` in `n00n.fs.glob`, reducing log noise when scanning directories that include unreadable container or system paths.
- Keep task progress timers and recent actions updating while isolated agents run, and prevent child cancellation or concurrent response state from aborting unrelated work.
- Fix n00n-search walk test path assertion to be cross-platform on Windows.n00n now restores saved models only when their provider is available and auto-detects from available built-in, custom, and script providers without treating `providers.toml` as a global provider allowlist.
- Hardened team and workflow timeouts, resumable orchestration state, native graph fallbacks, daemon session-mode restoration, and Codegraph source reads against malformed, missing, stale, and untrusted state.
- Fixed review findings from the native explore tools stack: explore router now preserves symbol case, uses injective cache keys, routes file-extension queries and "what does X call" correctly, and disables caching by default to avoid stale post-edit results. Codegraph no longer deadlocks on large output, and agent control read loops now propagate I/O errors and EOF distinctly.
- Re-disabled message-cache breakpoints for OpenAI Codex, preventing `Unsupported parameter: prompt_cache_options` errors on the Coding Plan endpoint.
- Fixed handling of empty tool arguments in the Devin provider, which had caused `invalid_argument` stream failures.
- Fail closed instead of silently running raw commands when enabled RTK rewriting cannot safely rewrite an RTK-managed bash command.
- Report storage writer shutdown failures instead of silently accepting unpersisted session snapshots.
- Kept tool arguments searchable when their preview is collapsed, broadened secret-key redaction, and capped argument preview lines by bytes like other tool output.
- Prevent the TUI from exiting when a selected provider cannot start. It now keeps the selected model, opens login, detects Codex CLI authentication, and shows the provider error in the UI.
- Don't convert retryable WebSocket API errors (including `server_is_overloaded` and 5xx `server_error`) into `RequestSent` before any output is emitted, so they are retried with the correct user-facing message instead of "not retrying".
- Workflow script errors now point to the workflow source, and the tool guidance clarifies how to map Lua input tables.
- Added a `devin` provider for Agent Client Protocol access via the devin acp subprocess.
- Added support for overriding the devin ACP CLI command per provider via `base_url` (e.g. `devin2`).
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
- Bash now requires a brief justification before running broad or unbounded commands, including chained and piped commands, while keeping simple command scopes focused.Removed the hard-coded 24/32 agent-call ceilings in `team` and `workflow`; `max_agents`/`max_agents_per_run` are now user-configurable with no hard maximum. Added a shared `n00n.guard` runaway detector that enforces call budgets, wall-clock timeouts, repeated-prompt loops, and consecutive subagent errors.
- Refactored `plugins/team` to launch subagents through the shared `n00n.subagent` helper, consolidating structured-output and model-tier handling with `task` and `workflow`.
- Split ChatGPT/Codex Coding Plan into a dedicated `codex` provider with OAuth device flow (`n00n auth login codex`), while `openai` keeps full context windows and API-key authentication.
- Resolved remaining strict clippy warnings in n00n-ui and restored the add-strict-lint-rules branch to a green `cargo clippy --all --tests` and `cargo nextest run --workspace` state.Removed remaining `#[allow(clippy::...)]` attributes from `n00n-agent` by extracting helpers and refactoring hot paths.
- Switch library fallback warnings to structured tracing instead of ad-hoc string formatting.
- CI now runs lint and test jobs on macOS and no longer installs ripgrep in Ubuntu test and coverage jobs.
- Improved agent guidance to prefer token-efficient code search and Thoughtbox for non-trivial reasoning.
- Removed additional `#[allow(clippy::...)]` attributes from small crates and refactored `n00n-storage` `append` into helpers.Update SWE-1.7 context window to 262K tokens.
- Tool preference guidance: prompt and instruction guidance now steer agents toward token-efficient, pre-indexed exploration tools. `codegraph` and `arbor` are included in the native efficient-tools list, system/general/research prompts describe the `codegraph`/`arbor`/`index` ladder before `grep`/`read`, and `AGENTS.md` adds a "Token-efficient exploration" section covering `rtk` and `tooned`.
- Fix hang when models emit only reasoning without final text.
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
- Fixed OpenAI Responses disconnect handling so sent requests require replay approval, stale WebSockets are replaced before writes, and ambiguous stream failures retain safe delivery details.
- Fixed OpenAI session continuity by serializing OAuth refreshes across processes, preserving valid credentials on refresh failures, keeping ephemeral subagent chains in memory, and replacing stale Responses WebSockets before sending.
- Use the current 272K served context window consistently for OpenAI coding-plan and Codex models.
- Pass Devin API keys through ACP initialize/authenticate metadata and harden the terminal-bench-2.1 harness for local providers.
- Propagate rejected agent control responses as errors and use real Windows PID liveness in daemon lock checks.
- Fixed bash tool RTK auto-rewrite coverage: `cargo` commands with `--`, `cargo nextest run`, `head -n N`, and read-only `git` subcommands (`remote`, `config`, `tag`, etc.) are now routed through `rtk` when possible. Updated prompts and AGENTS.md to stop claiming `jq`/`yq` are rewritten.
- Fixed strict workspace lint failures while preserving the established `n00nId` public API.
- Fixed team wave validation resolving model tiers as literal model IDs, which caused repeated validation failures and exhausted agent-call budgets.
- Use `cortexkit-tree-sitter-scss` so the SCSS grammar compiles on Windows with MSVC.
- Fixed `task`, `todo_write`, and `workflow` session restore crashing outright when the shared preview renderer hits an edge an older snapshot doesn't defend against; restore now falls back to a plain view instead of failing the whole callback.
- Stopped `grep`, `glob`, and MCP tool-loading from flooding the log with a warning per unreadable file, vanished path, or truncated description; expected conditions now log once as a summary instead of one line each. `PostCompact` hook timeouts now report the same actionable timeout hint as `PreCompact`.
- Add heuristic secret/PII detection to edit and write tools. Require justification when content may contain secrets (api keys, tokens, passwords, authorization headers) or PII (email addresses, phone numbers, SSNs). Prevents accidental secret exfiltration via file operations.
- Stop long sessions from freezing the UI and growing without bound: live sessions now cap how many tool outputs and subagent histories they keep in memory, saves coalesce instead of running once per tool completion, and the full history stays recoverable from the session log.
- Fixed the site docs build failing because the build script still copied the removed `demo.cast` file.
- Fixed the site docs build failing on the missing `install.ps1`, which was removed from the repository along with the other install scripts.
- Fixed the site docs build on zola 0.23, which removed the `concat` filter used by the sidebar template.
- Fixed the site docs build failing on recent zola versions, which dropped the `enabled` field from `[markdown.highlighting]`.
- Question prompts and long-running tools no longer block the UI during periodic saves or shutdown. Interactive prompts use a separate bounded execution lane, busy agent scopes no longer reserve slots needed by other sessions, and shutdown settles active tool cancellation before saving plugin state. Ambiguous provider replays remain explicitly approval-gated even when no output reached the client. Lua subprocesses now use bounded output buffering, lower CPU priority, process-group cancellation, and Linux memory limits without repeatedly scanning the full process table, while excessive `async.run` fanout is rejected before it can grow an unbounded task backlog. Tool setup callbacks and subagent event barriers now time out safely instead of leaving question prompts or task agents pending forever.
- Periodic saves now avoid active input prompts, highlight workers keep the UI awake until bounded result queues drain, saturated Lua job queues yield without adding polling delay, and queued setup callbacks start their timeout only after dispatch.
- Fixed n00n panicking when the TUI starts without an attached terminal (headless, piped, or no TTY); it now exits with a clear "needs an interactive terminal" error instead.
- Fixed workflow scripts failing with an unreadable `attempt to index function with 'concurrency'` when stages were passed to `pipeline` one per argument. `pipeline` now accepts both `pipeline(items, stages, opts)` and `pipeline(items, stage1, stage2, ..., opts?)`, and `parallel` and `pipeline` both reject a non-table options argument with an error naming the argument.

### Removed

- Removed the Arbor backend (the `arbor` tool, its Lua bindings, and the `n00n-arbor` crate). `explore` now routes caller/callee lookups and free-text graph queries to `codegraph` instead; project-map, blast-radius diff, and index-status commands had no `codegraph` equivalent and are gone.

### Security

- Hardened background agent state management by validating `agent_id` against path traversal and setting state directory and Unix socket permissions to owner-only.

### Performance

- Cancelled stale CI runs on new pushes and cached the CodeQL Rust build against the shared workspace cache.
- The cargo-deny security gate now fails the build on violations instead of passing silently, and two justfile recipes make local verification and toolchain bumps safer.
- The embedded Python sandbox now runs on monty v0.0.21, resolving the upstream dependency conflict that blocked the pyo3 security-advisory fix, and removing the dump/load clock-reset workaround since the new interpreter pauses the time budget while awaiting tool calls.
- Use OpenAI hosted tool search for deferred built-in and MCP tools on supported models, preserving the prompt cache while keeping local discovery as a fallback for other providers and endpoints.
- Improved session startup by making session-header scanning faster and cwd index handling more robust.
- Improved tool registry lookups with a HashMap-backed snapshot instead of linear scans.
- Reduced token use by trimming tool JSON schemas and result payloads.
- Improved OpenAI prompt cache routing and Google explicit cache reuse, billing, and token accounting.
- Reduced per-turn token overhead by compressing system/subagent prompts, tightening default output limits, and improving `dynamic_tool_size` observability. Prompt templates are smaller, tool-output line/byte defaults are lower, and the token-measurement binary now reports per-tool and per-prompt costs.Use model-aware tiktoken vocabularies (cl100k/o200k) for context-size estimation, choosing o200k for GPT-4o/GPT-4.1/GPT-5/o-series models. Adjust Anthropic cache breakpoints by conversation length so short sessions pay for fewer cache writes and long sessions cache more prefixes.
- Reduced the `skill` tool definition size by moving skill enumeration behind a `list` parameter, and added tiktoken-based token accounting for messages and tool definitions to the agent context window.
- Gave every `Swatinem/rust-cache` step in CI a `shared-key` grouped by platform and compile
- profile, and restricted cache writes to `main` with `save-if`. The repository's cache usage was
- 21.38 GB against GitHub's 10 GB per-repository limit, evicting entries continuously and forcing
- cold rebuilds on most jobs.
- Also moved every `dtolnay/rust-toolchain` step ahead of the `rust-cache` step that follows it.
- `rust-cache` derives its key from the active compiler, so running it first keyed each cache for
- whichever toolchain `rust-toolchain.toml` selected rather than the one the job goes on to install.
- The MSRV job was the clearest case: it cached under `linux-msrv` for the default toolchain and
- then built with 1.97.0. Coverage was affected too, because its `llvm-tools-preview` component was
- added after the cache step.
- Reduced initial prompt size by deferring low-frequency tool families behind filtered discovery, while keeping loaded tools available for the session. Tool search now ranks and caps results, file reads default to 200 lines, and file/content searches default to 50 results.

