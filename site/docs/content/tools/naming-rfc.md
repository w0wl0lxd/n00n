+++
title = "Tool and Command Naming RFC"
weight = 2
[extra]
group = "Reference"
+++

# Tool and command naming RFC

**Status: frozen design contract for Wave 0.** This page records the names users may
configure, type, see in the command palette, and receive from the model. It is an
inventory and migration guide, not an implementation promise for the current
release. The runtime work starts only after the Wave 0 quorum checkpoint.

The contract applies to native built-in tools and n00n-owned slash commands. It does
**not** rename MCP server-qualified names, external provider operation names, or the
configured/authenticated model allowlist.

## What the current code actually does

The current isolated worktree has these relevant boundaries:

| Surface | Current symbol/path | Contract consequence |
| --- | --- | --- |
| Native registry | `ToolRegistry::{get,has,register,register_many,replace_plugin,definitions,definitions_active}` in `n00n-agent/src/tools/registry.rs:336-694` | Registration and definition generation are centralized, but lookup is exact-name only and has no alias table. |
| Native dispatch | `run_authorized` in `n00n-agent/src/agent/tool_dispatch.rs:360-560` | Filters, local tools, registry lookup, MCP wire conversion, permission checks, and unknown-tool errors meet here. |
| Filters/deferred tools | `ToolFilter` and `filter_definitions` in `n00n-agent/src/tools/mod.rs:50-184`; `definitions_active` in `registry.rs:645-694`; active-tool warming in `n00n-agent/src/agent/run.rs:978-1079` | Names must be canonical before filtering or activation; aliases cannot be allowed to leak into active model definitions. |
| Native permission identity | `ToolKey` in `n00n-config/src/lib.rs:715-854`; checks in `n00n-agent/src/permissions.rs:241-673` | Permissions are keyed by native or qualified MCP identity and must resolve aliases before persistence/checking. |
| CLI ingress | `normalize_tool_name` in `src/cli.rs:425-445`; allow/deny flags in `src/cli.rs:133-139`; TUI forwarding in `src/cmd/tui.rs:97-109` | The current PascalCase-to-snake conversion is separate from registry lookup and silently drops invalid disallowed names in TUI forwarding. |
| Lua registration | `register_tool_from_lua` and `register_command_from_lua` in `n00n-lua/src/api/tool.rs:1120-1287` | Lua validates names but currently has no compatibility alias or command metadata fields. |
| MCP identity | `wire_tool_name`/`internal_tool_name` in `n00n-agent/src/mcp/mod.rs:46-80`; publication in `mcp/mod.rs:1004-1063` | `server.tool` is internal and `server__tool` is wire format. The first separator is structural; tool underscores are data. |
| SDK translation | `TOOL_NAME_MAP` and `n00n_to_claude_tool_name` in `src/sdk_mode.rs:40-57,394-399`; reverse mapping in `src/sdk_mode.rs:1272-1297` | The SDK has a hand-maintained compatibility map and emits Claude-style names at its public boundary. |
| Commands | `BUILTIN_COMMANDS` and `CommandPalette` in `n00n-ui/src/components/command.rs:19-169`; custom discovery in `n00n-agent/src/command.rs:8-174` | Built-ins have only name/description/max-args; custom commands currently use source prefixes and later discovery overrides earlier entries. |
| Raw status ownership | `Status`/`ToolStatus` in `n00n-ui/src/components/mod.rs:246-288`; `AgentEvent`/tool events in `n00n-agent/src/types.rs:770-932`; MCP statuses in `n00n-agent/src/mcp/config.rs:102-126` | Raw enum values and serialized event fields are protocol/state data. Presentation labels belong in UI only. |
| Status presentation | `StatusBarContext`/`StatusBar::view` in `n00n-ui/src/components/status_bar.rs:38-69,124-283`; MCP labels in `n00n-ui/src/components/mcp_picker.rs:12-50` | Human labels, color, icons, and animation must be derived views, never replacements for raw state. |
| Generated reference | `n00n-docgen/src/gen_tools.rs:14-58,267-380` and `site/docs/content/tools/_index.md:10-18` | Runtime definitions and generated docs must share one canonical metadata source or fail a drift check. |

The repository root worktree is currently in an unrelated merge-conflict state.
This RFC was added only to the clean isolated worktree; no production symbol is
changed here. Resolve that repository state before applying implementation waves.

