mod bash;
mod edit;
mod glob;
mod grep;
mod read;
mod todowrite;
mod webfetch;
mod write;

use std::path::Path;
use std::time::SystemTime;

use serde_json::{Value, json};

use crate::{AgentError, AgentMode, ToolDoneEvent, ToolStartEvent};

const PLAN_WRITE_RESTRICTED: &str = "write restricted to plan file in plan mode";
const MAX_OUTPUT_BYTES: usize = 30_000;
pub(crate) const MAX_OUTPUT_LINES: usize = 2000;
pub(crate) const SEARCH_RESULT_LIMIT: usize = 100;
pub(crate) const NO_FILES_FOUND: &str = "No files found";
pub const WEBFETCH_TOOL_NAME: &str = webfetch::WebFetch::NAME;

pub(crate) fn resolve_search_path(path: Option<&str>) -> Result<String, String> {
    match path {
        Some(p) => Ok(p.to_string()),
        None => std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| format!("cwd error: {e}")),
    }
}

pub(crate) fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

pub(crate) fn truncate_output(text: String) -> String {
    const TRUNCATED_MARKER: &str = "[truncated]";
    let mut lines = text.lines();
    let mut result = String::new();
    let mut truncated = false;

    for _ in 0..MAX_OUTPUT_LINES {
        let Some(line) = lines.next() else { break };
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(line);
        if result.len() > MAX_OUTPUT_BYTES {
            result.truncate(MAX_OUTPUT_BYTES);
            truncated = true;
            break;
        }
    }

    if !truncated && lines.next().is_some() {
        truncated = true;
    }

    if truncated {
        result.push('\n');
        result.push_str(TRUNCATED_MARKER);
    }
    result
}

macro_rules! register_tools {
    ($($Variant:ident($inner:path)),+ $(,)?) => {
        #[derive(Debug, Clone)]
        pub enum ToolCall {
            $($Variant($inner)),+
        }

        impl ToolCall {
            pub fn from_api(name: &str, input: &Value) -> Result<Self, AgentError> {
                match name {
                    $(<$inner>::NAME => {
                        <$inner>::parse_input(input)
                            .map(ToolCall::$Variant)
                            .map_err(|msg| AgentError::Tool { tool: name.to_string(), message: msg })
                    })+
                    _ => Err(AgentError::Tool {
                        tool: name.to_string(),
                        message: format!("unknown variant `{name}`"),
                    })
                }
            }

            pub fn name(&self) -> &'static str {
                match self {
                    $(ToolCall::$Variant(_) => <$inner>::NAME),+
                }
            }

            pub fn start_event(&self) -> ToolStartEvent {
                let summary = match self {
                    $(ToolCall::$Variant(inner) => inner.start_summary()),+
                };
                ToolStartEvent { tool: self.name(), summary }
            }

            pub fn execute(&self, mode: &AgentMode) -> ToolDoneEvent {
                if let Some(path) = self.mutable_path()
                    && let AgentMode::Plan(plan_path) = mode
                    && path != plan_path
                {
                    return ToolDoneEvent {
                        tool: self.name(),
                        content: PLAN_WRITE_RESTRICTED.into(),
                        is_error: true,
                    };
                }

                let result = match self {
                    $(ToolCall::$Variant(inner) => inner.execute()),+
                };
                let (content, is_error) = match result {
                    Ok(c) => (c, false),
                    Err(c) => (c, true),
                };
                ToolDoneEvent { tool: self.name(), content, is_error }
            }

            fn mutable_path(&self) -> Option<&str> {
                match self {
                    $(ToolCall::$Variant(inner) => inner.mutable_path()),+
                }
            }

            pub fn definitions() -> Value {
                Value::Array(vec![
                    $(json!({
                        "name": <$inner>::NAME,
                        "description": <$inner>::DESCRIPTION,
                        "input_schema": <$inner>::schema()
                    })),+
                ])
            }

            pub fn scrub_input(name: &str, input: &mut Value) {
                match name {
                    $(<$inner>::NAME => <$inner>::scrub_input(input),)+
                    _ => {}
                }
            }

            pub fn scrub_result(name: &str, content: &str) -> Option<String> {
                match name {
                    $(<$inner>::NAME => <$inner>::scrub_result(content),)+
                    _ => None,
                }
            }
        }
    };
}

