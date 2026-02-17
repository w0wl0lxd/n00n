use std::fs;
use std::path::Path;

use maki_tool_macro::Tool;

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

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<String, String> {
        if let Some(parent) = Path::new(&self.path).parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {e}"))?;
        }
        fs::write(&self.path, &self.content).map_err(|e| format!("write error: {e}"))?;
        Ok(format!(
            "wrote {} bytes to {}",
            self.content.len(),
            self.path
        ))
    }

    pub fn start_summary(&self) -> String {
        self.path.clone()
    }

    pub fn mutable_path(&self) -> Option<&str> {
        Some(&self.path)
    }
}
