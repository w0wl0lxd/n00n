use std::fmt::Write;
use std::sync::Arc;

use n00n_agent::tools::ToolRegistry;
use n00n_config::{
    AgentConfig, ConfigField, DEFAULT_MAX_LOG_FILES, DEFAULT_MAX_OUTPUT_LINES,
    DEFAULT_MOUSE_SCROLL_LINES, MIN_TOOL_OUTPUT_LINES, ProviderConfig, StorageConfig,
    TOP_LEVEL_FIELDS, ToolOutputLines, UiConfig,
};
use n00n_lua::{PluginHost, PluginOptionSpecs};

type GenResult<T> = Result<T, String>;

fn write_table_with_min(out: &mut String, fields: &[ConfigField]) {
    let _ = writeln!(out, "| Field | Type | Default | Min | Description |");
    let _ = writeln!(out, "|-------|------|---------|-----|-------------|");
    for f in fields {
        let default = f.default.format_default();
        let min = f.min.map_or_else(|| "-".to_string(), |v| v.to_string());
        let _ = writeln!(
            out,
            "| `{name}` | {ty} | `{default}` | {min} | {desc} |",
            name = f.name,
            ty = escape_pipes(f.ty),
            desc = f.description,
        );
    }
}

fn write_table_no_min(out: &mut String, fields: &[ConfigField]) {
    let _ = writeln!(out, "| Field | Type | Default | Description |");
    let _ = writeln!(out, "|-------|------|---------|-------------|");
    for f in fields {
        let default = f.default.format_default();
        let _ = writeln!(
            out,
            "| `{name}` | {ty} | `{default}` | {desc} |",
            name = f.name,
            ty = escape_pipes(f.ty),
            desc = f.description,
        );
    }
}

fn escape_pipes(ty: &str) -> String {
    ty.replace('|', "\\|")
}

fn has_any_min(fields: &[ConfigField]) -> bool {
    fields.iter().any(|f| f.min.is_some())
}

fn lua_section_name(heading: &str) -> String {
    heading
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string()
}

fn write_section(out: &mut String, heading: &str, fields: &[ConfigField]) {
    let lua_name = lua_section_name(heading);
    let _ = writeln!(out, "### `{lua_name}`\n");
    if has_any_min(fields) {
        write_table_with_min(out, fields);
    } else {
        write_table_no_min(out, fields);
    }
    let _ = writeln!(out);
}

fn write_plugin_options(out: &mut String, specs: &PluginOptionSpecs) {
    for (plugin, options) in specs {
        let _ = writeln!(out, "### `plugins.{plugin}`\n");
        let _ = writeln!(out, "| Field | Type | Default | Min | Description |");
        let _ = writeln!(out, "|-------|------|---------|-----|-------------|");
        for o in options {
            let default = o
                .default
                .as_ref()
                .map_or_else(|| "-".to_string(), |d| format!("`{d}`"));
            let min = o.min.map_or_else(|| "-".to_string(), |m| m.to_string());
            let _ = writeln!(
                out,
                "| `{name}` | {ty} | {default} | {min} | {desc} |",
                name = o.name,
                ty = o.ty,
                desc = o.desc,
            );
        }
        let _ = writeln!(out);
    }
}

fn collect_plugin_options() -> GenResult<PluginOptionSpecs> {
    let host = PluginHost::with_all_builtins(Arc::new(ToolRegistry::new()))
        .map_err(|e| format!("failed to load builtins: {e}"))?;
    let specs = host
        .plugin_options()
        .map_err(|e| format!("failed to collect plugin options: {e}"))?;
    if specs.is_empty() {
        return Err("no plugin declared options; the plugins reference would be empty".to_string());
    }
    Ok(specs)
}

fn write_tool_output_section(out: &mut String) {
    let _ = writeln!(out, "### `ui.tool_output_lines`\n");
    let _ = writeln!(
        out,
        "How many lines of output to show per tool in the UI. \
         All values are `usize` with a minimum of {MIN_TOOL_OUTPUT_LINES}.\n"
    );
    let _ = writeln!(out, "| Field | Default |");
    let _ = writeln!(out, "|-------|---------|");
    for (name, default) in ToolOutputLines::FIELD_DEFAULTS {
        let _ = writeln!(out, "| `{name}` | {default} |");
    }
    let _ = writeln!(out);
}

