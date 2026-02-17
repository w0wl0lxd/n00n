use std::fs;

use maki_tool_macro::Tool;

use super::{MAX_OUTPUT_LINES, truncate_output};

#[derive(Tool, Debug, Clone)]
pub struct Read {
    #[param(description = "Absolute path to the file or directory")]
    path: String,
    #[param(description = "Line number to start from (1-indexed)")]
    offset: Option<usize>,
    #[param(description = "Max number of lines to read")]
    limit: Option<usize>,
}

impl Read {
    pub const NAME: &str = "read";
    pub const DESCRIPTION: &str = include_str!("read.md");

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<String, String> {
        let raw = fs::read_to_string(&self.path).map_err(|e| format!("read error: {e}"))?;

        let start = self.offset.unwrap_or(1).saturating_sub(1);
        let limit = self.limit.unwrap_or(MAX_OUTPUT_LINES);

        let numbered: String = raw
            .lines()
            .enumerate()
            .skip(start)
            .take(limit)
            .map(|(i, line)| format!("{}: {line}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(truncate_output(numbered))
    }

    pub fn start_summary(&self) -> String {
        self.path.clone()
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
}