## Frozen native tool names

A native tool has one canonical model name, consisting of lowercase ASCII
`verb_noun` segments and no more than 32 bytes. The following table is the complete
native migration inventory found in the built-in/plugin list (`n00n-config/src/lib.rs:60-88`)
and Lua registrations under `plugins/*/init.lua`.

| Canonical model name | Existing accepted names (deprecated aliases) | Source/notes |
| --- | --- | --- |
| `run_command` | `bash`, `Bash` | `plugins/bash/init.lua:625`; shell execution |
| `read_file` | `read`, `Read` | `plugins/read/init.lua:207` |
| `write_file` | `write`, `Write` | `plugins/write/init.lua:23` |
| `edit_file` | `edit`, `Edit` | `plugins/edit/init.lua:233` |
| `edit_multiple` | `multiedit`, `MultiEdit` | `plugins/edit/init.lua:280` |
| `replace_lines` | `edit_lines` | `plugins/edit/init.lua:361` |
| `insert_lines` | — | `plugins/edit/init.lua:411`; already canonical |
| `find_files` | `glob`, `Glob` | `plugins/glob/init.lua:18` |
| `search_content` | `grep`, `Grep` | `plugins/grep/init.lua:204` |
| `inspect_file` | `index`, `Index` | `plugins/index/init.lua:149` |
| `view_image` | — | `plugins/view_image/init.lua:298`; already canonical |
| `explore_code` | `explore` | `plugins/explore/init.lua:51` |
| `query_graph` | `arbor` | `plugins/arbor/init.lua:174` |
| `query_codegraph` | `codegraph` | `plugins/codegraph/init.lua:17` |
| `search_index` | `semblem` | `plugins/semblem/init.lua:20` |
| `run_batch` | `batch` | `plugins/batch/init.lua:499` |
| `ask_question` | `question`, `Question` | `plugins/question/init.lua:25` |
| `write_todo` | `todo_write`, `TodoWrite` | `plugins/todo_write/init.lua:183` |
| `load_skill` | `skill`, `Skill` | `plugins/skill/init.lua:300` |
| `run_task` | `task`, `Task` | `plugins/task/init.lua:331` |
| `list_agents` | `agent_list`, `AgentList` | `plugins/agent_control/init.lua:180` |
| `get_agent_status` | `agent_status`, `AgentStatus` | `plugins/agent_control/init.lua:223` |
| `agent_control` | — | `plugins/agent_control/init.lua:483`; already canonical |
| `manage_blackboard` | `blackboard` | `plugins/blackboard/init.lua:636` |
| `manage_memory` | `memory`, `Memory` | `plugins/memory/init.lua:258` |
| `run_team` | `team` | `plugins/team/init.lua:1067` |
| `run_workflow` | `workflow` | `plugins/workflow/init.lua:833` |
| `fetch_web` | `webfetch`, `WebFetch` | `plugins/webfetch/init.lua:81` |
| `search_web` | `websearch`, `WebSearch` | `plugins/websearch/init.lua:30` |
| `code_execution` | — | `plugins/code_execution/init.lua:282`; already canonical |
| `fusion_delegate` | — | `plugins/fusion/init.lua:124`; already canonical |
| `activate_tool` | — | `plugins/activate_tool/init.lua:5`; already canonical |
| `tool_search` | — | Reserved synthetic MCP search tool, `n00n-agent/src/mcp/mod.rs:48-62`; protocol name |
| `load_namespace` | — | Reserved Lua/deferred activation operation; protocol name |

`web_search_exa` (`plugins/websearch/init.lua:71`) is an upstream/provider operation,
not a n00n native tool, and remains unchanged. Plugin IDs such as `fusion`, `sessions`,
and `workflow` are configuration namespaces, not additional model tool names; the
configuration list is therefore not itself a tool inventory.

### Alias and resolver rules

1. The canonical table above is the only source of truth. A registry build fails if
   two canonical names, aliases, plugin registrations, or reserved synthetic names
   collide. A collision is an error naming both owners; there is no last-writer-wins
   behavior for tools.
2. Resolution precedence is exact canonical name, exact deprecated alias, then an
   explicit compatibility spelling (the listed PascalCase form). There is no general
   case folding, punctuation stripping, or heuristic singular/plural conversion.
