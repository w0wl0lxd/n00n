mod bash;
mod batch;
mod code_execution;
mod edit;
mod fuzzy_replace;
mod glob;
mod grep;
mod multiedit;
mod question;
mod read;
mod skill;
mod task;
mod todowrite;
mod webfetch;
mod websearch;
mod write;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

use serde_json::{Value, json};
use std::future::Future;
use tracing::error;

use crate::cancel::CancelToken;
use crate::skill::Skill;
use crate::template::Vars;
use crate::{
    AgentError, AgentMode, EventSender, NO_FILES_FOUND, ToolDoneEvent, ToolInput, ToolOutput,
    ToolStartEvent,
};
use maki_providers::Model;
use maki_providers::provider::Provider;

pub struct DescriptionContext<'a> {
    pub skills: &'a [Skill],
}

pub const BASH_TOOL_NAME: &str = bash::Bash::NAME;
pub const BATCH_TOOL_NAME: &str = batch::Batch::NAME;
pub const EDIT_TOOL_NAME: &str = edit::Edit::NAME;
pub const GLOB_TOOL_NAME: &str = glob::Glob::NAME;
pub const GREP_TOOL_NAME: &str = grep::Grep::NAME;
pub const MULTIEDIT_TOOL_NAME: &str = multiedit::MultiEdit::NAME;
pub const QUESTION_TOOL_NAME: &str = question::Question::NAME;
pub const READ_TOOL_NAME: &str = read::Read::NAME;
pub const SKILL_TOOL_NAME: &str = skill::SkillTool::NAME;
pub const TASK_TOOL_NAME: &str = task::Task::NAME;
pub const TODOWRITE_TOOL_NAME: &str = todowrite::TodoWrite::NAME;
pub const WEBFETCH_TOOL_NAME: &str = webfetch::WebFetch::NAME;
pub const WEBSEARCH_TOOL_NAME: &str = websearch::WebSearch::NAME;
pub const WRITE_TOOL_NAME: &str = write::Write::NAME;
pub const CODE_EXECUTION_TOOL_NAME: &str = code_execution::CodeInterpreter::NAME;

pub(crate) const INTERPRETER_TOOLS: &[&str] = &[
    "read",
    "write",
    "edit",
    "multiedit",
    "glob",
    "grep",
    "bash",
    "webfetch",
    "websearch",
];

pub(crate) const RESEARCH_SUBAGENT_TOOLS: &[&str] = &["bash", "read", "glob", "grep", "webfetch"];

pub(crate) const GENERAL_SUBAGENT_TOOLS: &[&str] = &[
    "bash",
    "read",
    "write",
    "edit",
    "multiedit",
    "glob",
    "grep",
    "webfetch",
    "batch",
    "code_execution",
];

const MAX_OUTPUT_BYTES: usize = 50 * 1024;
pub(crate) const MAX_OUTPUT_LINES: usize = 2000;
pub(crate) const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
pub(crate) const SEARCH_RESULT_LIMIT: usize = 100;
pub(crate) const MAX_LINE_BYTES: usize = 500;
const PLAN_WRITE_RESTRICTED: &str = "write restricted to plan file in plan mode";

#[derive(Clone)]
pub struct ToolContext {
    pub provider: Arc<dyn Provider>,
    pub model: Model,
    pub event_tx: EventSender,
    pub mode: AgentMode,
    pub tool_use_id: Option<String>,
    pub user_response_rx: Option<Arc<async_lock::Mutex<flume::Receiver<String>>>>,
    pub skills: Arc<[Skill]>,
    pub loaded_instructions: Arc<Mutex<HashSet<PathBuf>>>,
    pub cancel: CancelToken,
}

pub(crate) fn resolve_search_path(path: Option<&str>) -> Result<String, String> {
    match path {
        Some(p) => Ok(p.to_string()),
        None => std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| format!("cwd error: {e}")),
    }
}

pub(crate) fn line_at_offset(content: &str, offset: usize) -> usize {
    content[..offset].matches('\n').count() + 1
}

pub(crate) fn relative_path(path: &str) -> String {
    let Ok(cwd) = std::env::current_dir() else {
        return path.to_string();
    };
    let cwd = cwd.to_string_lossy();
    path.strip_prefix(cwd.as_ref())
        .and_then(|p| p.strip_prefix('/'))
        .unwrap_or(path)
        .to_string()
}

