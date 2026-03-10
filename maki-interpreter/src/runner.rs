use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;

use monty::MontyException;
use monty::{
    LimitedTracker, MontyObject, MontyRun, NameLookupResult, PrintWriter, PrintWriterCallback,
    ResourceLimits, RunProgress,
};
use serde_json::Value;
use tracing::debug;

use crate::convert::{json_to_monty, monty_to_json};
use crate::error::InterpreterError;

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_MEMORY: usize = 50 * 1024 * 1024;
const DEFAULT_MAX_RECURSION: usize = 100;
const SCRIPT_NAME: &str = "agent.py";

pub type ToolFn = Box<dyn Fn(&str, Vec<Value>, Vec<(String, Value)>) -> Result<Value, String>>;

#[derive(Debug)]
pub struct InterpreterResult {
    pub output: Option<Value>,
    pub stdout: String,
}

struct StreamingWriter<'a> {
    buffer: String,
    on_line: &'a mut dyn FnMut(&str),
}

impl PrintWriterCallback for StreamingWriter<'_> {
    fn stdout_write(&mut self, output: Cow<'_, str>) -> Result<(), MontyException> {
        self.buffer.push_str(&output);
        Ok(())
    }

    fn stdout_push(&mut self, ch: char) -> Result<(), MontyException> {
        self.buffer.push(ch);
        if ch == '\n' {
            (self.on_line)(&self.buffer);
        }
        Ok(())
    }
}

pub fn run(
    code: &str,
    tools: &HashMap<String, ToolFn>,
) -> Result<InterpreterResult, InterpreterError> {
    let mut stdout = String::new();
    let mut print_writer = PrintWriter::Collect(&mut stdout);
    let output = run_inner(code, tools, default_limits(), &mut print_writer)?;
    Ok(InterpreterResult { output, stdout })
}

pub fn run_streaming(
    code: &str,
    tools: &HashMap<String, ToolFn>,
    on_output: &mut dyn FnMut(&str),
) -> Result<InterpreterResult, InterpreterError> {
    let mut writer = StreamingWriter {
        buffer: String::new(),
        on_line: on_output,
    };
    let mut print_writer = PrintWriter::Callback(&mut writer);
    let output = run_inner(code, tools, default_limits(), &mut print_writer)?;
    let stdout = writer.buffer;
    Ok(InterpreterResult { output, stdout })
}

fn run_inner(
    code: &str,
    tools: &HashMap<String, ToolFn>,
    limits: ResourceLimits,
    print_writer: &mut PrintWriter<'_>,
) -> Result<Option<Value>, InterpreterError> {
    let runner = MontyRun::new(code.to_owned(), SCRIPT_NAME, vec![])
        .map_err(|e| InterpreterError::Parse(e.to_string()))?;

    let tracker = LimitedTracker::new(limits);

    let mut progress = runner
        .start(vec![], tracker, print_writer.reborrow())
        .map_err(|e| InterpreterError::Runtime(e.to_string()))?;

    loop {
        match progress {
            RunProgress::Complete(obj) => {
                let output = match &obj {
                    MontyObject::None => None,
                    _ => Some(monty_to_json(&obj)),
                };
                return Ok(output);
            }
            RunProgress::FunctionCall(call) => {
                let name = call.function_name.clone();
                let args_json: Vec<Value> = call.args.iter().map(monty_to_json).collect();
                let kwargs_json: Vec<(String, Value)> = call
                    .kwargs
                    .iter()
                    .map(|(k, v)| (k.to_string(), monty_to_json(v)))
                    .collect();

                debug!(
                    function = %name,
                    num_args = args_json.len(),
                    num_kwargs = kwargs_json.len(),
                    "interpreter: function call"
                );

                let result = match tools.get(name.as_str()) {
                    Some(tool_fn) => tool_fn(&name, args_json, kwargs_json).map_err(|e| {
                        InterpreterError::ToolCall {
                            tool: name.clone(),
                            message: e,
                        }
                    })?,
                    None => {
                        progress = call
                            .resume(
                                monty::ExtFunctionResult::NotFound(name),
                                print_writer.reborrow(),
                            )
                            .map_err(|e| InterpreterError::Runtime(e.to_string()))?;
                        continue;
                    }
                };

                let return_value = json_to_monty(result);
                progress = call
                    .resume(return_value, print_writer.reborrow())
                    .map_err(|e| InterpreterError::Runtime(e.to_string()))?;
            }
            RunProgress::NameLookup(lookup) => {
                let name = &lookup.name;
                debug!(name = %name, "interpreter: name lookup");

                let result = if tools.contains_key(name.as_str()) {
                    NameLookupResult::Value(MontyObject::Function {
                        name: name.clone(),
                        docstring: None,
                    })
                } else {
                    NameLookupResult::Undefined
                };

                progress = lookup
                    .resume(result, print_writer.reborrow())
                    .map_err(|e| InterpreterError::Runtime(e.to_string()))?;
            }
            RunProgress::OsCall(_) => {
                return Err(InterpreterError::Sandboxed(
                    "OS calls are not permitted".into(),
                ));
            }
            RunProgress::ResolveFutures(_) => {
                return Err(InterpreterError::Sandboxed(
                    "async operations are not supported".into(),
                ));
            }
        }
    }
}

