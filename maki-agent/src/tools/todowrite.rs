use maki_tool_macro::Tool;
use serde::Deserialize;

const MARKER_COMPLETED: &str = "[x]";
const MARKER_IN_PROGRESS: &str = "[>]";
const MARKER_PENDING: &str = "[ ]";
const MARKER_CANCELLED: &str = "[-]";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Deserialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
enum TodoPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Deserialize)]
struct TodoItem {
    content: String,
    status: TodoStatus,
    priority: TodoPriority,
}

impl TodoItem {
    fn item_schema() -> serde_json::Value {
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

#[derive(Tool, Debug, Clone)]
pub struct TodoWrite {
    #[param(description = "The updated todo list")]
    todos: Vec<TodoItem>,
}

impl TodoWrite {
    pub const NAME: &str = "todowrite";
    pub const DESCRIPTION: &str = include_str!("todowrite.md");

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<String, String> {
        if self.todos.is_empty() {
            return Ok("No todos.".to_string());
        }
        Ok(self
            .todos
            .iter()
            .map(|t| {
                let marker = match t.status {
                    TodoStatus::Completed => MARKER_COMPLETED,
                    TodoStatus::InProgress => MARKER_IN_PROGRESS,
                    TodoStatus::Pending => MARKER_PENDING,
                    TodoStatus::Cancelled => MARKER_CANCELLED,
                };
                format!("{marker} ({}) {}", t.priority, t.content)
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub fn start_summary(&self) -> String {
        format!("{} todos", self.todos.len())
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
}
