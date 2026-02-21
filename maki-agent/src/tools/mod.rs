mod bash;
mod batch;
mod edit;
mod fuzzy_replace;
mod glob;
mod grep;
mod multiedit;
mod read;
mod task;
mod todowrite;
mod webfetch;
mod write;

use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::SystemTime;

use serde_json::{Value, json};

use crate::template::Vars;
use crate::{AgentError, AgentMode, Envelope, ToolDoneEvent, ToolOutput, ToolStartEvent};
use maki_providers::Model;
use maki_providers::provider::Provider;

pub const WEBFETCH_TOOL_NAME: &str = webfetch::WebFetch::NAME;
pub const GLOB_TOOL_NAME: &str = glob::Glob::NAME;
pub const GREP_TOOL_NAME: &str = grep::Grep::NAME;
const MAX_OUTPUT_BYTES: usize = 30_000;
pub(crate) const MAX_OUTPUT_LINES: usize = 2000;
pub(crate) const SEARCH_RESULT_LIMIT: usize = 100;
pub(crate) const NO_FILES_FOUND: &str = "No files found";
const PLAN_WRITE_RESTRICTED: &str = "write restricted to plan file in plan mode";

pub struct ToolContext<'a> {
    pub provider: &'a dyn Provider,
    pub model: &'a Model,
    pub event_tx: &'a Sender<Envelope>,
    pub mode: &'a AgentMode,
    pub tool_use_id: Option<&'a str>,
}

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
            let boundary = result.floor_char_boundary(MAX_OUTPUT_BYTES);
            result.truncate(boundary);
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

            pub fn start_summary(&self) -> String {
                match self {
                    $(ToolCall::$Variant(inner) => inner.start_summary()),+
                }
            }

            pub fn start_event(&self, id: String) -> ToolStartEvent {
                let input = match self {
                    $(ToolCall::$Variant(inner) => inner.start_input()),+
                };
                ToolStartEvent { id, tool: self.name(), summary: self.start_summary(), input }
            }

            pub fn execute(&self, ctx: &ToolContext, id: String) -> ToolDoneEvent {
                if let Some(path) = self.mutable_path()
                    && let AgentMode::Plan(plan_path) = ctx.mode
                    && path != plan_path
                {
                    return ToolDoneEvent {
                        id,
                        tool: self.name(),
                        output: ToolOutput::Plain(PLAN_WRITE_RESTRICTED.into()),
                        is_error: true,
                    };
                }

                let result = match self {
                    $(ToolCall::$Variant(inner) => inner.execute(ctx)),+
                };
                let (output, is_error) = match result {
                    Ok(o) => (o, false),
                    Err(e) => (ToolOutput::Plain(e), true),
                };
                ToolDoneEvent { id, tool: self.name(), output, is_error }
            }

            fn mutable_path(&self) -> Option<&str> {
                match self {
                    $(ToolCall::$Variant(inner) => inner.mutable_path()),+
                }
            }

            pub fn definitions(vars: &Vars) -> Value {
                Self::definitions_filtered(vars, None)
            }

            pub fn definitions_filtered(vars: &Vars, allowed: Option<&[&str]>) -> Value {
                let all = vec![
                    $((<$inner>::NAME, json!({
                        "name": <$inner>::NAME,
                        "description": vars.apply(<$inner>::DESCRIPTION),
                        "input_schema": <$inner>::schema()
                    }))),+
                ];
                Value::Array(match allowed {
                    Some(filter) => all.into_iter()
                        .filter(|(name, _)| filter.contains(name))
                        .map(|(_, def)| def)
                        .collect(),
                    None => all.into_iter().map(|(_, def)| def).collect(),
                })
            }
        }
    };
}

