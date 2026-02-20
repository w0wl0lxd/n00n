use std::fs;

use maki_providers::{DiffHunk, DiffLine, ToolInput, ToolOutput};
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

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        if self.edits.is_empty() {
            return Err("provide at least one edit".into());
        }
        let mut content = fs::read_to_string(&self.path).map_err(|e| format!("read error: {e}"))?;

        let mut hunks = Vec::with_capacity(self.edits.len());
        for (i, edit) in self.edits.iter().enumerate() {
            let replace_all = edit.replace_all.unwrap_or(false);
            let result =
                fuzzy_replace::replace(&content, &edit.old_string, &edit.new_string, replace_all)
                    .map_err(|e| format!("edit {i}: {e}"))?;
            let start_line = content[..result.match_offset].matches('\n').count() + 1;
            hunks.push(build_hunk(start_line, &edit.old_string, &edit.new_string));
            content = result.content;
        }

        fs::write(&self.path, &content).map_err(|e| format!("write error: {e}"))?;
        let n = self.edits.len();
        Ok(ToolOutput::Diff {
            path: self.path.clone(),
            hunks,
            summary: format!(
                "applied {n} edit{s} to {path}",
                s = if n == 1 { "" } else { "s" },
                path = self.path
            ),
        })
    }

    pub fn start_summary(&self) -> String {
        self.path.clone()
    }

    pub fn start_input(&self) -> Option<ToolInput> {
        None
    }

    pub fn mutable_path(&self) -> Option<&str> {
        Some(&self.path)
    }
}

pub(super) fn build_hunk(start_line: usize, old: &str, new: &str) -> DiffHunk {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut diff_lines = Vec::new();

    let common_prefix = old_lines
        .iter()
        .zip(&new_lines)
        .take_while(|(a, b)| a == b)
        .count();
    let max_suffix = old_lines.len().min(new_lines.len()) - common_prefix;
    let common_suffix = old_lines
        .iter()
        .rev()
        .zip(new_lines.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();

    for &line in &old_lines[..common_prefix] {
        diff_lines.push(DiffLine::Unchanged(line.to_owned()));
    }
    for &line in &old_lines[common_prefix..old_lines.len() - common_suffix] {
        diff_lines.push(DiffLine::Removed(line.to_owned()));
    }
    for &line in &new_lines[common_prefix..new_lines.len() - common_suffix] {
        diff_lines.push(DiffLine::Added(line.to_owned()));
    }
    for &line in &old_lines[old_lines.len() - common_suffix..] {
        diff_lines.push(DiffLine::Unchanged(line.to_owned()));
    }

    DiffHunk {
        start_line,
        lines: diff_lines,
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
        let msg = tool.execute(&ctx).unwrap().as_text().to_string();
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

    #[test]
    fn build_hunk_unchanged_lines_not_duplicated() {
        let old = "pub const PLANS_DIR: &str = \"plans\";\n";
        let new = "pub const PLANS_DIR: &str = \"plans\";\npub const TEST: &str = \"test\";";
        let hunk = build_hunk(1, old, new);
        assert_eq!(hunk.start_line, 1);
        assert_eq!(
            hunk.lines,
            vec![
                DiffLine::Unchanged("pub const PLANS_DIR: &str = \"plans\";".into()),
                DiffLine::Added("pub const TEST: &str = \"test\";".into()),
            ]
        );
    }

    #[test]
    fn build_hunk_middle_change() {
        let old = "a\nb\nc";
        let new = "a\nB\nc";
        let hunk = build_hunk(5, old, new);
        assert_eq!(hunk.start_line, 5);
        assert_eq!(
            hunk.lines,
            vec![
                DiffLine::Unchanged("a".into()),
                DiffLine::Removed("b".into()),
                DiffLine::Added("B".into()),
                DiffLine::Unchanged("c".into()),
            ]
        );
    }
}