3. Aliases are accepted indefinitely at ingress for the compatibility lifetime of
   this protocol: CLI flags, config allow/deny lists, skill policy, registry lookup,
   dispatch, permission keys, deferred activation, Lua handler lookup, SDK input,
   and model-tool call decoding. They are not accepted as a second registry entry.
4. Canonical names are emitted by native model definitions, prompt/tool listings,
   generated docs, UI primary labels, and n00n-owned SDK output. An incoming alias is
   immediately converted to its canonical identity before filtering, authorization,
   activation, or event identity is recorded.
5. A deprecated alias emits exactly one warning per alias per process. Human-facing
   CLI/TUI warnings go to stderr or the visible status/error surface; warnings never
   go to machine-readable stdout, stream-json payloads, MCP responses, or tool output.
   The wording is `warning: '<alias>' is deprecated; use '<canonical>' instead`.
   Alias use does not change the exit status.
6. Unknown names in an allow/deny list are hard errors. The structured diagnostic
   contains `kind = "unknown_tool"`, the supplied value, the field (`allowed` or
   `disallowed`), and the sorted canonical-name list; parsing/configuration exits
   nonzero and never silently drops a value.
7. Persisted permission/config data is canonicalized on write. Reading a deprecated
   key may migrate it once, preserving scope/effect, and emits the same warning.
   MCP names bypass this native resolver and remain qualified exactly as received.

## Name boundaries: native, MCP, SDK

There are three identities, and they must not be conflated:

* **Native canonical identity:** one name from the table above, used by the registry,
  filters, permissions, deferred activation, model definitions, events, and docs.
* **MCP qualified identity:** internal `server.tool`; provider wire name
  `server__tool`; configured permission identity `server.tool` or `server.*`. The
  server prefix is mandatory for dispatch and permission matching. Only the first
  `__` is structural (`wire_tool_name`/`internal_tool_name`); subsequent underscores
  belong to the MCP tool name.
* **SDK public identity:** Claude-compatible names such as `Bash` are a translation
  at the SDK boundary only. The translation must become a table generated from the
  native compatibility registry, and round-trip `(native -> SDK -> native)` without
  changing MCP qualification. Unknown third-party names remain unchanged and are
  never guessed.

The model-schema boundary is `ToolRegistry::definitions`/
`definitions_active` followed by MCP `extend_tools`: native definitions are emitted
with canonical names only, and MCP definitions are emitted with their qualified wire
names. Aliases must never appear in either model definition array.

## Slash-command contract

Canonical slash commands retain the short names users already know; friendly aliases
improve discovery without making the primary UI verbose. Every command is represented
by one typed metadata record:

```text
CommandMetadata {
  canonical: "/name",
  aliases: ["/alias", ...],
  description: string,
  category: CommandCategory,
  order: u16,
  max_args: usize,
  argument_hint: Option<string>,
  permission: CommandPermission,
  source: Builtin | Project | User | Lua(plugin) | Mcp(server),
  deprecation: None | { replacement: "/name", since: version }
}
```

`CommandCategory` is the closed set `Navigation`, `Session`, `Agents`, `Model`,
`Integrations`, `Safety`, `Appearance`, and `Extensions`. The palette groups by
category, then `order`, then canonical name. The metadata record is also the source
for help, `/welcome`, completion, generated command docs, and CLI examples where a
command has a CLI equivalent.

### Built-in commands and aliases

| Category | Canonical | Accepted aliases |
| --- | --- | --- |
| Navigation | `/help` | `/show-help` |
| Navigation | `/welcome` | — (new onboarding entry point) |
| Navigation | `/cd` | `/change-directory` |
| Session | `/new` | `/new-session` |
| Session | `/compact` | `/compact-history` |
| Session | `/queue` | `/manage-queue` |
| Session | `/reload` | `/reload-config` |
| Session | `/exit` | `/quit` |
| Agents | `/tasks` | `/agents`, `/agent-tasks` |
| Model | `/model` | `/select-model` |
| Model | `/login` | `/login-provider` |
| Integrations | `/mcp` | `/manage-mcp` |
| Safety | `/yolo` | `/toggle-permissions` |
| Safety | `/thinking` | `/set-thinking` |
| Safety | `/fast` | `/toggle-fast` |
| Safety | `/workflow` | `/toggle-workflow` |
| Appearance | `/theme` | `/select-theme` |
| Appearance | `/usage` | `/show-usage` |
| Extensions | `/btw` | `/ask` |

