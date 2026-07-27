# Feature Specification: Token profiling CI gate

**Feature Branch**: `feat/token-profiling-and-reduction`

**Created**: 2026-07-27

**Status**: Approved (Approach A, post plan-critique)

**Input**: Profile all LLM token surfaces; add an internal profiling crate with CI regression gates; then reduce token usage in 1–3 PRs.

## Critique outcome

Independent plan critique verdict: **SOUND WITH CHANGES**. Blocking fixes applied below. #142 already merged to main; PR3 retargeted.

## Goals

1. Offline, deterministic measurement of controllable per-turn token surfaces (no live LLM).
2. Committed baselines + hard CI failure on regressions for schema/static surfaces.
3. Enable trustworthy PR2/PR3 savings work with measured deltas.

## Non-goals

- Matching Anthropic/OpenAI billing tokenizers exactly (estimator is n00n's tiktoken heuristic).
- Measuring MCP tools, user AGENTS.md, or live conversation history in the hard gate (documented exclusions).
- Rebuilding defer_loading, System cache blocks, or schema minify.

## Surfaces

| Surface | How built | Gate |
|---------|-----------|------|
| `main_tools_schemas` | Cold-start `definitions_active` (MAIN), then strip to `{name,input_schema}` sorted by name | **Hard** |
| `system_prompt` | `build_system_prompt` with pinned Vars, empty instructions, empty slots, Build mode | **Hard** |
| `main_tools_payload` | Full cold-start `definitions_active` (includes `code_execution` describe) | Soft warn |
| `cache_prefix` | Sealed system static blocks + main tools schemas | Soft warn (PR1) |

## Fixture invariants (must match production cold-start)

- Fresh `ToolRegistry::new()` + `PluginHost::with_all_builtins` (not polluted global).
- Model: `anthropic/claude-sonnet-4-6` (pinned).
- `ToolFilter::from_config(&AgentConfig::default(), &model, &[])` (vision gating).
- `ActiveTools::default()` (deferred tools excluded until activated).
- `supports_tool_examples` from the pinned model.
- Pinned `Vars`: `{cwd}=/tmp/n00n-token-profile`, `{platform}=linux`, `{date}=2026-07-27`.
- MCP excluded (document in baseline README comment / report field).
- Sort tool arrays by `name` before byte/token measure.

## Hard gate thresholds

Absolute deltas vs committed baseline (not “min(2%, 100)” which collapses to 100 at current scale):

- `main_tools_schemas`: `tool_count` exact; tokens ≤ baseline + 100; bytes ≤ baseline + 400.
- `system_prompt`: tokens ≤ baseline + 80; bytes ≤ baseline + 320.

Soft: `main_tools_payload` / `cache_prefix` warn at +200 tokens (no fail in PR1).

Intentional growth requires updating `n00n-token-profile/baselines/cold_start.json` in the same PR.

## PR decomposition

1. **PR1** — `n00n-token-profile` crate + baselines + nextest coverage (behavior unchanged).
2. **PR2** — Resolve conflicting #132 (prompt/plugin trims) + rebaseline with measured Δ.
3. **PR3** — Land #144 (provider cache) + remaining profile-driven cuts (#142 already on main).

## Footguns (documented)

- `code_execution.describe` enumerates interpreter tools → payload soft metric moves when interpreter-audience tools change.
- `serde_json` map key order is insertion order; schemas must be measured after stable construction; tool array sorted by name.
- Estimator ≠ provider billing; PR savings claims must cite crate metrics, not invoice tokens.
- #132 merge is conflict-prone (`bash`, `plugin_host`, `n00n-config`, `dynamic_tool_size`).