#[allow(clippy::too_many_lines)]
pub fn generate() -> GenResult<String> {
    let mut out = String::with_capacity(4096);

    let _ = writeln!(
        out,
        "\
+++
title = \"Configuration\"
weight = 2
[extra]
group = \"Getting Started\"
+++

# Configuration

Settings go in `init.lua`, a Lua script that calls `n00n.setup()`. Same language as plugins.

Two places, both optional:

- **Global**: `~/.config/n00n/init.lua`
- **Project**: `.n00n/init.lua` (relative to your working directory)

When both exist, project settings override global ones. Neither file is required.

## Example

```lua
n00n.setup({{
    ui = {{
        splash_animation = true,
        mouse_scroll_lines = {mouse_scroll},
        tool_output_lines = {{
            bash = {tol_bash},
            read = {tol_read},
        }},
    }},
    agent = {{
        max_output_lines = {max_output_lines},
    }},
    provider = {{
        default_model = \"anthropic/claude-sonnet-4-6\",
    }},
    storage = {{
        max_log_files = {max_log_files},
    }},
    plugins = {{
        bash = {{ timeout_secs = 180 }},
        index = {{ max_file_size_mb = 4 }},
    }},
}})
```

All fields are optional. Typos in field names cause an error right away.

`n00n.setup()` can only be called once per init.lua.

## Full Reference
",
        mouse_scroll = DEFAULT_MOUSE_SCROLL_LINES + 2,
        tol_bash = ToolOutputLines::DEFAULT.bash + 3,
        tol_read = ToolOutputLines::DEFAULT.read + 2,
        max_output_lines = DEFAULT_MAX_OUTPUT_LINES + 1000,
        max_log_files = DEFAULT_MAX_LOG_FILES / 2,
    );

    let _ = writeln!(out, "### Top-level\n");
    write_table_no_min(&mut out, TOP_LEVEL_FIELDS);
    let _ = writeln!(out);

    write_section(&mut out, "[ui]", UiConfig::FIELDS);
    write_tool_output_section(&mut out);
    write_section(&mut out, "[agent]", AgentConfig::FIELDS);
    write_section(&mut out, "[provider]", ProviderConfig::FIELDS);
    write_section(&mut out, "[storage]", StorageConfig::FIELDS);

    let _ = writeln!(out, "## Plugins\n");
    let _ = writeln!(
        out,
        "The `plugins` table turns plugins on or off and passes options to \
         them. All bundled plugins are on by default. Set \
         `enabled = false` to turn one off.\n\n\
         Each plugin checks its own options at startup. A typo, a wrong \
         type, or an unknown plugin name gives you a clear error right \
         away.\n\n\
         The edit plugin's extra tools are options too: \
         `plugins.edit = {{ multiedit = false, edit_lines = true }}`. \
         The old `tools` table is gone. If your config still uses it, \
         N00n stops at startup and shows you the new form.\n"
    );
    let _ = writeln!(
        out,
        "\
```lua
n00n.setup({{
    plugins = {{
        bash = {{ timeout_secs = 180 }},
        websearch = {{ enabled = false }},
    }},
}})
```\n"
    );

    let specs = collect_plugin_options()?;
    write_plugin_options(&mut out, &specs);

    let _ = writeln!(out, "## Validation\n");
    let _ = writeln!(
        out,
        "If a value is below its minimum, N00n shows a `ConfigError` with the field name, \
         value, and minimum."
    );

    let _ = writeln!(
        out,
        "
## Directory layout

N00n uses XDG directories on Linux and macOS:

|| Purpose | Path |
||---------|------|
|| Config | `~/.config/n00n/` (init.lua, permissions.toml, mcp.toml) |
|| Data | `~/.local/share/n00n/` |
|| Logs | `~/.local/logs/n00n/` |
|| State | `~/.local/state/n00n/` |

## Personal Instructions

On top of `AGENTS.md`, you can add your own instructions in two places:

- `AGENTS.local.md` at project root for per-project preferences (gitignored)
- `~/.config/n00n/AGENTS.md` for preferences that apply to all projects

Both are added to the system prompt at the start of every session."
    );

    Ok(out)
}
