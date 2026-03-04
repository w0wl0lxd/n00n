use std::fmt::Write;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::TokenUsage;

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiffSpan {
    pub text: String,
    pub emphasized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum DiffLine {
    Unchanged(String),
    Added(Vec<DiffSpan>),
    Removed(Vec<DiffSpan>),
}

#[derive(Debug, Clone, Serialize)]
pub struct GrepFileEntry {
    pub path: String,
    pub matches: Vec<GrepMatch>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GrepMatch {
    pub line_nr: usize,
    pub text: String,
}

impl DiffSpan {
    pub fn plain(text: String) -> Self {
        Self {
            text,
            emphasized: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffHunk {
    pub start_line: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

impl QuestionOption {
    pub fn item_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "label": { "type": "string", "description": "Option label" },
                "description": { "type": "string", "description": "Option description" }
            },
            "required": ["label"]
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionInfo {
    pub question: String,
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multiple: bool,
}

impl QuestionInfo {
    pub fn item_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "The question text" },
                "header": { "type": "string", "description": "Short tab header for the question" },
                "options": {
                    "type": "array",
                    "description": "List of predefined options",
                    "items": QuestionOption::item_schema()
                },
                "multiple": { "type": "boolean", "description": "Whether multiple options can be selected" }
            },
            "required": ["question"]
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub priority: TodoPriority,
}

impl TodoItem {
    pub fn item_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "Task description" },
                "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"] },
                "priority": { "type": "string", "enum": ["high", "medium", "low"] }
            },
            "required": ["content", "status", "priority"]
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl TodoStatus {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Completed => "[✓]",
            Self::InProgress => "[•]",
            Self::Pending => "[ ]",
            Self::Cancelled => "[x]",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum TodoPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ToolInput {
    Code {
        language: &'static str,
        code: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum BatchToolStatus {
    Pending,
    InProgress,
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchToolEntry {
    pub tool: String,
    pub summary: String,
    pub status: BatchToolStatus,
}

#[derive(Debug, Clone, Serialize)]
pub enum ToolOutput {
    Plain(String),
    ReadCode {
        path: String,
        start_line: usize,
        lines: Vec<String>,
    },
    Diff {
        path: String,
        hunks: Vec<DiffHunk>,
        summary: String,
    },
    TodoList(Vec<TodoItem>),
    WriteCode {
        path: String,
        byte_count: usize,
        lines: Vec<String>,
    },
    GrepResult {
        entries: Vec<GrepFileEntry>,
    },
    Batch {
        entries: Vec<BatchToolEntry>,
        text: String,
    },
}

impl ToolOutput {
    pub fn as_text(&self) -> String {
        match self {
            Self::Plain(s) => s.clone(),
            Self::ReadCode {
                start_line, lines, ..
            } => lines
                .iter()
                .enumerate()
                .map(|(i, line)| format!("{}: {line}", start_line + i))
                .collect::<Vec<_>>()
                .join("\n"),
            Self::Diff {
                path,
                hunks,
                summary,
            } => {
                let mut out = format!("{summary}\n--- {path}\n+++ {path}");
                for hunk in hunks {
                    out.push('\n');
                    for dl in &hunk.lines {
                        match dl {
                            DiffLine::Unchanged(t) => {
                                let _ = write!(out, "\n  {t}");
                            }
                            DiffLine::Removed(spans) | DiffLine::Added(spans) => {
                                let prefix = if matches!(dl, DiffLine::Removed(_)) {
                                    "- "
                                } else {
                                    "+ "
                                };
                                let _ = write!(out, "\n{prefix}");
                                for s in spans {
                                    out.push_str(&s.text);
                                }
                            }
                        }
                    }
                }
                out
            }
            Self::TodoList(items) => {
                if items.is_empty() {
                    return "No todos.".into();
                }
                items
                    .iter()
                    .map(|t| format!("{} ({}) {}", t.status.marker(), t.priority, t.content))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            Self::WriteCode {
                path, byte_count, ..
            } => format!("wrote {byte_count} bytes to {path}"),
            Self::GrepResult { entries } => {
                let mut out = String::new();
                for entry in entries {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&entry.path);
                    out.push(':');
                    for m in &entry.matches {
                        out.push_str(&format!("\n  {}: {}", m.line_nr, m.text));
                    }
                }
                out
            }
            Self::Batch { text, .. } => text.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: String) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text }],
        }
    }

    pub fn tool_results(results: Vec<ToolDoneEvent>) -> Self {
        Self {
            role: Role::User,
            content: results
                .into_iter()
                .map(|r| ContentBlock::ToolResult {
                    tool_use_id: r.id,
                    content: r.output.as_text(),
                    is_error: r.is_error,
                })
                .collect(),
        }
    }

    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }

    pub fn has_tool_calls(&self) -> bool {
        self.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolStartEvent {
    pub id: String,
    pub tool: &'static str,
    pub summary: String,
    pub input: Option<ToolInput>,
    pub output: Option<ToolOutput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDoneEvent {
    pub id: String,
    pub tool: &'static str,
    pub output: ToolOutput,
    pub is_error: bool,
}

impl ToolDoneEvent {
    pub fn error(id: String, message: impl Into<String>) -> Self {
        Self {
            id,
            tool: "unknown",
            output: ToolOutput::Plain(message.into()),
            is_error: true,
        }
    }

    pub fn written_path(&self) -> Option<&str> {
        if self.is_error || self.tool != "write" {
            return None;
        }
        match &self.output {
            ToolOutput::WriteCode { path, .. } => Some(path),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolStart(ToolStartEvent),
    ToolOutput {
        id: String,
        content: String,
    },
    ToolDone(ToolDoneEvent),
    BatchProgress {
        batch_id: String,
        index: usize,
        status: BatchToolStatus,
    },
    TurnComplete {
        message: Message,
        usage: TokenUsage,
        model: String,
    },
    ToolResultsSubmitted {
        message: Message,
    },
    QuestionPrompt {
        id: String,
        questions: Vec<QuestionInfo>,
    },
    InterruptConsumed {
        message: String,
    },
    Done {
        usage: TokenUsage,
        num_turns: u32,
        stop_reason: Option<String>,
    },
    Retry {
        attempt: u32,
        message: String,
        delay_ms: u64,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Serialize)]
pub struct Envelope {
    #[serde(flatten)]
    pub event: AgentEvent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_prompt: Option<String>,
}

impl From<AgentEvent> for Envelope {
    fn from(event: AgentEvent) -> Self {
        Self {
            event,
            parent_tool_use_id: None,
            parent_name: None,
            parent_prompt: None,
        }
    }
}

#[derive(Debug)]
pub struct StreamResponse {
    pub message: Message,
    pub usage: TokenUsage,
    pub stop_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test]
    fn as_text_diff_covers_all_line_types_and_multiple_hunks() {
        let output = ToolOutput::Diff {
            path: "src/main.rs".into(),
            hunks: vec![
                DiffHunk {
                    start_line: 1,
                    lines: vec![
                        DiffLine::Unchanged("keep".into()),
                        DiffLine::Removed(vec![DiffSpan::plain("old".into())]),
                        DiffLine::Added(vec![DiffSpan::plain("new".into())]),
                    ],
                },
                DiffHunk {
                    start_line: 10,
                    lines: vec![
                        DiffLine::Removed(vec![DiffSpan::plain("c".into())]),
                        DiffLine::Added(vec![DiffSpan::plain("d".into())]),
                    ],
                },
            ],
            summary: "Updated value".into(),
        };
        let text = output.as_text();
        assert!(text.starts_with("Updated value"));
        assert!(text.contains("--- src/main.rs"));
        assert!(text.contains("+++ src/main.rs"));
        assert!(text.contains("  keep"));
        assert!(text.contains("- old"));
        assert!(text.contains("+ new"));
        assert!(text.contains("- c"));
        assert!(text.contains("+ d"));
    }

    #[test]
    fn as_text_todolist_formats_all_statuses() {
        let output = ToolOutput::TodoList(vec![
            TodoItem {
                content: "done".into(),
                status: TodoStatus::Completed,
                priority: TodoPriority::High,
            },
            TodoItem {
                content: "wip".into(),
                status: TodoStatus::InProgress,
                priority: TodoPriority::Medium,
            },
            TodoItem {
                content: "todo".into(),
                status: TodoStatus::Pending,
                priority: TodoPriority::Low,
            },
            TodoItem {
                content: "nope".into(),
                status: TodoStatus::Cancelled,
                priority: TodoPriority::Low,
            },
        ]);
        let text = output.as_text();
        assert!(text.contains("[✓] (high) done"));
        assert!(text.contains("[•] (medium) wip"));
        assert!(text.contains("[ ] (low) todo"));
        assert!(text.contains("[x] (low) nope"));
    }

    #[test]
    fn as_text_grep_result_multi_file() {
        let output = ToolOutput::GrepResult {
            entries: vec![
                GrepFileEntry {
                    path: "src/a.rs".into(),
                    matches: vec![
                        GrepMatch {
                            line_nr: 3,
                            text: "fn foo()".into(),
                        },
                        GrepMatch {
                            line_nr: 10,
                            text: "fn bar()".into(),
                        },
                    ],
                },
                GrepFileEntry {
                    path: "src/b.rs".into(),
                    matches: vec![GrepMatch {
                        line_nr: 1,
                        text: "use crate".into(),
                    }],
                },
            ],
        };
        let text = output.as_text();
        assert!(text.contains("src/a.rs"));
        assert!(text.contains("3: fn foo()"));
        assert!(text.contains("10: fn bar()"));
        assert!(text.contains("src/b.rs"));
        assert!(text.contains("1: use crate"));
    }

    #[test_case("write", false, Some("src/lib.rs") ; "success")]
    #[test_case("write", true,  None                 ; "error")]
    #[test_case("bash",  false, None                 ; "non_write_tool")]
    fn written_path_cases(tool: &'static str, is_error: bool, expected: Option<&str>) {
        let output = if tool == "write" {
            ToolOutput::WriteCode {
                path: "src/lib.rs".into(),
                byte_count: 10,
                lines: vec![],
            }
        } else {
            ToolOutput::Plain("ok".into())
        };
        let event = ToolDoneEvent {
            id: "id".into(),
            tool,
            output,
            is_error,
        };
        assert_eq!(event.written_path(), expected);
    }
}
