use std::fmt::Write;

use n00n_ui::{BUILTIN_COMMANDS, CommandCategory};

use crate::lua_util;

const PLUGIN_COMMAND_CATEGORIES: &[(&str, CommandCategory)] = &[
    ("/memory", CommandCategory::View),
    ("/rename", CommandCategory::Session),
    ("/sessions", CommandCategory::Session),
    ("/team", CommandCategory::Action),
];

fn builtin_reserves(name: &str) -> bool {
    BUILTIN_COMMANDS.iter().any(|command| {
        command.name == name || command.dispatch_name == name || command.aliases.contains(&name)
    })
}

fn plugin_category(name: &str) -> Option<CommandCategory> {
    PLUGIN_COMMAND_CATEGORIES
        .iter()
        .find_map(|(command, category)| (*command == name).then_some(*category))
}

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
    let plugin_commands = lua_util::load_builtin_plugin_commands()
        .into_iter()
        .filter(|command| !builtin_reserves(&command.name))
        .collect::<Vec<_>>();

    let _ = writeln!(out, "## Built-in commands");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "n00n has {} built-in commands. Project, user, and MCP prompt commands are separate.",
        BUILTIN_COMMANDS.len() + plugin_commands.len()
    );
    for category in CommandCategory::ALL {
        let builtins = BUILTIN_COMMANDS
            .iter()
            .filter(|command| command.category == category)
            .collect::<Vec<_>>();
        let plugins = plugin_commands
            .iter()
            .filter(|command| plugin_category(&command.name) == Some(category))
            .collect::<Vec<_>>();
        if builtins.is_empty() && plugins.is_empty() {
            continue;
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "### {}", category.label());
        let _ = writeln!(out);
        let _ = writeln!(out, "| Command | Description |");
        let _ = writeln!(out, "|---------|-------------|");
        for command in builtins {
            let _ = writeln!(out, "| `{}` | {} |", command.name, command.description);
        }
        for command in plugins {
            let _ = writeln!(out, "| `{}` | {} |", command.name, command.description);
        }
    }
}

fn write_sessions(out: &mut String) {
    let _ = writeln!(out);
    let _ = writeln!(out, "## Sessions");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Sessions run concurrently. `/session:new` starts a fresh session while the old one keeps working in the background, and `/session:list` shows the live status of each (Working, Waiting for your input, Idle) so you can jump between them. When a background session finishes or is waiting for your input, n00n flashes a note in the status bar."
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
    fn every_plugin_command_has_one_category() {
        let commands = lua_util::load_builtin_plugin_commands();
        let categorized = PLUGIN_COMMAND_CATEGORIES
            .iter()
            .map(|(name, _)| *name)
            .collect::<HashSet<_>>();

        assert_eq!(categorized.len(), PLUGIN_COMMAND_CATEGORIES.len());
        assert_eq!(categorized.len(), commands.len());
        assert!(
            commands
                .iter()
                .all(|command| categorized.contains(command.name.as_str()))
        );
    }

    #[test]
    fn command_reference_explains_namespace_boundary() {
        let docs = generate();
        assert!(docs.contains("Project, user, and MCP prompt commands are separate."));
        assert!(docs.contains("### Session"));
        assert!(docs.contains("### Settings"));
    }

    #[test]
    fn command_reference_uses_canonical_names_only() {
        let docs = generate();
        assert!(docs.contains("| Command | Description |"));
        assert!(!docs.contains("Legacy aliases"));
        assert!(!docs.contains("`/compact`"));
        assert!(!docs.contains("`/fork`"));
    }

    #[test]
    fn migration_guide_covers_command_aliases_and_removal_window() {
        let guide = include_str!("../../site/docs/content/migration/2026-ux-rename.md");
        for command in BUILTIN_COMMANDS {
            for alias in command.aliases {
                assert!(guide.contains(alias), "migration guide omits {alias}");
                assert!(
                    guide.contains(command.name),
                    "migration guide omits canonical command {}",
                    command.name
                );
            }
        }
        assert!(guide.contains("0.6 release line"));
        assert!(guide.contains("0.7 at the earliest"));
        assert!(guide.contains("resolve every legacy-name warning"));
    }

    #[test]
    fn command_reference_lists_team_inventory() {
        let docs = generate();
        assert!(docs.contains("| `/team` | Configure and run an agent team for a goal |"));
    }
}