Existing extension commands retain their canonical names and gain discoverable
aliases: `/sessions` -> `/list-sessions`, `/rename` -> `/rename-session`, `/memory`
-> `/manage-memory`, and `/team` -> `/run-team` (registrations at
`plugins/sessions/init.lua:795-801`, `plugins/memory/init.lua:368`, and
`plugins/team/init.lua:1305`). Custom commands remain source-qualified as
`/project:name` or `/user:name` (`n00n-agent/src/command.rs:65-73`).

Command collision rules are deterministic:

* Built-in canonical names and aliases reserve the global namespace. A project,
  user, Lua, or MCP command that claims one is rejected with an actionable error.
* For otherwise identical custom names, project scope overrides user scope, matching
  the existing discovery order (`command.rs:100-149`), and the override is reported.
* Lua commands are unique by `(plugin, canonical)`; reloading replaces only that
  plugin's entries. MCP prompts are always displayed as `/server:prompt` when an
  unqualified name would be ambiguous.
* Exact canonical wins over an alias. An alias collision or an ambiguous external
  name is an error, not a palette duplicate. Deprecated command aliases warn once
  per process with the same stderr/visible-surface and machine-output rules as tool
  aliases.

## Raw status and presentation contract

Raw state remains byte-for-byte/API-compatible. No serializer tag, enum variant,
event field, or error payload is renamed. UI labels are the following derived
presentation values:

| Raw owner/value | UI label | Allowed visual treatment |
| --- | --- | --- |
| `Status::Idle` | `Ready` | neutral text/icon |
| `Status::Streaming` | `Working` | spinner only when motion is enabled; static `Working` otherwise |
| `Status::Error { .. }` | `Error` plus the actionable message | text plus icon/semantic style; never color alone |
| `ToolStatus::InProgress` | `Running` | progress icon/text |
| `ToolStatus::Success` | `Done` | check icon/text |
| `ToolStatus::Error` | `Failed` | error icon/text |
| `McpServerStatus::Connecting` | `Connecting` | progress icon/text |
| `McpServerStatus::Running` | `Connected` | connected icon/text |
| `McpServerStatus::Disabled` | `Disabled` | muted text |
| `McpServerStatus::Failed(reason)` | `Error: {reason}` | visible actionable text |
| `McpServerStatus::NeedsAuth` | `Sign-in required` | action text, including the existing `n00n mcp auth <server>` route |

The agent owns raw events and statuses; the UI owns the label mapping and layout.
CLI/SDK/MCP machine output keeps raw values. Semantic theme slots are
`status.progress`, `status.success`, `status.warning`, `status.error`,
`status.muted`, `focus`, `selection`, `permission.allow`, `permission.deny`, and
`permission.scope`. `NO_COLOR` removes styling but retains labels/icons; high
contrast adds redundant text/icon distinctions; reduced motion uses no animated
spinner. Status must remain understandable in narrow terminals without color.

The footer may show at most five contextual hints. Priority is, in order: active
permission decision, active error/retry action, cancel/submit action, focused
picker navigation, then help. At narrow widths it keeps the first three applicable
short labels and drops decorative/model-cost hints; it never clips an actionable
hint.

## User-facing permission and onboarding rules

Permission prompts display the canonical tool label plus plain-language scopes:
`Run command: cargo test`, `Write file: src/main.rs`, or `Use MCP server
`server` tool `tool``. The raw `ToolKey` and exact scope remain available in the
expanded/help view. Allow/deny decisions are applied to canonical native identities
and fully qualified MCP identities; a path scope is never reduced to a tool-only
permission in the UI.

`/welcome` is the first-run and re-openable onboarding route. It must expose, in
this order: canonical command discovery and aliases; permission scope examples;
model picker and MCP picker entry points; active tool/subagent monitoring; status
and session navigation; help and tool-output inspection. It may improve model
labels/picker presentation but does not change configured/authenticated model
allowlist or auth-selection/discovery behavior.

## Migration map

