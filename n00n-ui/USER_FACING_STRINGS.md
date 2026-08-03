# User-facing strings

This inventory defines n00n's canonical user-facing vocabulary. Runtime aliases are compatibility details and are not shown to the model.

## Product and concepts

- Product: **n00n**
- Permission bypass: **no-confirm mode**
- Agent states: **Working**, **Needs input**, **Idle**, **Running**, **Paused**, **Stopping…**, **Stopped**, **Done**, **Failed**
- Model tiers: **Strong**, **Medium**, **Weak**, **Compaction**

## Canonical tools

- **File:** `read_file`, `write_file`, `edit_file`, `edit_file_bulk`, `edit_file_lines`, `insert_file_lines`
- **Search:** `search_files`, `search_code`, `index_file`, `explore_code`, `search_text`, `map_code`, `map_codegraph`
- **Execute:** `run_shell`, `run_python`, `run_batch`
- **Agent:** `list_agents`, `get_agent`, `control_agent`, `run_team`, `run_task`, `run_workflow`, `use_blackboard`
- **Knowledge:** `update_todo`, `use_memory`, `load_skill`
- **Meta:** `search_tools`, `load_toolset`, `delegate_fusion`, `activate_tool`
- **Web:** `fetch_url`, `search_web`
- **Media:** `view_image`
- **Control:** `ask_user`

Descriptions must say when to use the tool and name the better sibling when it should not be used.

## Canonical command labels

Group palette entries under Session, Model, View, Settings, Mode, and Action. Friendly labels describe the outcome. Legacy short forms remain aliases during migration.

Examples:

- **Start new session** (`/session new`, alias `/new`)
- **Switch model** (`/model pick`, alias `/model`)
- **View tasks** (`/view tasks`, alias `/tasks`)
- **Toggle no-confirm mode** (`/mode no-confirm`, alias `/yolo`)
- **Quick question** (`/action ask`, alias `/btw`)
- **Show welcome guide** (`/welcome`)

## Writing guidance

Use plain language, sentence case, and concrete actions. Do not use color as the only state signal. Keep internal protocol values out of user copy. Preserve raw status values in machine-readable output while presenting the labels above to people.
