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
- **Meta:** `search_tools`, `load_toolset`, `delegate_fusion`
- **Web:** `fetch_url`, `search_web`
- **Media:** `view_image`
- **Control:** `ask_user`

Descriptions must say when to use the tool and name the better sibling when it should not be used.

## Canonical command labels

Group palette entries under Session, Model, View, Settings, Mode, and Action. Friendly labels describe the outcome. Legacy short forms and former session-scoped action forms remain aliases during migration.

Examples:

- **Start a new session** (`/session:new`, alias `/new`)
- **Fork the current session** (`/session:fork`, alias `/fork`)
- **Switch model** (`/model:pick`, alias `/model`)
- **View tasks** (`/view:tasks`, alias `/tasks`)
- **Toggle no-confirm mode** (`/mode:no-confirm`, alias `/yolo`)
- **Quick question** (`/action:ask`, alias `/btw`)
- **Compact conversation history** (`/action:compact`, aliases `/compact`, `/session:compact`)
- **Reload plugins and configuration** (`/action:reload`, aliases `/reload`, `/session:reload`)
- **Exit n00n** (`/action:exit`, aliases `/exit`, `/session:exit`)
- **Show the welcome guide** (`/welcome`)

## Canonical flags

- **Resume a session** (`--session <id>` or `-s <id>`, alias `--resume <id>`)

## Writing guidance

Use plain language, sentence case, and concrete actions. Do not use color as the only state signal. Keep internal protocol values out of user copy. Preserve raw status values in machine-readable output while presenting the labels above to people.