| Legacy input | Canonical replacement | Applies to |
| --- | --- | --- |
| `bash`, `Bash` | `run_command` | CLI, config, model ingress, SDK translation |
| `read`, `Read` | `read_file` | same |
| `write`, `Write` | `write_file` | same |
| `edit`, `Edit` | `edit_file` | same |
| `multiedit`, `MultiEdit` | `edit_multiple` | same |
| `edit_lines` | `replace_lines` | same |
| `glob`, `Glob` | `find_files` | same |
| `grep`, `Grep` | `search_content` | same |
| `index`, `Index` | `inspect_file` | same |
| `explore` | `explore_code` | same |
| `arbor` | `query_graph` | same |
| `codegraph` | `query_codegraph` | same |
| `semblem` | `search_index` | same |
| `batch` | `run_batch` | same |
| `question`, `Question` | `ask_question` | same |
| `todo_write`, `TodoWrite` | `write_todo` | same |
| `skill`, `Skill` | `load_skill` | same |
| `task`, `Task` | `run_task` | same |
| `agent_list`, `AgentList` | `list_agents` | same |
| `agent_status`, `AgentStatus` | `get_agent_status` | same |
| `blackboard` | `manage_blackboard` | same |
| `memory`, `Memory` | `manage_memory` | same |
| `team` | `run_team` | same |
| `workflow` | `run_workflow` | same |
| `webfetch`, `WebFetch` | `fetch_web` | same |
| `websearch`, `WebSearch` | `search_web` | same |
| `/new-session` | `/new` | slash command ingress |
| `/compact-history` | `/compact` | slash command ingress |
| `/show-help` | `/help` | slash command ingress |
| `/show-usage` | `/usage` | slash command ingress |
| `/select-model` | `/model` | slash command ingress |
| `/select-theme` | `/theme` | slash command ingress |
| `/manage-mcp` | `/mcp` | slash command ingress |
| `/change-directory` | `/cd` | slash command ingress |
| `/toggle-permissions` | `/yolo` | slash command ingress |
| `/set-thinking` | `/thinking` | slash command ingress |
| `/toggle-fast` | `/fast` | slash command ingress |
| `/toggle-workflow` | `/workflow` | slash command ingress |
| `/quit` | `/exit` | slash command ingress |
| `/reload-config` | `/reload` | slash command ingress |
| `/agents`, `/agent-tasks` | `/tasks` | slash command ingress |
| `/ask` | `/btw` | slash command ingress |
| `/list-sessions` | `/sessions` | Lua command ingress |
| `/rename-session` | `/rename` | Lua command ingress |
| `/manage-memory` | `/memory` | Lua command ingress |
| `/run-team` | `/team` | Lua command ingress |

MCP `server.tool`/`server__tool` and provider operation `web_search_exa` are not
migration rows: changing them would violate their external wire contracts.

## Dependency-aware implementation waves

The following waves are ordered by contract dependency. No later wave may add a
second resolver, emit a legacy name, or redefine a category/status label.

### Wave 0 — inventory, contracts, and fixtures (design gate)

1. Add the canonical registry table, alias lifetime/warning policy, collision
   diagnostics, command metadata schema/taxonomy, status mapping, semantic theme
   tokens, footer priorities, and migration fixture files. Keep these in the
   registry/metadata source selected by the implementation team; generated docs
   consume it rather than duplicating it.
2. Add golden fixtures for canonical-only model definitions, deprecated ingress,
   unknown allow/deny errors, MCP qualifier round-trips, SDK translation, raw event
   statuses, command alias/collision resolution, and narrow footer behavior.
3. Re-check this page against every current registration and generated reference.
   **C0 requires 3 of 4 independent validators.** Any missing registration,
   disputed mapping, or source-of-truth disagreement stops Wave 1.

### Wave 1 — canonical registry and compatibility layer (depends on C0)

1. **Registry:** extend `RegisteredTool`/`ToolRegistry` with canonical metadata and
   a centralized alias resolver. Make registration and batch/plugin replacement
   validate canonical/alias/reserved collisions atomically (`registry.rs:387-525`).
2. **Dispatch:** resolve aliases at `run_authorized` ingress before filter, local
   tool, plan-mode, audience, parse, permission, and event identity decisions
   (`tool_dispatch.rs:360-560`). Preserve `functions.` stripping as a separate
   provider compatibility step.
