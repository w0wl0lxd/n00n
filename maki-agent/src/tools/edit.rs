use std::fs;

use maki_tool_macro::Tool;
use serde_json::Value;

const NO_MATCH: &str = "old_string not found in file";
const MULTIPLE_MATCHES: &str =
    "old_string matches multiple locations; add surrounding context to make it unique";

#[derive(Tool, Debug, Clone)]
pub struct Edit {
    #[param(description = "Absolute path to the file")]
    path: String,
    #[param(description = "Exact string to find (must match uniquely unless replace_all is true)")]
    old_string: String,
    #[param(description = "Replacement string")]
    new_string: String,
    #[param(description = "Replace all occurrences (default false)")]
    replace_all: Option<bool>,
}

impl Edit {
    pub const NAME: &str = "edit";
    pub const DESCRIPTION: &str = include_str!("edit.md");

    pub fn execute(&self) -> Result<String, String> {
        let content = fs::read_to_string(&self.path).map_err(|e| format!("read error: {e}"))?;
        let count = content.matches(self.old_string.as_str()).count();
        if count == 0 {
            return Err(NO_MATCH.into());
        }
        let replace_all = self.replace_all.unwrap_or(false);
        if !replace_all && count > 1 {
            return Err(MULTIPLE_MATCHES.into());
        }
        let updated = content.replace(self.old_string.as_str(), &self.new_string);
        fs::write(&self.path, &updated).map_err(|e| format!("write error: {e}"))?;
        Ok(format!(
            "edited {} ({count} occurrence{s})",
            self.path,
            s = if count == 1 { "" } else { "s" }
        ))
    }

    pub fn start_summary(&self) -> String {
        self.path.clone()
    }

    pub fn mutable_path(&self) -> Option<&str> {
        Some(&self.path)
    }

    pub fn scrub_input(input: &mut Value) {
        for key in &["old_string", "new_string"] {
            if let Some(v) = input.get(*key).and_then(|v| v.as_str()) {
                input[*key] = Value::String(format!("[{} bytes]", v.len()));
            }
        }
    }

    pub fn scrub_result(_content: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn temp_file(dir: &TempDir, name: &str, content: &str) -> String {
        let path = dir.path().join(name);
        fs::write(&path, content).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn edit_unique_and_replace_all() {
        let dir = TempDir::new().unwrap();
        let path = temp_file(&dir, "f.rs", "fn old() {}\nfn keep() {}");
        Edit {
            path: path.clone(),
            old_string: "fn old() {}".into(),
            new_string: "fn new() {}".into(),
            replace_all: None,
        }
        .execute()
        .unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "fn new() {}\nfn keep() {}"
        );

        let path = temp_file(&dir, "g.rs", "let x = 1;\nlet x = 1;\nlet y = 2;");
        let msg = Edit {
            path: path.clone(),
            old_string: "let x = 1;".into(),
            new_string: "let x = 9;".into(),
            replace_all: Some(true),
        }
        .execute()
        .unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "let x = 9;\nlet x = 9;\nlet y = 2;"
        );
        assert!(msg.contains("2 occurrence"));
    }

    #[test]
    fn edit_rejects_no_match_and_ambiguous() {
        let dir = TempDir::new().unwrap();
        let path = temp_file(&dir, "f.rs", "let x = 1;\nlet x = 1;");
        assert_eq!(
            Edit {
                path: path.clone(),
                old_string: "NOPE".into(),
                new_string: "b".into(),
                replace_all: None,
            }
            .execute()
            .unwrap_err(),
            NO_MATCH
        );
        assert_eq!(
            Edit {
                path,
                old_string: "let x = 1;".into(),
                new_string: "let x = 2;".into(),
                replace_all: None,
            }
            .execute()
            .unwrap_err(),
            MULTIPLE_MATCHES
        );
    }
}
