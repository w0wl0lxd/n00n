use std::path::Path;

use crate::ToolOutput;
use maki_code_index::{IndexError, index_file};
use maki_tool_macro::Tool;

use super::relative_path;

#[derive(Tool, Debug, Clone)]
pub struct Index {
    #[param(description = "Absolute path to the file")]
    path: String,
}

impl Index {
    pub const NAME: &str = "index";
    pub const DESCRIPTION: &str = include_str!("index.md");
    pub const EXAMPLES: Option<&str> = Some(r#"[{"path": "/home/user/project/src/main.rs"}]"#);

    pub async fn execute(&self, _ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        let path = self.path.clone();
        smol::unblock(move || {
            let p = Path::new(&path);
            match index_file(p) {
                Ok(skeleton) => Ok(ToolOutput::Plain(skeleton)),
                Err(IndexError::UnsupportedLanguage(ext)) => Err(format!(
                    "Unsupported file type: {ext}. Use the read tool instead."
                )),
                Err(IndexError::FileTooLarge { size, max }) => Err(format!(
                    "File too large ({size} bytes, max {max}). Use read with offset/limit instead."
                )),
                Err(e) => Err(format!("{e}. Use the read tool instead.")),
            }
        })
        .await
    }

    pub fn start_summary(&self) -> String {
        relative_path(&self.path)
    }
}

impl super::ToolDefaults for Index {}
