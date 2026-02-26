use std::fs;
use std::path::Path;

use maki_providers::{ToolInput, ToolOutput};
use maki_tool_macro::Tool;

use super::{relative_path, truncate_line};

const PREVIEW_LINES: usize = 30;

#[derive(Tool, Debug, Clone)]
pub struct Write {
    #[param(description = "Absolute path to the file")]
    path: String,
    #[param(description = "The complete file content to write")]
    content: String,
}

impl Write {
    pub const NAME: &str = "write";
    pub const DESCRIPTION: &str = include_str!("write.md");

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        if let Some(parent) = Path::new(&self.path).parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {e}"))?;
        }
        fs::write(&self.path, &self.content).map_err(|e| format!("write error: {e}"))?;
        let path = relative_path(&self.path);
        let lines: Vec<String> = self
            .content
            .lines()
            .take(PREVIEW_LINES)
            .map(truncate_line)
            .collect();
        Ok(ToolOutput::WriteCode {
            path,
            byte_count: self.content.len(),
            lines,
        })
    }

    pub fn start_summary(&self) -> String {
        relative_path(&self.path)
    }

    pub fn start_input(&self) -> Option<ToolInput> {
        None
    }

    pub fn mutable_path(&self) -> Option<&str> {
        Some(&self.path)
    }
}
