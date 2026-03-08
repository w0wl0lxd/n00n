use std::collections::HashMap;
use std::fmt::Write;
use std::time::{Duration, Instant};

use maki_interpreter::runner::{self, ToolFn};
use maki_tool_macro::Tool;
use serde_json::Value;

use crate::{AgentEvent, AgentMode, EventSender, ToolInput, ToolOutput};

use super::INTERPRETER_TOOLS;
use super::truncate_output;

const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(100);
const PREAMBLE: &str = "import re\n";

#[derive(Tool, Debug, Clone)]
pub struct CodeInterpreter {
    #[param(
        description = "Python code to execute. Tools are callable functions that return strings (not objects). Available: read, write, edit, multiedit, glob, grep, bash, webfetch, websearch. Call with keyword args: read(path='/file')"
    )]
    code: String,
}

impl CodeInterpreter {
    pub const NAME: &str = "code_execution";
    pub const DESCRIPTION: &str = include_str!("code_execution.md");
    pub const EXAMPLES: Option<&str> = Some(
        r##"[
  {"code": "# Tools return strings, parse them as needed\nresult = grep(pattern='TODO', include='*.rs')\nlines = result.strip().split('\\n')\nprint(f'{len(lines)} TODOs found')"},
  {"code": "# Batch file reading\nfiles = glob(pattern='**/*.rs')\nfor f in files.strip().split('\\n'):\n    if f.strip():\n        content = read(path=f)\n        if 'fn main' in content:\n            print(f)"},
  {"code": "# Aggregate data from multiple files\ntotal = 0\nfor f in glob(pattern='**/*.rs').strip().split('\\n'):\n    if f.strip():\n        content = read(path=f)\n        total += len(content.split('\\n'))\nprint(f'Total lines: {total}')"},
  {"code": "# Chain web requests\nurls = ['https://api.example.com/status', 'https://api.example.com/health']\nfor url in urls:\n    result = webfetch(url=url, timeout=5)\n    print(f'{url}: {len(result)} bytes')"},
  {"code": "# Fetch a page and extract only relevant sections\ncontent = webfetch(url='https://docs.example.com/api')\nfor line in content.split('\\n'):\n    if 'authentication' in line.lower():\n        print(line)"}
]"##,
    );

    pub async fn execute(&self, ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        let code = self.code.clone();
        let tool_use_id = ctx.tool_use_id.clone();
        let event_tx = ctx.event_tx.clone();
        let mode = ctx.mode.clone();

        smol::unblock(move || {
            let tools = build_tool_fns(&event_tx, &mode);
            let code = format!("{PREAMBLE}{code}");

            let result = if let Some(ref id) = tool_use_id {
                let mut last_len = 0usize;
                let mut last_flush = Instant::now();
                runner::run_streaming(&code, &tools, &mut |stdout| {
                    if last_flush.elapsed() >= STREAM_FLUSH_INTERVAL && stdout.len() > last_len {
                        event_tx.try_send(AgentEvent::ToolOutput {
                            id: id.to_string(),
                            content: stdout.to_owned(),
                        });
                        last_len = stdout.len();
                        last_flush = Instant::now();
                    }
                })
            } else {
                runner::run(&code, &tools)
            }
            .map_err(|e| e.to_string())?;

            let mut output = String::new();
            if !result.stdout.is_empty() {
                output.push_str(result.stdout.trim_end());
                output.push('\n');
            }
            if let Some(ref val) = result.output {
                if !output.is_empty() {
                    output.push('\n');
                }
                let _ = write!(output, "return: {val}");
            }
            if output.is_empty() {
                output.push_str("(no output)");
            }

            Ok(ToolOutput::Plain(truncate_output(output)))
        })
        .await
    }

    pub fn start_summary(&self) -> String {
        let lines = self.code.lines().count();
        format!("{lines} lines")
    }
}

