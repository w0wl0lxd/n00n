# Fusion Mode — Implementation Plan

Tracked in Linear: **[N00N-69](https://linear.app/n00n/issue/N00N-69/native-fusion-dual-lane-routing-devin-fusion-parity)** (epic)

## Research basis (Cognition Devin Fusion, June–July 2026)

1. **Sidekick pattern**: two parallel fully-capable agents (frontier lead + cheap sidekick), each with own persistent cached context.
2. **Dynamic mid-session routing**: planned for a later phase; the beta keeps the lead model stable and does not switch the main model at compaction.
3. **Lead tuning**: minimal actions; delegate early with spec-quality briefs; own plan, ambiguity, final review, commit.
4. **When delegation fails**: short tasks, serial debugging chains.

## n00n-native improvements

| Cognition | n00n twist |
|-----------|------------|
| Configuration-only sidekick tier | `[agent.fusion].sidekick_tier` with optional trusted plugin auto-tier |
| Proprietary classifiers | Lexical `classify_delegation` + `route_tier` in Lua |
| Devin-only harness | Works with any provider via `task`/`fusion_delegate` infra |
| Hidden routing | `--fusion` CLI + `[agent.fusion]` config + per-session toggle |
| No policy hooks | Skill policy + compaction hooks integration |
| Opaque cost | Lead vs sidekick usage in agent stats (follow-up) |

## Deliverables

- [x] `n00n-agent/src/fusion/` routing core + tests
- [x] `FusionConfig` in `n00n-config`
- [x] `plugins/fusion/init.lua` — `fusion_delegate` tool
- [x] Compaction-boundary model switch in `run.rs`
- [x] `--fusion` / `always_fusion` CLI + config
- [x] Devin `list_models` uses cached ACP config_options
- [ ] Docs (`just gen-docs`) — [N00N-75](https://linear.app/n00n/issue/N00N-75/fusion-mode-docs-gen-docs)

## Linear issues

| ID | Title | Status |
|----|-------|--------|
| [N00N-69](https://linear.app/n00n/issue/N00N-69) | Epic: Native Fusion dual-lane routing | In Progress |
| [N00N-70](https://linear.app/n00n/issue/N00N-70) | Fusion routing core | Done |
| [N00N-71](https://linear.app/n00n/issue/N00N-71) | Config + CLI toggle | Done |
| [N00N-72](https://linear.app/n00n/issue/N00N-72) | fusion_delegate plugin | Done |
| [N00N-73](https://linear.app/n00n/issue/N00N-73) | Compaction routing hook | Done |
| [N00N-74](https://linear.app/n00n/issue/N00N-74) | Devin cached model list | Done |
| [N00N-75](https://linear.app/n00n/issue/N00N-75) | Fusion docs | Done |
| [N00N-76](https://linear.app/n00n/issue/N00N-76) | Lead vs sidekick cost stats | In Progress |
| [N00N-77](https://linear.app/n00n/issue/N00N-77) | Tool-error escalation | Done |
| [N00N-78](https://linear.app/n00n/issue/N00N-78) | Open draft PR | In Review — [PR #180](https://github.com/w0wl0lxd/n00n/pull/180) |
