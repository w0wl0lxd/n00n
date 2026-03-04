use maki_agent::tools::{
    BASH_TOOL_NAME, BATCH_TOOL_NAME, EDIT_TOOL_NAME, GLOB_TOOL_NAME, GREP_TOOL_NAME,
    MULTIEDIT_TOOL_NAME, QUESTION_TOOL_NAME, READ_TOOL_NAME, TASK_TOOL_NAME, TODOWRITE_TOOL_NAME,
    WEBFETCH_TOOL_NAME, WRITE_TOOL_NAME,
};
use maki_providers::{
    BatchToolEntry, BatchToolStatus, DiffHunk, DiffLine, DiffSpan, GrepFileEntry, GrepMatch,
    QuestionInfo, QuestionOption, TodoItem, TodoPriority, TodoStatus, ToolInput, ToolOutput,
};

use crate::components::{DisplayMessage, DisplayRole, ToolStatus};

fn msg(role: DisplayRole, text: &str) -> DisplayMessage {
    DisplayMessage::new(role, text.into())
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
        plan_path: None,
        timestamp: None,
    }
}

pub const MOCK_TASK_TOOL_ID: &str = "t_task";
pub const MOCK_QUESTION_TOOL_ID: &str = "t_qform";

pub fn mock_questions() -> Vec<QuestionInfo> {
    vec![
        QuestionInfo {
            question: "What language do you want to use?".into(),
            header: "Language".into(),
            options: vec![
                QuestionOption {
                    label: "TypeScript".into(),
                    description: "Popular for web".into(),
                },
                QuestionOption {
                    label: "Rust".into(),
                    description: "Fast and safe".into(),
                },
                QuestionOption {
                    label: "Go".into(),
                    description: "Simple concurrency".into(),
                },
            ],
            multiple: false,
        },
        QuestionInfo {
            question: "Which framework do you prefer?".into(),
            header: "Framework".into(),
            options: vec![
                QuestionOption {
                    label: "Next.js".into(),
                    description: "React SSR".into(),
                },
                QuestionOption {
                    label: "tRPC".into(),
                    description: "End-to-end typesafe".into(),
                },
                QuestionOption {
                    label: "SvelteKit".into(),
                    description: "Compiler-based".into(),
                },
            ],
            multiple: true,
        },
        QuestionInfo {
            question: "What database should we use?".into(),
            header: "Database".into(),
            options: vec![
                QuestionOption {
                    label: "PostgreSQL".into(),
                    description: "Relational".into(),
                },
                QuestionOption {
                    label: "SQLite".into(),
                    description: "Embedded".into(),
                },
            ],
            multiple: false,
        },
    ]
}

pub fn mock_question_messages() -> Vec<DisplayMessage> {
    vec![
        msg(DisplayRole::User, "Help me set up a new web project."),
        msg(
            DisplayRole::Thinking,
            "I need to ask the user about their preferences before scaffolding the project.",
        ),
        tool(
            MOCK_QUESTION_TOOL_ID,
            QUESTION_TOOL_NAME,
            ToolStatus::InProgress,
            "3 questions",
            None,
            None,
        ),
    ]
}