pub(crate) fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

pub(crate) fn truncate_bytes(line: &str) -> String {
    if line.len() > MAX_LINE_BYTES {
        let boundary = line.floor_char_boundary(MAX_LINE_BYTES);
        format!("{}...", &line[..boundary])
    } else {
        line.to_owned()
    }
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

fn format_examples_as_text(examples: &[Value]) -> Option<String> {
    if examples.is_empty() {
        return None;
    }
    let mut text = String::from("Examples:");
    for ex in examples {
        if let Some(code) = ex.get("code").and_then(|c| c.as_str()) {
            text.push_str("\n```\n");
            text.push_str(code);
            text.push_str("\n```");
        }
    }
    Some(text)
}

fn format_tool_signature(name: &str, schema: &Value) -> String {
    let empty_props = serde_json::Map::new();
    let props = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .unwrap_or(&empty_props);
    let required: std::collections::HashSet<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let params: Vec<String> = props
        .iter()
        .map(|(pname, pschema)| {
            let ptype = pschema
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("any");
            let ptype_py = match ptype {
                "string" => "str",
                "integer" => "int",
                "boolean" => "bool",
                "array" => "list",
                _ => "any",
            };
            if required.contains(pname.as_str()) {
                format!("{pname}: {ptype_py}")
            } else {
                format!("{pname}: {ptype_py} = None")
            }
        })
        .collect();

    format!("- {name}({}) -> str", params.join(", "))
}

pub(crate) fn build_interpreter_tools_description() -> String {
    let mut desc =
        String::from("\n\nAvailable tools (called as Python functions with keyword arguments):\n");
    for name in INTERPRETER_TOOLS {
        if let Some(schema) = ToolCall::schema_for(name) {
            desc.push_str(&format_tool_signature(name, &schema));
            desc.push('\n');
        }
    }
    desc
}

pub(crate) trait ToolDefaults {
    fn start_input(&self) -> Option<ToolInput> {
        None
    }
    fn start_output(&self) -> Option<ToolOutput> {
        None
    }
    fn start_annotation(&self) -> Option<String> {
        None
    }
    fn mutable_path(&self) -> Option<&str> {
        None
    }
    fn augment_description(_description: &mut String, _ctx: &DescriptionContext) {}
}

macro_rules! register_tools {
    ($($Variant:ident($inner:path)),+ $(,)?) => {
        $(const _: () = { fn _assert_defaults<T: ToolDefaults>() {} fn _check() { _assert_defaults::<$inner>() } };)+

        const _: () = {
            const NAMES: &[&str] = &[$(<$inner>::NAME),+];
            const fn str_eq(a: &str, b: &str) -> bool {
                let (a, b) = (a.as_bytes(), b.as_bytes());
                if a.len() != b.len() { return false; }
                let mut i = 0;
                while i < a.len() {
                    if a[i] != b[i] { return false; }
                    i += 1;
                }
                true
            }
            let mut i = 0;
            while i < NAMES.len() {
                let mut j = i + 1;
                while j < NAMES.len() {
                    assert!(!str_eq(NAMES[i], NAMES[j]), "duplicate tool NAME detected");
                    j += 1;
                }
                i += 1;
            }
        };

        #[derive(Debug, Clone)]
        pub enum ToolCall {
            $($Variant($inner)),+
        }

        macro_rules! dispatch {
            ($self:expr, |$t:ident| $body:expr) => {
                match $self { $(ToolCall::$Variant($t) => $body),+ }
            };
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

            pub fn name_static(name: &str) -> Option<&'static str> {
                match name {
                    $(<$inner>::NAME => Some(<$inner>::NAME),)+
                    _ => None,
                }
            }

            pub fn start_summary(&self) -> String {
                dispatch!(self, |t| t.start_summary())
            }

            pub fn start_input(&self) -> Option<ToolInput> {
                dispatch!(self, |t| t.start_input())
            }

            pub fn start_event(&self, id: String) -> ToolStartEvent {
                dispatch!(self, |t| ToolStartEvent {
                    id,
                    tool: self.name(),
                    summary: t.start_summary(),
                    annotation: t.start_annotation(),
                    input: t.start_input(),
                    output: t.start_output(),
                })
            }

            pub fn execute<'a>(&'a self, ctx: &'a ToolContext, id: String) -> Pin<Box<dyn Future<Output = ToolDoneEvent> + Send + 'a>> {
                Box::pin(async move {
                    if let Some(path) = self.mutable_path()
                        && let AgentMode::Plan(plan_path) = &ctx.mode
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
                        $(ToolCall::$Variant(inner) => inner.execute(ctx).await),+
                    };
                    let (output, is_error) = match result {
                        Ok(o) => (o, false),
                        Err(e) => {
                            error!(tool = self.name(), error = %e, "tool execution failed");
                            (ToolOutput::Plain(e), true)
                        }
                    };
                    ToolDoneEvent { id, tool: self.name(), output, is_error }
                })
            }

            fn mutable_path(&self) -> Option<&str> {
                dispatch!(self, |t| t.mutable_path())
            }

            pub fn schema_for(name: &str) -> Option<Value> {
                match name {
                    $(<$inner>::NAME => Some(<$inner>::schema()),)+
                    _ => None,
                }
            }

            fn all_defs(vars: &Vars, skills: &[Skill], supports_examples: bool) -> Vec<(&'static str, Value)> {
                let ctx = DescriptionContext { skills };
                vec![
                    $((<$inner>::NAME, {
                        let mut description = vars.apply(<$inner>::DESCRIPTION).into_owned();
                        <$inner>::augment_description(&mut description, &ctx);
                        let mut def = json!({
                            "name": <$inner>::NAME,
                            "description": &description,
                            "input_schema": <$inner>::schema()
                        });
                        if let Some(json) = <$inner>::EXAMPLES {
                            let examples: Vec<Value> = serde_json::from_str(json)
                                .expect(concat!("invalid EXAMPLES JSON for ", stringify!($inner)));
                            if supports_examples {
                                def["input_examples"] = Value::Array(examples);
                            } else if let Some(text) = format_examples_as_text(&examples) {
                                def["description"] = Value::String(format!("{}\n\n{}", description, text));
                            }
                        }
                        def
                    })),+
                ]
            }

            pub fn definitions(vars: &Vars, skills: &[Skill], supports_examples: bool) -> (Vec<&'static str>, Value) {
                let defs = Self::all_defs(vars, skills, supports_examples);
                let names = defs.iter().map(|(name, _)| *name).collect();
                let values = Value::Array(defs.into_iter().map(|(_, def)| def).collect());
                (names, values)
            }

            pub fn definitions_filtered(vars: &Vars, allowed: &[&str], supports_examples: bool) -> Value {
                Value::Array(
                    Self::all_defs(vars, &[], supports_examples).into_iter()
                        .filter(|(name, _)| allowed.contains(name))
                        .map(|(_, def)| def)
                        .collect()
                )
            }

            pub fn static_name(name: &str) -> Option<&'static str> {
                match name {
                    $(<$inner>::NAME => Some(<$inner>::NAME),)+
                    _ => None,
                }
            }

            pub fn definitions_excluding(vars: &Vars, skills: &[Skill], blocked: &[&str], supports_examples: bool) -> (Vec<&'static str>, Value) {
                let defs: Vec<_> = Self::all_defs(vars, skills, supports_examples).into_iter()
                    .filter(|(name, _)| !blocked.contains(name))
                    .collect();
                let names = defs.iter().map(|(name, _)| *name).collect();
                let values = Value::Array(defs.into_iter().map(|(_, def)| def).collect());
                (names, values)
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
    Question(question::Question),
    TodoWrite(todowrite::TodoWrite),
    WebFetch(webfetch::WebFetch),
    WebSearch(websearch::WebSearch),
    Skill(skill::SkillTool),
    Task(task::Task),
    Batch(batch::Batch),
    CodeInterpreter(code_execution::CodeInterpreter),
}

use maki_providers::provider::BoxFuture;

struct NullProvider;

impl Provider for NullProvider {
    fn stream_message<'a>(
        &'a self,
        _: &'a Model,
        _: &'a [maki_providers::Message],
        _: &'a str,
        _: &'a Value,
        _: &'a flume::Sender<maki_providers::ProviderEvent>,
    ) -> BoxFuture<'a, Result<maki_providers::StreamResponse, AgentError>> {
        Box::pin(async { unimplemented!() })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<String>, AgentError>> {
        Box::pin(async { unimplemented!() })
    }
}

