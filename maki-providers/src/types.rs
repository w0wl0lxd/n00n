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
pub enum DiffLine {
    Unchanged(String),
    Added(String),
    Removed(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffHunk {
    pub start_line: usize,
    pub lines: Vec<DiffLine>,
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

#[derive(Debug, Clone, Serialize)]
pub enum ToolOutput {
    Plain(String),
    Diff {
        path: String,
        hunks: Vec<DiffHunk>,
        summary: String,
    },
    TodoList(Vec<TodoItem>),
}

impl ToolOutput {
    pub fn as_text(&self) -> String {
        match self {
            Self::Plain(s) => s.clone(),
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
                            DiffLine::Unchanged(l) => {
                                let _ = write!(out, "\n  {l}");
                            }
                            DiffLine::Removed(l) => {
                                let _ = write!(out, "\n- {l}");
                            }
                            DiffLine::Added(l) => {
                                let _ = write!(out, "\n+ {l}");
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
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDoneEvent {
    pub id: String,
    pub tool: &'static str,
    pub output: ToolOutput,
    pub is_error: bool,
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
    ToolDone(ToolDoneEvent),
    TurnComplete {
        message: Message,
        usage: TokenUsage,
        model: String,
    },
    ToolResultsSubmitted {
        message: Message,
    },
    Done {
        usage: TokenUsage,
        num_turns: u32,
        stop_reason: Option<String>,
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
}

impl From<AgentEvent> for Envelope {
    fn from(event: AgentEvent) -> Self {
        Self {
            event,
            parent_tool_use_id: None,
        }
    }
}

pub struct StreamResponse {
    pub message: Message,
    pub usage: TokenUsage,
    pub stop_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_text_diff_covers_all_line_types_and_multiple_hunks() {
        let output = ToolOutput::Diff {
            path: "src/main.rs".into(),
            hunks: vec![
                DiffHunk {
                    start_line: 1,
                    lines: vec![
                        DiffLine::Unchanged("keep".into()),
                        DiffLine::Removed("old".into()),
                        DiffLine::Added("new".into()),
                    ],
                },
                DiffHunk {
                    start_line: 10,
                    lines: vec![DiffLine::Removed("c".into()), DiffLine::Added("d".into())],
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
    fn as_text_todolist_empty() {
        let output = ToolOutput::TodoList(vec![]);
        assert_eq!(output.as_text(), "No todos.");
    }

    #[test]
    fn tool_result_is_error_skipped_when_false() {
        let ok = ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "ok".into(),
            is_error: false,
        };
        let ok_json: Value = serde_json::to_value(&ok).unwrap();
        assert!(ok_json.get("is_error").is_none());

        let err = ContentBlock::ToolResult {
            tool_use_id: "t2".into(),
            content: "fail".into(),
            is_error: true,
        };
        let err_json: Value = serde_json::to_value(&err).unwrap();
        assert_eq!(err_json["is_error"], true);
    }

    #[test]
    fn tool_results_maps_events_to_content_blocks() {
        let events = vec![ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain("fail".into()),
            is_error: true,
        }];
        let msg = Message::tool_results(events);
        assert!(matches!(msg.role, Role::User));
        match &msg.content[0] {
            ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => {
                assert_eq!(tool_use_id, "t1");
                assert!(is_error);
            }
            _ => panic!("expected ToolResult"),
        }
    }
}