register_tools! {
    Bash(bash::Bash),
    Read(read::Read),
    Write(write::Write),
    Edit(edit::Edit),
    MultiEdit(multiedit::MultiEdit),
    Glob(glob::Glob),
    Grep(grep::Grep),
    TodoWrite(todowrite::TodoWrite),
    WebFetch(webfetch::WebFetch),
    Task(task::Task),
    Batch(batch::Batch),
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::mpsc::Sender;

    use maki_providers::provider::Provider;
    use maki_providers::{AgentError, Envelope, Model, StreamResponse};
    use serde_json::Value;

    use super::*;

    struct StubProvider;

    impl Provider for StubProvider {
        fn stream_message(
            &self,
            _: &Model,
            _: &[maki_providers::Message],
            _: &str,
            _: &Value,
            _: &Sender<Envelope>,
        ) -> Result<StreamResponse, AgentError> {
            unimplemented!()
        }

        fn list_models(&self) -> Result<Vec<String>, AgentError> {
            unimplemented!()
        }
    }

    pub(crate) fn stub_ctx(mode: &AgentMode) -> ToolContext<'_> {
        let tx: &Sender<Envelope> = Box::leak(Box::new(std::sync::mpsc::channel().0));
        let model: &Model = Box::leak(Box::new(
            Model::from_spec("anthropic/claude-sonnet-4-20250514").unwrap(),
        ));
        let provider: &dyn Provider = Box::leak(Box::new(StubProvider));
        ToolContext {
            provider,
            model,
            event_tx: tx,
            mode,
            tool_use_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::test_support::stub_ctx;
    use super::*;

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
        let ctx = stub_ctx(&AgentMode::Build);

        let w = write::Write::parse_input(&json!({"path": path, "content": content})).unwrap();
        w.execute(&ctx).unwrap();

        let r = read::Read::parse_input(&json!({"path": path})).unwrap();
        let full = r.execute(&ctx).unwrap().as_text().to_string();
        assert!(full.contains("1: line1"));
        assert!(full.contains("10: line10"));

        let r = read::Read::parse_input(&json!({"path": path, "offset": 3, "limit": 2})).unwrap();
        let slice = r.execute(&ctx).unwrap().as_text().to_string();
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
        let ctx = stub_ctx(&AgentMode::Build);

        let g = glob::Glob::parse_input(&json!({"pattern": "*.txt", "path": dir_str})).unwrap();
        let hit = g.execute(&ctx).unwrap().as_text().to_string();
        assert!(hit.contains("a.txt"));
        assert!(!hit.contains("c.rs"));

        let g = glob::Glob::parse_input(&json!({"pattern": "*.nope", "path": dir_str})).unwrap();
        assert_eq!(g.execute(&ctx).unwrap().as_text(), NO_FILES_FOUND);
    }

    #[test]
    fn grep_finds_filters_and_misses() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "hello world\ngoodbye world").unwrap();
        fs::write(dir.path().join("b.rs"), "hello rust").unwrap();
        let dir_str = dir.path().to_string_lossy().to_string();
        let ctx = stub_ctx(&AgentMode::Build);

        let g = grep::Grep::parse_input(&json!({"pattern": "hello", "path": dir_str})).unwrap();
        let hit = g.execute(&ctx).unwrap().as_text().to_string();
        assert!(hit.contains("a.txt"));
        assert!(hit.contains("b.rs"));

        let g = grep::Grep::parse_input(
            &json!({"pattern": "hello", "path": dir_str, "include": "*.rs"}),
        )
        .unwrap();
        let filtered = g.execute(&ctx).unwrap().as_text().to_string();
        assert!(filtered.contains("b.rs"));
        assert!(!filtered.contains("a.txt"));

        let g =
            grep::Grep::parse_input(&json!({"pattern": "zzzznotfound", "path": dir_str})).unwrap();
        assert_eq!(g.execute(&ctx).unwrap().as_text(), NO_FILES_FOUND);
    }

    #[test]
    fn from_api_unknown_tool_returns_error() {
        let err = ToolCall::from_api("nonexistent_tool", &json!({})).unwrap_err();
        match err {
            AgentError::Tool { tool, message } => {
                assert_eq!(tool, "nonexistent_tool");
                assert!(message.contains("unknown variant"));
            }
            other => panic!("expected AgentError::Tool, got {other:?}"),
        }
    }

    #[test]
    fn definitions_filtered_restricts_to_allowed() {
        let vars = Vars::new().set("{cwd}", "/tmp");
        let filtered = ToolCall::definitions_filtered(&vars, Some(&["bash", "read"]));
        let names: Vec<&str> = filtered
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"read"));
        assert!(!names.iter().any(|n| *n != "bash" && *n != "read"));
    }

    #[test]
    fn plan_mode_restricts_mutations() {
        let dir = TempDir::new().unwrap();
        let plan_path = dir.path().join("plan.md").to_string_lossy().to_string();
        let mode = AgentMode::Plan(plan_path.clone());
        let ctx = stub_ctx(&mode);

        let other = dir.path().join("other.rs").to_string_lossy().to_string();
        let blocked = ToolCall::from_api("write", &json!({"path": other, "content": "x"})).unwrap();
        assert!(blocked.execute(&ctx, "t1".into()).is_error);

        let allowed = ToolCall::from_api(
            "write",
            &json!({"path": plan_path, "content": "plan content"}),
        )
        .unwrap();
        assert!(!allowed.execute(&ctx, "t2".into()).is_error);
    }
}
