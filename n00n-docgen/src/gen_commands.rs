use std::fmt::Write;

use n00n_ui::BUILTIN_COMMANDS;

use crate::lua_util;

const COMMAND_GROUPS: &[(&str, &[&str])] = &[
    ("Session", &["/new", "/sessions", "/rename", "/exit"]),
    ("Agent work", &["/tasks", "/team", "/queue", "/workflow"]),
    ("Conversation", &["/compact", "/btw", "/thinking", "/fast"]),
    (
        "Settings and access",
        &[
            "/help", "/usage", "/model", "/theme", "/mcp", "/login", "/cd", "/yolo", "/reload",
        ],
    ),
    ("Plugin", &["/memory"]),
];

pub fn generate() -> String {
    let mut out = String::new();
    write_header(&mut out);
    write_builtin_commands(&mut out);
    write_sessions(&mut out);
    write_custom_commands(&mut out);
    write_metadata(&mut out);
    write_arguments(&mut out);
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

fn write_header(out: &mut String) {
    let _ = writeln!(out, "+++");
    let _ = writeln!(out, "title = \"Commands\"");
    let _ = writeln!(out, "weight = 5");
    let _ = writeln!(out, "[extra]");
    let _ = writeln!(out, "group = \"Reference\"");
    let _ = writeln!(out, "+++");
    let _ = writeln!(out);
    let _ = writeln!(out, "# Commands");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Type `/` in the input box to open the command palette."
    );
    let _ = writeln!(out);
}

fn write_builtin_commands(out: &mut String) {
    let mut commands: Vec<(&str, &str)> = BUILTIN_COMMANDS
        .iter()
        .map(|cmd| (cmd.name, cmd.description))
        .collect();
    let plugin_commands = lua_util::load_builtin_plugin_commands();
    commands.extend(
        plugin_commands
            .iter()
            .map(|cmd| (cmd.name.as_str(), cmd.description.as_str())),
    );

    let _ = writeln!(out, "## Built-in commands");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "n00n has {} built-in commands. Project, user, and MCP prompt commands are separate.",
        commands.len()
    );
    for (group, names) in COMMAND_GROUPS {
        let entries: Vec<(&str, &str)> = names
            .iter()
            .filter_map(|name| {
                commands
                    .iter()
                    .find(|(current, _)| current == name)
                    .copied()
            })
            .collect();
        if entries.is_empty() {
            continue;
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "### {group}");
        let _ = writeln!(out);
        let _ = writeln!(out, "| Command | Description |");
        let _ = writeln!(out, "|---------|-------------|");
        for (name, description) in entries {
            let _ = writeln!(out, "| `{name}` | {description} |");
        }
    }
}

fn write_sessions(out: &mut String) {
    let _ = writeln!(out);
    let _ = writeln!(out, "## Sessions");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Sessions run concurrently. `/new` starts a fresh session while the old one keeps working in the background, and `/sessions` shows the live status of each (working, needs input, idle) so you can jump between them. When a background session finishes or needs input, n00n flashes a note in the status bar."
    );
}

fn write_custom_commands(out: &mut String) {
    let _ = writeln!(out);
    let _ = writeln!(out, "## Custom commands");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "You can define your own slash commands as Markdown files."
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "### Project commands");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Place `.md` files in `.n00n/commands/` in your project root."
    );
    let _ = writeln!(out, "They appear in the palette as `/project:<filename>`.");
    let _ = writeln!(out);

    let _ = writeln!(out, "### User commands");
    let _ = writeln!(out);
    let _ = writeln!(out, "Place `.md` files in `~/.config/n00n/commands/`.");
    let _ = writeln!(out, "They appear in the palette as `/user:<filename>`.");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Project commands override user commands with the same name."
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "`.claude/commands/` directories are also supported for compatibility."
    );
    let _ = writeln!(out);
}

fn write_metadata(out: &mut String) {
    let _ = writeln!(out, "### Metadata");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "You can add optional metadata at the top of the file between `---` lines to set `name`, `description`, and `argument-hint`:"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "```markdown");
    let _ = writeln!(out, "---");
    let _ = writeln!(out, "description: Review code for issues");
    let _ = writeln!(out, "argument-hint: <file>");
    let _ = writeln!(out, "---");
    let _ = writeln!(out, "Review $ARGUMENTS and suggest improvements.");
    let _ = writeln!(out, "```");
    let _ = writeln!(out);
}

fn write_arguments(out: &mut String) {
    let _ = writeln!(out, "### Arguments");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Use `$ARGUMENTS` in the command body. It gets replaced with whatever you type after the command name."
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "For example, `/project:review main.rs` replaces `$ARGUMENTS` with `main.rs`."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn command_groups_cover_core_commands_once() {
        let grouped: Vec<&str> = COMMAND_GROUPS
            .iter()
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        let expected: Vec<&str> = BUILTIN_COMMANDS.iter().map(|cmd| cmd.name).collect();
        let unique: HashSet<&str> = grouped.iter().copied().collect();

        assert_eq!(grouped.len(), unique.len());
        assert_eq!(unique.len(), expected.len() + 4);
        assert!(expected.iter().all(|name| unique.contains(name)));
        assert!(
            ["/memory", "/rename", "/sessions", "/team"]
                .iter()
                .all(|name| unique.contains(name))
        );
    }

    #[test]
    fn command_reference_explains_namespace_boundary() {
        let docs = generate();
        assert!(docs.contains("Project, user, and MCP prompt commands are separate."));
        assert!(docs.contains("### Session"));
        assert!(docs.contains("### Settings and access"));
    }
}