3. **Filters/permissions/config:** canonicalize `ToolFilter`, active/deferred names,
   skill policy, `ToolKey`, permission persistence, and both allow/deny lists. Return
   structured nonzero errors for unknown entries; do not use `filter_map` to discard
   invalid values (`tools/mod.rs:50-184`, `n00n-config/src/lib.rs:715-854`,
   `n00n-agent/src/permissions.rs:241-673`).
4. **Deferred activation:** canonicalize `ActiveTools`, `tool_search` results,
   namespace activation, and MCP loaded-name checks without changing the deferred
   search protocol (`registry.rs:609-694`, `mcp/mod.rs:418-525`).
5. **Lua:** accept legacy registration names and handler lookups through the same
   resolver, publish canonical names, and add command aliases/metadata to the Lua
   API (`n00n-lua/src/api/tool.rs:1120-1287`; plugin handlers remain compatible).
6. **Model/MCP/SDK:** emit only canonical native schemas; retain exact MCP
   qualification and generate SDK translations from the compatibility table
   (`mcp/mod.rs:1004-1063`, `src/sdk_mode.rs:40-57,394-399`).
7. Add focused unit/contract/protocol tests. **C1 requires 3 of 4 validators,
   including registry/protocol and permissions/config.** Stop on alias leakage,
   lost qualifier, raw-status mutation, or permissive unknown handling.

### Wave 2 — command metadata, palette, CLI, and onboarding (depends on C1)

1. Replace `BuiltinCommand`'s ad-hoc fields with typed metadata and one resolver;
   retain source-qualified custom commands while enforcing the collision rules
   (`n00n-ui/src/components/command.rs:19-169`, `n00n-agent/src/command.rs:50-174`).
2. Make palette grouping/order, completion, execution, help, `/welcome`, and Lua/MCP
   command presentation consume those metadata records. Deprecated aliases are
   discoverable but never primary.
3. Normalize CLI flags, examples, config, SDK-parity flags, warnings, and TUI
   forwarding around the same tool resolver (`src/cli.rs:133-139,205-235,425-445`,
   `src/cmd/tui.rs:75-109`). Warnings stay off machine-readable output.
4. Add command collision, alias, warning, help-example, palette grouping, onboarding,
   and migration tests. **C2 requires 3 of 4 validators.**

### Wave 3 — presentation, permissions, and accessibility (depends on C1 and C2)

1. Add semantic theme slots and raw-to-label mapping at the UI boundary only.
2. Implement the footer/activity shelf/status bar/session navigation with the five-hint
   limit, narrow fallback, `NO_COLOR`, high contrast, reduced motion, and keyboard
   navigation (`n00n-ui/src/components/status_bar.rs:38-283`, `n00n-ui/src/app/view.rs:294-338`).
3. Improve permission approval text with exact command/path scope; update model/MCP
   pickers, help, and tool-output inspection without changing model allowlist/auth
   behavior (`n00n-ui/src/components/mcp_picker.rs:12-50`).
4. Add snapshots and scenario tests for narrow terminals, color modes, keyboard-only
   navigation, hidden errors, status preservation, picker collisions, and ≤5 hints.
   **C3 requires 3 of 4 validators.**

### Wave 4 — generated docs, migration, and full verification (depends on all prior waves)

1. Generate `site/docs/content/tools/_index.md`, CLI help references, Lua API
   examples, SDK compatibility notes, and migration pages from the canonical source;
   add a drift check against runtime definitions (`n00n-docgen/src/gen_tools.rs:267-380`).
2. Document every row in the native and command migration tables, including aliases,
   warning behavior, MCP/SDK boundaries, and unknown allow/deny failures.
3. Run layered validation: unit/property tests; registry/filter/permission contracts;
   CLI/config/Lua integration; MCP/SDK round-trips; canonical-only model schema;
   TUI snapshots/responsive/accessibility; doc generation/drift; then formatting,
   `cargo check`, clippy, and workspace nextest. **Final gate is 4 of 4 validators**
   or a written escalation for dissent.

## Explicit non-goals and stop conditions

* Do not change raw statuses, machine-readable output, MCP server-qualified names,
  SDK wire framing, Lua handler semantics, permission scope meaning, or model
  allowlist/auth-selection behavior.
* Do not add a dependency or unsafe code for naming, metadata, or presentation.
* Do not silently swallow unknown values or failed registration.
* Do not begin Wave 1 from a dirty/conflicted worktree; the implementation branch
  must start from this frozen contract in a clean isolated worktree.