fn default_limits() -> ResourceLimits {
    ResourceLimits::new()
        .max_duration(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .max_memory(DEFAULT_MAX_MEMORY)
        .max_recursion_depth(Some(DEFAULT_MAX_RECURSION))
}

pub fn run_with_limits(
    code: &str,
    tools: &HashMap<String, ToolFn>,
    limits: ResourceLimits,
) -> Result<InterpreterResult, InterpreterError> {
    let mut stdout = String::new();
    let mut print_writer = PrintWriter::Collect(&mut stdout);
    let output = run_inner(code, tools, limits, &mut print_writer)?;
    Ok(InterpreterResult { output, stdout })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn empty_tools() -> HashMap<String, ToolFn> {
        HashMap::new()
    }

    fn echo_tools() -> HashMap<String, ToolFn> {
        let mut tools: HashMap<String, ToolFn> = HashMap::new();
        tools.insert(
            "echo".into(),
            Box::new(|_, args, _| Ok(args.first().cloned().unwrap_or(json!(null)))),
        );
        tools
    }

    #[test]
    fn simple_expression() {
        let result = run("2 + 3", &empty_tools()).unwrap();
        assert_eq!(result.output, Some(json!(5)));
        assert!(result.stdout.is_empty());
    }

    #[test]
    fn print_output() {
        let result = run("print('hello world')", &empty_tools()).unwrap();
        assert_eq!(result.stdout.trim(), "hello world");
    }

    #[test]
    fn tool_call_positional() {
        let result = run("echo(42)", &echo_tools()).unwrap();
        assert_eq!(result.output, Some(json!(42)));
    }

    #[test]
    fn tool_call_kwargs() {
        let mut tools: HashMap<String, ToolFn> = HashMap::new();
        tools.insert(
            "greet".into(),
            Box::new(|_, _, kwargs| {
                let name = kwargs
                    .iter()
                    .find(|(k, _)| k == "name")
                    .map(|(_, v)| v.as_str().unwrap_or("unknown").to_string())
                    .unwrap_or_default();
                Ok(json!(format!("hello {name}")))
            }),
        );
        let result = run("greet(name='world')", &tools).unwrap();
        assert_eq!(result.output, Some(json!("hello world")));
    }

    #[test]
    fn parse_error() {
        let err = run("def", &empty_tools()).unwrap_err();
        assert!(matches!(err, InterpreterError::Parse(_)));
    }

    #[test]
    fn unknown_tool_raises_name_error() {
        let err = run("nonexistent()", &empty_tools()).unwrap_err();
        assert!(
            matches!(err, InterpreterError::Runtime(_)),
            "expected Runtime NameError, got {err:?}"
        );
    }

    #[test]
    fn none_return_is_none_output() {
        let result = run("None", &empty_tools()).unwrap();
        assert_eq!(result.output, None);
    }

    #[test]
    fn tool_error_propagates() {
        let mut tools: HashMap<String, ToolFn> = HashMap::new();
        tools.insert(
            "fail".into(),
            Box::new(|_, _, _| Err("intentional failure".into())),
        );
        let err = run("fail()", &tools).unwrap_err();
        assert!(matches!(err, InterpreterError::ToolCall { .. }));
    }

    #[test]
    fn tool_results_in_computation() {
        let mut tools: HashMap<String, ToolFn> = HashMap::new();
        tools.insert(
            "get_values".into(),
            Box::new(|_, _, _| Ok(json!([10, 20, 30]))),
        );
        let code =
            "values = get_values()\ntotal = 0\nfor v in values:\n    total = total + v\ntotal";
        let result = run(code, &tools).unwrap();
        assert_eq!(result.output, Some(json!(60)));
    }

    #[test]
    fn infinite_loop_hits_timeout() {
        let limits = ResourceLimits::new()
            .max_duration(Duration::from_millis(500))
            .max_recursion_depth(Some(100));
        let err = run_with_limits("while True: pass", &empty_tools(), limits).unwrap_err();
        assert!(
            matches!(err, InterpreterError::Runtime(_)),
            "expected Runtime timeout, got {err:?}"
        );
    }

    #[test]
    fn streaming_collects_stdout() {
        let mut called = false;
        let result = run_streaming(
            "print('hello')\nprint('world')",
            &empty_tools(),
            &mut |_| {
                called = true;
            },
        )
        .unwrap();
        assert_eq!(result.stdout.trim(), "hello\nworld");
        assert!(called);
    }
}
