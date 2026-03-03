use maki_providers::{TodoItem, ToolInput, ToolOutput};
use maki_tool_macro::Tool;

#[derive(Tool, Debug, Clone)]
pub struct TodoWrite {
    #[param(description = "The updated todo list")]
    todos: Vec<TodoItem>,
}

impl TodoWrite {
    pub const NAME: &str = "todowrite";
    pub const DESCRIPTION: &str = include_str!("todowrite.md");
    pub const EXAMPLES: Option<&str> = None;

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        Ok(ToolOutput::TodoList(self.todos.clone()))
    }

    pub fn start_summary(&self) -> String {
        format!("{} todos", self.todos.len())
    }

    pub fn start_input(&self) -> Option<ToolInput> {
        None
    }

    pub fn start_output(&self) -> Option<ToolOutput> {
        None
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
}