impl super::ToolDefaults for CodeInterpreter {
    fn start_input(&self) -> Option<ToolInput> {
        Some(ToolInput::Script {
            language: "python",
            code: self.code.clone(),
        })
    }

    fn augment_description(description: &mut String, _ctx: &super::DescriptionContext) {
        description.push_str(&super::build_interpreter_tools_description());
    }
}

fn build_tool_fns(event_tx: &EventSender, mode: &AgentMode) -> HashMap<String, ToolFn> {
    let mut tools: HashMap<String, ToolFn> = HashMap::new();

    for &tool_name in INTERPRETER_TOOLS {
        let name = tool_name.to_string();
        let tx = event_tx.clone();
        let mode = mode.clone();

        tools.insert(
            name.clone(),
            Box::new(
                move |fn_name: &str, args: Vec<Value>, kwargs: Vec<(String, Value)>| {
                    let input = build_tool_input(&args, &kwargs)?;
                    let call = super::ToolCall::from_api(fn_name, &input)
                        .map_err(|e| format!("tool parse error: {e}"))?;

                    let inner_ctx = super::interpreter_ctx(&mode, &tx);
                    let done = smol::future::block_on(call.execute(&inner_ctx, String::new()));
                    if done.is_error {
                        Err(done.output.as_text())
                    } else {
                        Ok(Value::String(done.output.as_text()))
                    }
                },
            ),
        );
    }

    tools
}

fn build_tool_input(args: &[Value], kwargs: &[(String, Value)]) -> Result<Value, String> {
    if let Some(first) = args.first()
        && first.is_object()
    {
        return Ok(first.clone());
    }

    if !kwargs.is_empty() {
        let mut obj = serde_json::Map::new();
        for (k, v) in kwargs {
            obj.insert(k.clone(), v.clone());
        }
        return Ok(Value::Object(obj));
    }

    if args.is_empty() {
        return Ok(serde_json::json!({}));
    }

    Err("pass arguments as keyword arguments (e.g. read(path='/file'))".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use test_case::test_case;

    use crate::AgentMode;
    use crate::tools::test_support::stub_ctx;

    use super::*;

    #[test]
    fn execute_wraps_output() {
        smol::block_on(async {
            let ctx = stub_ctx(&AgentMode::Build);
            let ci = CodeInterpreter {
                code: "2 + 3".into(),
            };
            let output = ci.execute(&ctx).await.unwrap().as_text();
            assert!(output.contains("5"));
        });
    }

    #[test]
    fn read_tool_via_interpreter() {
        smol::block_on(async {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("test.txt");
            std::fs::write(&path, "line1\nline2\n").unwrap();
            let path_str = path.to_string_lossy();

            let ctx = stub_ctx(&AgentMode::Build);
            let ci = CodeInterpreter {
                code: format!("result = read(path='{path_str}')\nprint(result)"),
            };
            let output = ci.execute(&ctx).await.unwrap().as_text();
            assert!(output.contains("line1"));
        });
    }

    #[test_case(&[], &[("path".into(), json!("/foo"))],  json!({"path": "/foo"}) ; "kwargs")]
    #[test_case(&[json!({"path": "/foo"})], &[],         json!({"path": "/foo"}) ; "dict_passthrough")]
    #[test_case(&[], &[],                                json!({})               ; "no_args")]
    fn build_tool_input_cases(args: &[Value], kwargs: &[(String, Value)], expected: Value) {
        assert_eq!(build_tool_input(args, kwargs).unwrap(), expected);
    }

    #[test]
    fn re_module_available_by_default() {
        smol::block_on(async {
            let ctx = stub_ctx(&AgentMode::Build);
            let ci = CodeInterpreter {
                code: r"re.findall(r'\d+', 'abc123def456')".into(),
            };
            let output = ci.execute(&ctx).await.unwrap().as_text();
            assert!(output.contains("123") && output.contains("456"));
        })
    }
}
