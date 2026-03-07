use super::Tool;
use crate::{TodoItem, ToolOutput};
use maki_tool_macro::Tool;

#[derive(Tool, Debug, Clone)]
pub struct TodoWrite {
    #[param(description = "The updated todo list")]
    todos: Vec<TodoItem>,
}

impl Tool for TodoWrite {
    const NAME: &str = "todowrite";
    const DESCRIPTION: &str = include_str!("todowrite.md");

    fn execute(&self, _ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        Ok(ToolOutput::TodoList(self.todos.clone()))
    }

    fn start_summary(&self) -> String {
        format!("{} todos", self.todos.len())
    }
}