pub(crate) fn interpreter_ctx(
    mode: &AgentMode,
    event_tx: &EventSender,
    cancel: CancelToken,
) -> ToolContext {
    static PROVIDER: LazyLock<Arc<dyn Provider>> = LazyLock::new(|| Arc::new(NullProvider));
    static MODEL: LazyLock<Model> =
        LazyLock::new(|| Model::from_spec("anthropic/claude-sonnet-4-20250514").unwrap());
    static SKILLS: LazyLock<Arc<[Skill]>> = LazyLock::new(|| Arc::from([]));
    ToolContext {
        provider: Arc::clone(&PROVIDER),
        model: MODEL.clone(),
        event_tx: event_tx.clone(),
        mode: mode.clone(),
        tool_use_id: None,
        user_response_rx: None,
        skills: Arc::clone(&SKILLS),
        loaded_instructions: Arc::new(Mutex::new(HashSet::new())),
        cancel,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::{Envelope, EventSender};

    use super::*;

    pub(crate) fn stub_ctx_with(
        mode: &AgentMode,
        event_tx: Option<&EventSender>,
        tool_use_id: Option<&str>,
    ) -> ToolContext {
        let fallback_tx;
        let event_tx = match event_tx {
            Some(tx) => tx,
            None => {
                fallback_tx = EventSender::new(flume::unbounded::<Envelope>().0, 0);
                &fallback_tx
            }
        };
        let mut ctx = interpreter_ctx(mode, event_tx, CancelToken::none());
        ctx.tool_use_id = tool_use_id.map(String::from);
        ctx
    }

    pub(crate) fn stub_ctx(mode: &AgentMode) -> ToolContext {
        stub_ctx_with(mode, None, None)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;
    use test_case::test_case;

    use super::test_support::stub_ctx;
    use super::*;

    #[test_case("short",                            "short"                             ; "short_passthrough")]
    #[test_case(&"x".repeat(MAX_LINE_BYTES),       &"x".repeat(MAX_LINE_BYTES)        ; "exact_boundary")]
    #[test_case(&"x".repeat(MAX_LINE_BYTES + 500), &format!("{}...", "x".repeat(MAX_LINE_BYTES)) ; "long_truncated")]
    fn truncate_bytes_cases(input: &str, expected: &str) {
        let result = truncate_bytes(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn truncate_bytes_respects_char_boundary() {
        let input = "a".repeat(MAX_LINE_BYTES - 1) + "\u{1F600}";
        let result = truncate_bytes(&input);
        assert!(result.len() <= MAX_LINE_BYTES + "...".len());
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_output_respects_line_and_byte_limits() {
        let many_lines: String = (0..MAX_OUTPUT_LINES + 500)
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
        smol::block_on(async {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("test.txt").to_string_lossy().to_string();
            let content = (1..=10)
                .map(|i| format!("line{i}"))
                .collect::<Vec<_>>()
                .join("\n");
            let ctx = stub_ctx(&AgentMode::Build);

            let w = write::Write::parse_input(&json!({"path": path, "content": content})).unwrap();
            w.execute(&ctx).await.unwrap();

            let r = read::Read::parse_input(&json!({"path": path})).unwrap();
            let full = r.execute(&ctx).await.unwrap().as_text().to_string();
            assert!(full.contains("1: line1"));
            assert!(full.contains("10: line10"));

            let r =
                read::Read::parse_input(&json!({"path": path, "offset": 3, "limit": 2})).unwrap();
            let slice = r.execute(&ctx).await.unwrap().as_text().to_string();
            assert!(slice.contains("3: line3"));
            assert!(slice.contains("4: line4"));
            assert!(!slice.contains("5: line5"));
        });
    }

    #[test]
    fn glob_finds_and_misses() {
        smol::block_on(async {
            let dir = TempDir::new().unwrap();
            fs::write(dir.path().join("a.txt"), "hello").unwrap();
            fs::write(dir.path().join("b.txt"), "world").unwrap();
            fs::write(dir.path().join("c.rs"), "fn main(){}").unwrap();
            let dir_str = dir.path().to_string_lossy().to_string();
            let ctx = stub_ctx(&AgentMode::Build);

            let g = glob::Glob::parse_input(&json!({"pattern": "*.txt", "path": dir_str})).unwrap();
            let output = g.execute(&ctx).await.unwrap();
            let ToolOutput::GlobResult { files } = &output else {
                panic!("expected GlobResult");
            };
            assert_eq!(files.len(), 2);
            assert!(files.iter().all(|f| f.contains(".txt")));
            assert!(files.iter().all(|f| !f.contains(".rs")));

            let g =
                glob::Glob::parse_input(&json!({"pattern": "*.nope", "path": dir_str})).unwrap();
            let output = g.execute(&ctx).await.unwrap();
            assert!(matches!(output, ToolOutput::GlobResult { files } if files.is_empty()));
        });
    }

    #[test]
    fn grep_finds_filters_and_misses() {
        smol::block_on(async {
            let dir = TempDir::new().unwrap();
            fs::write(dir.path().join("a.txt"), "hello world\ngoodbye world").unwrap();
            fs::write(dir.path().join("b.rs"), "hello rust").unwrap();
            let dir_str = dir.path().to_string_lossy().to_string();
            let ctx = stub_ctx(&AgentMode::Build);

            let g = grep::Grep::parse_input(&json!({"pattern": "hello", "path": dir_str})).unwrap();
            let hit = g.execute(&ctx).await.unwrap().as_text().to_string();
            assert!(hit.contains("a.txt"));
            assert!(hit.contains("b.rs"));

            let g = grep::Grep::parse_input(
                &json!({"pattern": "hello", "path": dir_str, "include": "*.rs"}),
            )
            .unwrap();
            let filtered = g.execute(&ctx).await.unwrap().as_text().to_string();
            assert!(filtered.contains("b.rs"));
            assert!(!filtered.contains("a.txt"));

            let g = grep::Grep::parse_input(&json!({"pattern": "zzzznotfound", "path": dir_str}))
                .unwrap();
            assert_eq!(g.execute(&ctx).await.unwrap().as_text(), NO_FILES_FOUND);
        });
    }

    #[test]
    fn from_api_unknown_tool_returns_error() {
        let err = ToolCall::from_api("nonexistent_tool", &json!({})).unwrap_err();
        let AgentError::Tool { tool, .. } = err else {
            panic!("expected AgentError::Tool, got {err:?}");
        };
        assert_eq!(tool, "nonexistent_tool");
    }

    #[test]
    fn tool_definitions_schema_requires_additional_properties_false() {
        let vars = Vars::new().set("{cwd}", "/tmp");
        let (_, all) = ToolCall::definitions(&vars, &[], true);
        for def in all.as_array().unwrap() {
            assert_eq!(
                def["input_schema"]["additionalProperties"],
                json!(false),
                "tool {} missing additionalProperties: false",
                def["name"]
            );
        }
    }

    #[test]
    fn definitions_filtered_returns_only_requested() {
        let vars = Vars::new().set("{cwd}", "/tmp");
        let filtered = ToolCall::definitions_filtered(&vars, &["bash", "read"], true);
        let names: Vec<&str> = filtered
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["bash", "read"]);
    }

    #[test]
    fn plan_mode_restricts_mutations() {
        smol::block_on(async {
            let dir = TempDir::new().unwrap();
            let plan_path = dir.path().join("plan.md").to_string_lossy().to_string();
            let mode = AgentMode::Plan(plan_path.clone());
            let ctx = stub_ctx(&mode);

            let other = dir.path().join("other.rs").to_string_lossy().to_string();
            let blocked =
                ToolCall::from_api("write", &json!({"path": other, "content": "x"})).unwrap();
            assert!(blocked.execute(&ctx, "t1".into()).await.is_error);

            let allowed = ToolCall::from_api(
                "write",
                &json!({"path": plan_path, "content": "plan content"}),
            )
            .unwrap();
            assert!(!allowed.execute(&ctx, "t2".into()).await.is_error);
        });
    }
}
