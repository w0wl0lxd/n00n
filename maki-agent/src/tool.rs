use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{AgentError, ToolOutput};

const MAX_OUTPUT_BYTES: usize = 50_000;
const MAX_OUTPUT_LINES: usize = 2000;
const DEFAULT_BASH_TIMEOUT_SECS: u64 = 120;
const PROCESS_POLL_INTERVAL_MS: u64 = 50;
pub const TRUNCATED_MARKER: &str = "[truncated]";

pub fn missing_field_msg(field: &str) -> String {
    format!("missing field `{field}`")
}

pub fn unknown_tool_msg(name: &str) -> String {
    format!("unknown variant `{name}`")
}

pub fn timed_out_msg(secs: u64) -> String {
    format!("command timed out after {secs}s")
}

#[derive(Debug, Clone)]
pub enum ToolCall {
    Bash {
        command: String,
        timeout: Option<u64>,
    },
    Read {
        path: String,
        offset: Option<usize>,
        limit: Option<usize>,
    },
    Write {
        path: String,
        content: String,
    },
}

fn required_str(input: &Value, field: &str, tool: &str) -> Result<String, AgentError> {
    input[field]
        .as_str()
        .map(String::from)
        .ok_or_else(|| AgentError::Tool {
            tool: tool.to_string(),
            message: missing_field_msg(field),
        })
}

impl ToolCall {
    pub fn from_api(name: &str, input: &Value) -> Result<Self, AgentError> {
        let err = || AgentError::Tool {
            tool: name.to_string(),
            message: unknown_tool_msg(name),
        };
        match name {
            "bash" => Ok(Self::Bash {
                command: required_str(input, "command", name)?,
                timeout: input["timeout"].as_u64(),
            }),
            "read" => Ok(Self::Read {
                path: required_str(input, "path", name)?,
                offset: input["offset"].as_u64().map(|v| v as usize),
                limit: input["limit"].as_u64().map(|v| v as usize),
            }),
            "write" => Ok(Self::Write {
                path: required_str(input, "path", name)?,
                content: required_str(input, "content", name)?,
            }),
            _ => Err(err()),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Bash { .. } => "bash",
            Self::Read { .. } => "read",
            Self::Write { .. } => "write",
        }
    }

    pub fn input_summary(&self) -> String {
        match self {
            Self::Bash { command, .. } => command.clone(),
            Self::Read { path, .. } => path.clone(),
            Self::Write { path, .. } => path.clone(),
        }
    }

    pub fn execute(&self) -> ToolOutput {
        match self {
            Self::Bash { command, timeout } => execute_bash(command, *timeout),
            Self::Read {
                path,
                offset,
                limit,
            } => execute_read(path, *offset, *limit),
            Self::Write { path, content } => execute_write(path, content),
        }
    }

    pub fn definitions() -> Value {
        json!([
            {
                "name": "bash",
                "description": "Execute a bash command. Use for running shell commands, git operations, builds, etc.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The bash command to execute" },
                        "timeout": { "type": "integer", "description": "Timeout in seconds (default 120)" }
                    },
                    "required": ["command"]
                }
            },
            {
                "name": "read",
                "description": "Read a file from the filesystem. Returns file contents with line numbers.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the file" },
                        "offset": { "type": "integer", "description": "Line number to start from (1-indexed)" },
                        "limit": { "type": "integer", "description": "Max number of lines to read" }
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "write",
                "description": "Write content to a file. Creates parent directories if needed.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the file" },
                        "content": { "type": "string", "description": "The content to write" }
                    },
                    "required": ["path", "content"]
                }
            }
        ])
    }
}

fn truncate_output(text: String) -> String {
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

fn execute_bash(command: &str, timeout: Option<u64>) -> ToolOutput {
    let timeout_secs = timeout.unwrap_or(DEFAULT_BASH_TIMEOUT_SECS);
    let result = Command::new("bash")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let mut child = match result {
        Ok(c) => c,
        Err(e) => return ToolOutput::err(format!("failed to spawn: {e}")),
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(ref mut out) = child.stdout {
                    let _ = out.read_to_string(&mut stdout);
                }
                if let Some(ref mut err) = child.stderr {
                    let _ = err.read_to_string(&mut stderr);
                }
                let mut output = stdout;
                if !stderr.is_empty() {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&stderr);
                }
                return ToolOutput {
                    content: truncate_output(output),
                    is_error: !status.success(),
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return ToolOutput::err(timed_out_msg(timeout_secs));
                }
                thread::sleep(Duration::from_millis(PROCESS_POLL_INTERVAL_MS));
            }
            Err(e) => return ToolOutput::err(format!("wait error: {e}")),
        }
    }
}