pub fn mock_subagent_messages() -> Vec<DisplayMessage> {
    vec![
        msg(
            DisplayRole::User,
            "Search the codebase for existing builder patterns and validation approaches. Return file paths and a summary of how they are implemented.",
        ),
        msg(
            DisplayRole::Thinking,
            "The user wants me to explore config patterns in the codebase. Let me search for existing builder patterns and validation approaches.",
        ),
        tool(
            "s_grep1",
            GREP_TOOL_NAME,
            ToolStatus::Success,
            "\\bBuilder\\b [*.rs]",
            None,
            Some(ToolOutput::GrepResult {
                entries: vec![
                    GrepFileEntry {
                        path: "src/http/client.rs".into(),
                        matches: vec![
                            GrepMatch {
                                line_nr: 22,
                                text: "pub struct ClientBuilder {".into(),
                            },
                            GrepMatch {
                                line_nr: 45,
                                text: "impl ClientBuilder {".into(),
                            },
                        ],
                    },
                    GrepFileEntry {
                        path: "src/db/pool.rs".into(),
                        matches: vec![GrepMatch {
                            line_nr: 8,
                            text: "pub struct PoolBuilder {".into(),
                        }],
                    },
                ],
            }),
        ),
        tool(
            "s_read1",
            READ_TOOL_NAME,
            ToolStatus::Success,
            "src/http/client.rs (12 lines)",
            None,
            Some(ToolOutput::ReadCode {
                path: "src/http/client.rs".into(),
                start_line: 22,
                lines: vec![
                    "pub struct ClientBuilder {".into(),
                    "    timeout: Option<Duration>,".into(),
                    "    retries: u32,".into(),
                    "    base_url: String,".into(),
                    "}".into(),
                    "".into(),
                    "impl ClientBuilder {".into(),
                    "    pub fn new(base_url: impl Into<String>) -> Self {".into(),
                    "        Self { timeout: None, retries: 3, base_url: base_url.into() }".into(),
                    "    }".into(),
                    "".into(),
                    "    pub fn build(self) -> Result<Client, ConfigError> {".into(),
                ],
            }),
        ),
        tool(
            "s_grep2",
            GREP_TOOL_NAME,
            ToolStatus::Success,
            "validate [*.rs] in src/",
            None,
            Some(ToolOutput::GrepResult {
                entries: vec![GrepFileEntry {
                    path: "src/auth/token.rs".into(),
                    matches: vec![GrepMatch {
                        line_nr: 31,
                        text: "fn validate_token(token: &str) -> Result<Claims> {".into(),
                    }],
                }],
            }),
        ),
        tool(
            "s_read2",
            READ_TOOL_NAME,
            ToolStatus::Success,
            "src/db/pool.rs (8 lines)",
            None,
            Some(ToolOutput::ReadCode {
                path: "src/db/pool.rs".into(),
                start_line: 1,
                lines: vec![
                    "use std::time::Duration;".into(),
                    "".into(),
                    "pub struct PoolBuilder {".into(),
                    "    max_connections: u32,".into(),
                    "    idle_timeout: Duration,".into(),
                    "}".into(),
                    "".into(),
                    "impl Default for PoolBuilder {".into(),
                ],
            }),
        ),
        msg(
            DisplayRole::Assistant,
            concat!(
                "Found 3 relevant patterns in the codebase:\n",
                "\n",
                "- **Builder pattern** in `src/http/client.rs` — uses `ClientBuilder` with fluent setters and a `build()` that returns `Result<Client, ConfigError>`\n",
                "- **Validation** in `src/auth/token.rs` — `validate_token()` returns `Result<Claims>` with descriptive errors\n",
                "- **Default impl** in `src/db/pool.rs` — `PoolBuilder` implements `Default` for sensible defaults",
            ),
        ),
    ]
}

