use std::fmt::Write;

use n00n_ui::BUILTIN_COMMANDS;

use crate::lua_util;

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
    let _ = writeln!(out, "## Built-in commands");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Command | Description |");
    let _ = writeln!(out, "|---------|-------------|");
    for cmd in BUILTIN_COMMANDS {
        let _ = writeln!(out, "| `{}` | {} |", cmd.name, cmd.description);
    }
    for cmd in &lua_util::load_builtin_plugin_commands() {
        let _ = writeln!(out, "| `{}` | {} |", cmd.name, cmd.description);
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
