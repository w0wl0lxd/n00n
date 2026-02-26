use maki_agent::tools::{
    BASH_TOOL_NAME, BATCH_TOOL_NAME, EDIT_TOOL_NAME, GLOB_TOOL_NAME, GREP_TOOL_NAME,
    MULTIEDIT_TOOL_NAME, READ_TOOL_NAME, TASK_TOOL_NAME, TODOWRITE_TOOL_NAME, WEBFETCH_TOOL_NAME,
    WRITE_TOOL_NAME,
};
use maki_providers::{
    BatchToolEntry, DiffHunk, DiffLine, DiffSpan, GrepFileEntry, GrepMatch, TodoItem, TodoPriority,
    TodoStatus, ToolInput, ToolOutput,
};

use crate::components::{DisplayMessage, DisplayRole, ToolStatus};

fn msg(role: DisplayRole, text: &str) -> DisplayMessage {
    DisplayMessage {
        role,
        text: text.into(),
        tool_input: None,
        tool_output: None,
    }
}

fn tool(
    id: &str,
    name: &'static str,
    status: ToolStatus,
    text: &str,
    input: Option<ToolInput>,
    output: Option<ToolOutput>,
) -> DisplayMessage {
    DisplayMessage {
        role: DisplayRole::Tool {
            id: id.into(),
            status,
            name,
        },
        text: text.into(),
        tool_input: input,
        tool_output: output,
    }
}