fn execute_read(path: &str, offset: Option<usize>, limit: Option<usize>) -> ToolOutput {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return ToolOutput::err(format!("read error: {e}")),
    };

    let start = offset.unwrap_or(1).saturating_sub(1);
    let limit = limit.unwrap_or(MAX_OUTPUT_LINES);

    let numbered: String = content
        .lines()
        .enumerate()
        .skip(start)
        .take(limit)
        .map(|(i, line)| format!("{}: {line}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");

    ToolOutput::ok(truncate_output(numbered))
}

fn execute_write(path: &str, content: &str) -> ToolOutput {
    if let Some(parent) = Path::new(path).parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        return ToolOutput::err(format!("mkdir error: {e}"));
    }
    match fs::write(path, content) {
        Ok(()) => ToolOutput::ok(format!("wrote {} bytes to {path}", content.len())),
        Err(e) => ToolOutput::err(format!("write error: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::path::PathBuf;

    use super::*;
    use serde_json::json;

    #[test]
    fn from_api_bash_valid() {
        let input = json!({"command": "echo hello", "timeout": 5});
        let tool = ToolCall::from_api("bash", &input).unwrap();
        assert!(
            matches!(tool, ToolCall::Bash { ref command, timeout: Some(5) } if command == "echo hello")
        );
    }

    #[test]
    fn from_api_missing_required_field() {
        let input = json!({});
        let err = ToolCall::from_api("bash", &input).unwrap_err();
        assert!(err.to_string().contains(&missing_field_msg("command")));
    }

    #[test]
    fn from_api_unknown_tool() {
        let err = ToolCall::from_api("unknown", &json!({})).unwrap_err();
        assert!(err.to_string().contains(&unknown_tool_msg("unknown")));
    }

    #[test]
    fn truncate_within_limits() {
        let text = "line1\nline2\nline3".to_string();
        assert_eq!(truncate_output(text.clone()), text);
    }

    #[test]
    fn truncate_excess_lines() {
        let text: String = (0..2500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_output(text);
        assert!(result.ends_with(TRUNCATED_MARKER));
        let line_count = result.lines().count();
        assert!(line_count <= MAX_OUTPUT_LINES + 1);
    }

    #[test]
    fn truncate_excess_bytes() {
        let text = "x".repeat(MAX_OUTPUT_BYTES + 1000);
        let result = truncate_output(text);
        assert!(result.ends_with(TRUNCATED_MARKER));
        assert!(result.len() <= MAX_OUTPUT_BYTES + 20);
    }

    #[test]
    fn execute_bash_echo() {
        let output = execute_bash("echo hello", Some(5));
        assert!(!output.is_error);
        assert_eq!(output.content.trim(), "hello");
    }

    #[test]
    fn execute_bash_failing_command() {
        let output = execute_bash("exit 1", Some(5));
        assert!(output.is_error);
    }

    #[test]
    fn execute_bash_timeout() {
        let output = execute_bash("sleep 10", Some(0));
        assert!(output.is_error);
        assert!(output.content.contains(&timed_out_msg(0)));
    }

    fn temp_file(name: &str) -> (PathBuf, String) {
        let dir = env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("test.txt");
        (dir, path.to_string_lossy().to_string())
    }

    #[test]
    fn read_write_roundtrip() {
        let (dir, path) = temp_file("maki_test_rw");

        let w = execute_write(&path, "hello\nworld\n");
        assert!(!w.is_error);

        let r = execute_read(&path, None, None);
        assert!(!r.is_error);
        assert!(r.content.contains("1: hello"));
        assert!(r.content.contains("2: world"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_with_offset_and_limit() {
        let (dir, path) = temp_file("maki_test_offset");
        let content = (1..=10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        execute_write(&path, &content);

        let r = execute_read(&path, Some(3), Some(2));
        assert!(!r.is_error);
        assert!(r.content.contains("3: line3"));
        assert!(r.content.contains("4: line4"));
        assert!(!r.content.contains("5: line5"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_nonexistent() {
        let r = execute_read("/nonexistent/path/abcfilecba.txt", None, None);
        assert!(r.is_error);
    }
}