pub fn mock_messages() -> Vec<DisplayMessage> {
    vec![
        // User
        msg(DisplayRole::User, "Refactor the config module to use builder pattern and add validation."),
        // Thinking
        msg(DisplayRole::Thinking, "Let me analyze the config module structure. I'll need to look at the existing implementation, understand the current API surface, and plan the refactor to use a builder pattern with proper validation."),
        // Assistant (rich markdown)
        msg(DisplayRole::Assistant, concat!(
            "I'll refactor the config module. Let me start by reading the current implementation.\n",
            "\n",
            "## Plan\n",
            "\n",
            "1. Read existing `Config` struct and *understand* the current API\n",
            "2. Create **`ConfigBuilder`** with a ***fluent interface***\n",
            "3. Add validation - ~~manual checks~~ replaced with `validate()` method\n",
            "4. Update tests\n",
            "   - Unit tests for _builder methods_\n",
            "   - Integration tests for **validation rules**\n",
            "\n",
            "### Current structure\n",
            "\n",
            "The `Config` struct in ``src/config/mod.rs`` is straightforward:\n",
            "\n",
            "```rust\n",
            "pub struct Config {\n",
            "    pub port: u16,\n",
            "    pub host: String,\n",
            "    pub workers: Option<usize>,\n",
            "}\n",
            "```\n",
            "\n",
            "I'll transform this into a *builder* with **compile-time** guarantees.",
        )),
        // Bash - Success, Plain, header+body
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
        // Read - Success, ReadCode
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
        // Edit - Success, Diff
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
        // Write - Success, WriteCode
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
        // Glob - Success, Plain, header+body
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
        // Grep - Success, GrepResult (pattern + filter + path)
        tool(
            "t_grep",
            GREP_TOOL_NAME,
            ToolStatus::Success,
            "\\b(Config|Builder)\\b [*.rs] in src/config/",
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
        // TodoWrite - Success, TodoList
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
        // WebFetch - Success, Plain, header only (body hidden)
        tool(
            "t_web",
            WEBFETCH_TOOL_NAME,
            ToolStatus::Success,
            "https://docs.rs/config (42 lines)",
            None,
            Some(ToolOutput::Plain("Configuration crate docs content...".into())),
        ),
        // Task - Success, Plain, header+body
        tool(
            MOCK_TASK_TOOL_ID,
            TASK_TOOL_NAME,
            ToolStatus::Success,
            "Explore config patterns: Found 3 relevant patterns in the codebase:\n- Builder pattern in src/http/\n- Validation in src/auth/\n- Default impl in src/db/",
            None,
            Some(ToolOutput::Plain(
                "Found 3 relevant patterns in the codebase:\n- Builder pattern in src/http/\n- Validation in src/auth/\n- Default impl in src/db/".into(),
            )),
        ),
        // Batch - Success, Batch
        tool(
            "t_batch",
            BATCH_TOOL_NAME,
            ToolStatus::Success,
            "Batch (3 tools)",
            None,
            Some(ToolOutput::Batch {
                entries: vec![
                    BatchToolEntry { tool: "read".into(), summary: "src/config/mod.rs".into(), status: BatchToolStatus::Success },
                    BatchToolEntry { tool: "read".into(), summary: "src/config/builder.rs".into(), status: BatchToolStatus::Success },
                    BatchToolEntry { tool: "read".into(), summary: "src/config/validation.rs".into(), status: BatchToolStatus::Success },
                ],
                text: String::new(),
            }),
        ),
        // Question - Success, Plain
        tool(
            "t_question",
            QUESTION_TOOL_NAME,
            ToolStatus::Success,
            "2 questions",
            None,
            Some(ToolOutput::Plain(
                "What testing framework do you prefer?
\
                 Should I add integration tests as well?".into(),
            )),
        ),
        // MultiEdit - Success, Diff
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
        // Bash - Error, Plain, header+stderr
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
        // Bash - InProgress (spinner animates)
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
        // Error
        msg(DisplayRole::Error, "Connection timed out after 30s. Retrying..."),
        // Assistant - plain code block (no language)
        msg(DisplayRole::Assistant, concat!(
            "Here's what the output looks like:\n",
            "\n",
            "```\n",
            "$ cargo test\n",
            "running 396 tests\n",
            "test result: ok. 396 passed; 0 failed\n",
            "```\n",
            "\n",
            "All good.",
        )),
        // Assistant - final summary
        msg(DisplayRole::Assistant, concat!(
            "Done! The config module now uses a ***builder pattern*** with validation.\n",
            "\n",
            "## Summary\n",
            "\n",
            "**Changes:**\n",
            "- `ConfigBuilder` with `port()` and `host()` methods\n",
            "- ~~`Config::new()`~~ replaced with `ConfigBuilder::default().build()`\n",
            "- _Validation_ via `validate_port()` - rejects ports **outside** `1..=65534`\n",
            "  - Returns `Result<Config, ConfigError>` instead of *panicking*\n",
            "\n",
            "| File | Change | Lines |\n",
            "| --- | --- | --- |\n",
            "| `mod.rs` | Renamed struct | +2 / -1 |\n",
            "| `builder.rs` | New builder impl | +45 |\n",
            "| `validation.rs` | New validation | +12 |\n",
            "| `main.rs` | Updated imports | +1 / -1 |\n",
            "\n",
            "---\n",
            "\n",
            "### Before / After\n",
            "\n",
            "```rust\n",
            "// Before\n",
            "let cfg = Config { port: 8080, host: \"localhost\".into() };\n",
            "\n",
            "// After - this is an intentionally very long line to test horizontal wrapping behavior in the UI: ConfigBuilder::default().port(8080).host(\"localhost\").workers(num_cpus::get()).timeout(Duration::from_secs(30)).max_retries(3).backoff_strategy(ExponentialBackoff::new()).enable_tls(true).tls_cert_path(\"/etc/ssl/certs/server.pem\").build()\n",
            "// After\n",
            "let cfg = ConfigBuilder::default()\n",
            "    .port(8080)\n",
            "    .host(\"localhost\")\n",
            "    .build()?;\n",
            "```\n",
            "\n",
            "All **396** tests pass. Run `cargo test` to verify.",
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn mock_data_invariants() {
        check_invariants(&mock_messages());
    }

    #[test]
    fn mock_question_data_invariants() {
        check_invariants(&mock_question_messages());
    }

    #[test]
    fn mock_subagent_data_invariants() {
        check_invariants(&mock_subagent_messages());
    }

    fn check_invariants(msgs: &[DisplayMessage]) {
        let mut ids = HashSet::new();
        for msg in msgs {
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