register_tools! {
    Bash(bash::Bash),
    Read(read::Read),
    Write(write::Write),
    Edit(edit::Edit),
    Glob(glob::Glob),
    Grep(grep::Grep),
    TodoWrite(todowrite::TodoWrite),
    WebFetch(webfetch::WebFetch),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn from_api_parses_valid_and_rejects_unknown() {
        let tool =
            ToolCall::from_api("bash", &json!({"command": "echo hello", "timeout": 5})).unwrap();
        assert_eq!(tool.name(), "bash");
        assert!(ToolCall::from_api("unknown", &json!({})).is_err());
    }

    #[test]
    fn truncate_output_respects_line_and_byte_limits() {
        let many_lines: String = (0..2500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_output(many_lines);
        assert!(result.ends_with("[truncated]"));

        let many_bytes = "x".repeat(MAX_OUTPUT_BYTES + 1000);
        let result = truncate_output(many_bytes);
        assert!(result.ends_with("[truncated]"));
    }

    #[test]
    fn read_write_roundtrip_with_offset() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.txt").to_string_lossy().to_string();
        let content = (1..=10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");

        let w = write::Write::parse_input(&json!({"path": path, "content": content})).unwrap();
        w.execute().unwrap();

        let r = read::Read::parse_input(&json!({"path": path})).unwrap();
        let full = r.execute().unwrap();
        assert!(full.contains("1: line1"));
        assert!(full.contains("10: line10"));

        let r = read::Read::parse_input(&json!({"path": path, "offset": 3, "limit": 2})).unwrap();
        let slice = r.execute().unwrap();
        assert!(slice.contains("3: line3"));
        assert!(slice.contains("4: line4"));
        assert!(!slice.contains("5: line5"));
    }

    #[test]
    fn glob_finds_and_misses() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        fs::write(dir.path().join("b.txt"), "world").unwrap();
        fs::write(dir.path().join("c.rs"), "fn main(){}").unwrap();
        let dir_str = dir.path().to_string_lossy().to_string();

        let g = glob::Glob::parse_input(&json!({"pattern": "*.txt", "path": dir_str})).unwrap();
        let hit = g.execute().unwrap();
        assert!(hit.contains("a.txt"));
        assert!(!hit.contains("c.rs"));

        let g = glob::Glob::parse_input(&json!({"pattern": "*.nope", "path": dir_str})).unwrap();
        assert_eq!(g.execute().unwrap(), NO_FILES_FOUND);
    }

    #[test]
    fn grep_finds_filters_and_misses() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "hello world\ngoodbye world").unwrap();
        fs::write(dir.path().join("b.rs"), "hello rust").unwrap();
        let dir_str = dir.path().to_string_lossy().to_string();

        let g = grep::Grep::parse_input(&json!({"pattern": "hello", "path": dir_str})).unwrap();
        let hit = g.execute().unwrap();
        assert!(hit.contains("a.txt"));
        assert!(hit.contains("b.rs"));

        let g = grep::Grep::parse_input(
            &json!({"pattern": "hello", "path": dir_str, "include": "*.rs"}),
        )
        .unwrap();
        let filtered = g.execute().unwrap();
        assert!(filtered.contains("b.rs"));
        assert!(!filtered.contains("a.txt"));

        let g =
            grep::Grep::parse_input(&json!({"pattern": "zzzznotfound", "path": dir_str})).unwrap();
        assert_eq!(g.execute().unwrap(), NO_FILES_FOUND);
    }

    #[test]
    fn plan_mode_restricts_mutations() {
        let dir = TempDir::new().unwrap();
        let plan_path = dir.path().join("plan.md").to_string_lossy().to_string();
        let mode = AgentMode::Plan(plan_path.clone());

        let other = dir.path().join("other.rs").to_string_lossy().to_string();
        let blocked = ToolCall::from_api("write", &json!({"path": other, "content": "x"})).unwrap();
        assert!(blocked.execute(&mode).is_error);

        let allowed = ToolCall::from_api(
            "write",
            &json!({"path": plan_path, "content": "plan content"}),
        )
        .unwrap();
        assert!(!allowed.execute(&mode).is_error);
    }

    #[test]
    fn scrub_input_redacts_mutable_tools() {
        let mut input = json!({"path": "/tmp/f.txt", "content": "hello\nworld"});
        ToolCall::scrub_input("write", &mut input);
        assert!(!input["content"].as_str().unwrap().contains("hello"));

        let mut input = json!({"path": "/tmp/f.txt", "old_string": "abc", "new_string": "def"});
        ToolCall::scrub_input("edit", &mut input);
        assert!(!input["old_string"].as_str().unwrap().contains("abc"));
    }

    #[test]
    fn scrub_result_summarizes_read_tools_and_skips_others() {
        assert!(ToolCall::scrub_result("read", "line1\nline2").is_some());
        assert!(ToolCall::scrub_result("glob", "f1\nf2\nf3").is_some());
        assert!(ToolCall::scrub_result("grep", "file.rs:\n  1: match").is_some());
        assert!(ToolCall::scrub_result("bash", "output").is_none());
    }
}
