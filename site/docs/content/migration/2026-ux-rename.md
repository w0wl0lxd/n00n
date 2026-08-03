# 2026 UX naming migration

n00n now presents built-in tools with consistent `verb_noun` names. Existing names remain supported as deprecated aliases for at least one minor release.

## Tool names

| Previous name | Canonical name |
|---|---|
| `read` | `read_file` |
| `write` | `write_file` |
| `edit` | `edit_file` |
| `multiedit` | `edit_file_bulk` |
| `multi_edit` | `edit_file_bulk` |
| `edit_lines` | `edit_file_lines` |
| `insert_lines` | `insert_file_lines` |
| `bash` | `run_shell` |
| `code_execution` | `run_python` |
| `batch` | `run_batch` |
| `glob` | `search_files` |
| `grep` | `search_code` |
| `index` | `index_file` |
| `explore` | `explore_code` |
| `semblem` | `search_text` |
| `arbor` | `map_code` |
| `codegraph` | `map_codegraph` |
| `webfetch` | `fetch_url` |
| `websearch` | `search_web` |
| `question` | `ask_user` |
| `agent_list` | `list_agents` |
| `agent_status` | `get_agent` |
| `agent_control` | `control_agent` |
| `team` | `run_team` |
| `task` | `run_task` |
| `workflow` | `run_workflow` |
| `blackboard` | `use_blackboard` |
| `todo_write` | `update_todo` |
| `memory` | `use_memory` |
| `skill` | `load_skill` |
| `tool_search` | `search_tools` |
| `load_namespace` | `load_toolset` |
| `fusion_delegate` | `delegate_fusion` |

`view_image` is already canonical and does not change.

Aliases work in tool dispatch, permission rules, deferred activation, and tool filters. Model-facing definitions contain canonical names only. New configuration and documentation should use canonical names.

## Slash commands

The command palette groups commands by purpose and shows canonical names. Previous short commands and the former session-scoped action paths remain compatibility aliases.

| Previous command | Canonical command |
|---|---|
| `/new` | `/session:new` |
| `/fork` | `/session:fork` |
| `/sessions` | `/session:list` |
| `/rename` | `/session:rename` |
| `/model` | `/model:pick` |
| `/tasks` | `/view:tasks` |
| `/usage` | `/view:usage` |
| `/memory` | `/view:memory` |
| `/theme` | `/settings:theme` |
| `/mcp` | `/settings:mcp` |
| `/login` | `/settings:login` |
| `/yolo` | `/mode:no-confirm` |
| `/fast` | `/mode:fast` |
| `/workflow` | `/mode:workflow` |
| `/thinking` | `/mode:thinking` |
| `/session:compact` | `/action:compact` |
| `/compact` | `/action:compact` |
| `/queue` | `/action:queue` |
| `/cd` | `/action:cd` |
| `/btw` | `/action:ask` |
| `/help` | `/action:help` |
| `/session:reload` | `/action:reload` |
| `/reload` | `/action:reload` |
| `/session:exit` | `/action:exit` |
| `/exit` | `/action:exit` |
| `/welcome` | `/welcome` |
| `/team` | `/team` |

## Flags

Use `--session <id>` (or `-s <id>`) to resume a session. `--resume <id>` remains a compatibility alias. Use `--no-confirm` for non-interactive permission approval. `--yolo` remains a deprecated alias during the migration period.

## Timeline

- **Current release:** canonical names are shown; previous names continue to work with a deprecation warning.
- **At least one minor release:** aliases remain supported while scripts and permission rules migrate.
- **Before alias removal:** release notes will announce the exact version and provide a migration check.

MCP server-qualified names and SDK protocol translations are unchanged.