pub fn mock_messages() -> Vec<DisplayMessage> {
    vec![
        // #1 User
        msg(DisplayRole::User, "Refactor the config module to use builder pattern and add validation."),
        // #2 Thinking
        msg(DisplayRole::Thinking, "Let me analyze the config module structure. I'll need to look at the existing implementation, understand the current API surface, and plan the refactor to use a builder pattern with proper validation."),
        // #3 Assistant (rich markdown)
        msg(DisplayRole::Assistant, "I'll refactor the config module. Let me start by reading the current implementation.\n\n## Plan\n1. Read existing config\n2. Create builder struct\n3. Add validation\n4. Update tests"),
        // #4 Bash — Success, Plain, header+body
        tool(
            "t_bash",
            BASH_TOOL_NAME,
            ToolStatus::Success,
            "ls -la src/config/ (12 lines)\n-rw-r--r-- 1 user staff  2048 Jan 15 10:30 mod.rs\n-rw-r--r-- 1 user staff  1024 Jan 15 10:30 builder.rs\n-rw-r--r-- 1 user staff   512 Jan 15 10:30 validation.rs",
            Some(ToolInput::Code {
                language: "bash",
                code: "ls -la src/config/".into(),
            }),
            Some(ToolOutput::Plain(
                "-rw-r--r-- 1 user staff  2048 Jan 15 10:30 mod.rs\n\
                 -rw-r--r-- 1 user staff  1024 Jan 15 10:30 builder.rs\n\
                 -rw-r--r-- 1 user staff   512 Jan 15 10:30 validation.rs"
                    .into(),
            )),
        ),
        // #5 Read — Success, ReadCode
        tool(
            "t_read",
            READ_TOOL_NAME,
            ToolStatus::Success,
            "src/config/mod.rs (5 lines)",
            None,
            Some(ToolOutput::ReadCode {
                path: "src/config/mod.rs".into(),
                start_line: 1,
                lines: vec![
                    "use std::path::PathBuf;".into(),
                    "".into(),
                    "pub struct Config {".into(),
                    "    pub port: u16,".into(),
                    "}".into(),
                ],
            }),
        ),
        // #6 Edit — Success, Diff
        tool(
            "t_edit",
            EDIT_TOOL_NAME,
            ToolStatus::Success,
            "src/config/mod.rs",
            None,
            Some(ToolOutput::Diff {
                path: "src/config/mod.rs".into(),
                hunks: vec![DiffHunk {
                    start_line: 3,
                    lines: vec![
                        DiffLine::Removed(vec![DiffSpan::plain("pub struct Config {".into())]),
                        DiffLine::Added(vec![DiffSpan::plain("pub struct ConfigBuilder {".into())]),
                        DiffLine::Unchanged("    pub port: u16,".into()),
                        DiffLine::Added(vec![DiffSpan::plain("    pub host: String,".into())]),
                    ],
                }],
                summary: "Renamed Config to ConfigBuilder, added host field".into(),
            }),
        ),
        // #7 Write — Success, WriteCode
        tool(
            "t_write",
            WRITE_TOOL_NAME,
            ToolStatus::Success,
            "src/config/validation.rs (87 bytes)",
            None,
            Some(ToolOutput::WriteCode {
                path: "src/config/validation.rs".into(),
                byte_count: 87,
                lines: vec![
                    "pub fn validate_port(port: u16) -> bool {".into(),
                    "    port > 0 && port < 65535".into(),
                    "}".into(),
                ],
            }),
        ),
        // #8 Glob — Success, Plain, header+body
        tool(
            "t_glob",
            GLOB_TOOL_NAME,
            ToolStatus::Success,
            "**/*.rs (3 files)\nsrc/config/mod.rs\nsrc/config/builder.rs\nsrc/config/validation.rs",
            None,
            Some(ToolOutput::Plain(
                "src/config/mod.rs\nsrc/config/builder.rs\nsrc/config/validation.rs".into(),
            )),
        ),
        // #9 Grep — Success, GrepResult
        tool(
            "t_grep",
            GREP_TOOL_NAME,
            ToolStatus::Success,
            "ConfigBuilder",
            None,
            Some(ToolOutput::GrepResult {
                entries: vec![
                    GrepFileEntry {
                        path: "src/config/mod.rs".into(),
                        matches: vec![GrepMatch { line_nr: 3, text: "pub struct ConfigBuilder {".into() }],
                    },
                    GrepFileEntry {
                        path: "src/main.rs".into(),
                        matches: vec![GrepMatch { line_nr: 12, text: "use config::ConfigBuilder;".into() }],
                    },
                ],
            }),
        ),
        // #10 TodoWrite — Success, TodoList
        tool(
            "t_todo",
            TODOWRITE_TOOL_NAME,
            ToolStatus::Success,
            "Updated todo list",
            None,
            Some(ToolOutput::TodoList(vec![
                TodoItem { content: "Read existing config".into(), status: TodoStatus::Completed, priority: TodoPriority::High },
                TodoItem { content: "Create builder struct".into(), status: TodoStatus::Completed, priority: TodoPriority::High },
                TodoItem { content: "Add validation".into(), status: TodoStatus::InProgress, priority: TodoPriority::Medium },
                TodoItem { content: "Update tests".into(), status: TodoStatus::Pending, priority: TodoPriority::Low },
            ])),
        ),
        // #11 WebFetch — Success, Plain, header only (body hidden)
        tool(
            "t_web",
            WEBFETCH_TOOL_NAME,
            ToolStatus::Success,
            "https://docs.rs/config (42 lines)",
            None,
            Some(ToolOutput::Plain("Configuration crate docs content...".into())),
        ),
        // #12 Task — Success, Plain, header+body
        tool(
            "t_task",
            TASK_TOOL_NAME,
            ToolStatus::Success,
            "Explore config patterns\nFound 3 relevant patterns in the codebase:\n- Builder pattern in src/http/\n- Validation in src/auth/\n- Default impl in src/db/",
            None,
            Some(ToolOutput::Plain(
                "Found 3 relevant patterns in the codebase:\n- Builder pattern in src/http/\n- Validation in src/auth/\n- Default impl in src/db/".into(),
            )),
        ),
        // #13 Batch — Success, Batch
        tool(
            "t_batch",
            BATCH_TOOL_NAME,
            ToolStatus::Success,
            "Batch (3 tools)",
            None,
            Some(ToolOutput::Batch {
                entries: vec![
                    BatchToolEntry { tool: "read".into(), summary: "src/config/mod.rs".into(), is_error: false },
                    BatchToolEntry { tool: "read".into(), summary: "src/config/builder.rs".into(), is_error: false },
                    BatchToolEntry { tool: "read".into(), summary: "src/config/validation.rs".into(), is_error: false },
                ],
                text: String::new(),
            }),
        ),
        // #14 MultiEdit — Success, Diff
        tool(
            "t_multiedit",
            MULTIEDIT_TOOL_NAME,
            ToolStatus::Success,
            "src/main.rs",
            None,
            Some(ToolOutput::Diff {
                path: "src/main.rs".into(),
                hunks: vec![DiffHunk {
                    start_line: 1,
                    lines: vec![
                        DiffLine::Removed(vec![DiffSpan::plain("use config::Config;".into())]),
                        DiffLine::Added(vec![DiffSpan::plain("use config::ConfigBuilder;".into())]),
                    ],
                }],
                summary: "Updated import to use ConfigBuilder".into(),
            }),
        ),
        // #15 Bash — Error, Plain, header+stderr
        tool(
            "t_bash_err",
            BASH_TOOL_NAME,
            ToolStatus::Error,
            "cargo test (3 lines)\nerror[E0433]: failed to resolve: use of undeclared type `Config`\n  --> src/main.rs:15:5",
            Some(ToolInput::Code {
                language: "bash",
                code: "cargo test".into(),
            }),
            Some(ToolOutput::Plain(
                "error[E0433]: failed to resolve: use of undeclared type `Config`\n  --> src/main.rs:15:5".into(),
            )),
        ),
        // #16 Bash — InProgress (spinner animates)
        tool(
            "t_bash_ip",
            BASH_TOOL_NAME,
            ToolStatus::InProgress,
            "cargo build --release",
            Some(ToolInput::Code {
                language: "bash",
                code: "cargo build --release".into(),
            }),
            None,
        ),
        // #17 Error
        msg(DisplayRole::Error, "Connection timed out after 30s. Retrying..."),
        // #18 Assistant — final summary
        msg(DisplayRole::Assistant, "Done! The config module now uses a builder pattern with validation. All tests pass.\n\n**Changes:**\n- `ConfigBuilder` with `port()` and `host()` methods\n- `validate_port()` for input validation\n- Updated imports across the codebase"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn mock_data_invariants() {
        let msgs = mock_messages();
        let mut ids = HashSet::new();
        for msg in &msgs {
            if let DisplayRole::Tool { id, status, name } = &msg.role {
                assert!(ids.insert(id), "duplicate tool id: {id}");
                match status {
                    ToolStatus::Success | ToolStatus::Error => {
                        assert!(msg.tool_output.is_some(), "tool {name} missing output");
                    }
                    ToolStatus::InProgress => {
                        assert!(
                            msg.tool_output.is_none(),
                            "in-progress tool {name} has output"
                        );
                    }
                }
            }
        }
    }
}
