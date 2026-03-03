use std::fs;
use std::path::Path;

use maki_providers::{ToolInput, ToolOutput};
use maki_tool_macro::Tool;

use super::{MAX_OUTPUT_LINES, relative_path};

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
    pub const EXAMPLES: Option<&str> = None;

    fn write_output(&self) -> ToolOutput {
        ToolOutput::WriteCode {
            path: relative_path(&self.path),
            byte_count: self.content.len(),
            lines: self
                .content
                .lines()
                .take(MAX_OUTPUT_LINES)
                .map(ToOwned::to_owned)
                .collect(),
        }
    }

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        if let Some(parent) = Path::new(&self.path).parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {e}"))?;
        }
        fs::write(&self.path, &self.content).map_err(|e| format!("write error: {e}"))?;
        Ok(self.write_output())
    }

    pub fn start_summary(&self) -> String {
        relative_path(&self.path)
    }

    pub fn start_input(&self) -> Option<ToolInput> {
        None
    }

    pub fn start_output(&self) -> Option<ToolOutput> {
        Some(self.write_output())
    }

    pub fn mutable_path(&self) -> Option<&str> {
        Some(&self.path)
    }
}
