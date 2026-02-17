use std::fs;

use serde::Deserialize;
use serde_json::Value;

use maki_tool_macro::Tool;

use super::fuzzy_replace;

#[derive(Debug, Clone, Deserialize)]
struct EditEntry {
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
}

impl EditEntry {
    fn item_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "old_string": { "type": "string", "description": "Exact string to find" },
                "new_string": { "type": "string", "description": "Replacement string" },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)" }
            },
            "required": ["old_string", "new_string"]
        })
    }
}

#[derive(Tool, Debug, Clone)]
pub struct MultiEdit {
    #[param(description = "Absolute path to the file")]
    path: String,
    #[param(description = "Array of edit operations to apply sequentially")]
    edits: Vec<EditEntry>,
}

impl MultiEdit {
    pub const NAME: &str = "multiedit";
    pub const DESCRIPTION: &str = include_str!("multiedit.md");

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<String, String> {
        if self.edits.is_empty() {
            return Err("provide at least one edit".into());
        }
        let mut content = fs::read_to_string(&self.path).map_err(|e| format!("read error: {e}"))?;

        for (i, edit) in self.edits.iter().enumerate() {
            let replace_all = edit.replace_all.unwrap_or(false);
            content =
                fuzzy_replace::replace(&content, &edit.old_string, &edit.new_string, replace_all)
                    .map_err(|e| format!("edit {i}: {e}"))?;
        }

        fs::write(&self.path, &content).map_err(|e| format!("write error: {e}"))?;
        let n = self.edits.len();
        Ok(format!(
            "applied {n} edit{s} to {path}",
            s = if n == 1 { "" } else { "s" },
            path = self.path
        ))
    }

    pub fn start_summary(&self) -> String {
        self.path.clone()
    }

    pub fn mutable_path(&self) -> Option<&str> {
        Some(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use crate::AgentMode;
    use crate::tools::test_support::stub_ctx;

    use super::*;

    const EMPTY_ERR: &str = "provide at least one edit";

    fn temp_file(dir: &TempDir, name: &str, content: &str) -> String {
        let path = dir.path().join(name);
        fs::write(&path, content).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn sequential_edits_compose() {
        let dir = TempDir::new().unwrap();
        let ctx = stub_ctx(&AgentMode::Build);
        let path = temp_file(&dir, "f.rs", "fn alpha() {}\nfn beta() {}");
        let tool = MultiEdit::parse_input(&json!({
            "path": path,
            "edits": [
                { "old_string": "fn alpha() {}", "new_string": "fn one() {}" },
                { "old_string": "fn beta() {}", "new_string": "fn two() {}" }
            ]
        }))
        .unwrap();
        let msg = tool.execute(&ctx).unwrap();
        assert!(msg.contains("2 edits"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "fn one() {}\nfn two() {}"
        );
    }

    #[test]
    fn failure_leaves_file_unchanged() {
        let dir = TempDir::new().unwrap();
        let ctx = stub_ctx(&AgentMode::Build);
        let original = "let a = 1;\nlet b = 2;";
        let path = temp_file(&dir, "f.rs", original);
        let tool = MultiEdit::parse_input(&json!({
            "path": path,
            "edits": [
                { "old_string": "let a = 1;", "new_string": "let a = 9;" },
                { "old_string": "MISSING", "new_string": "x" }
            ]
        }))
        .unwrap();
        let err = tool.execute(&ctx).unwrap_err();
        assert!(err.contains("edit 1"));
        assert!(err.contains(fuzzy_replace::NO_MATCH));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn empty_edits_rejected() {
        let dir = TempDir::new().unwrap();
        let ctx = stub_ctx(&AgentMode::Build);
        let path = temp_file(&dir, "f.rs", "content");
        let tool = MultiEdit::parse_input(&json!({ "path": path, "edits": [] })).unwrap();
        assert_eq!(tool.execute(&ctx).unwrap_err(), EMPTY_ERR);
    }
}
