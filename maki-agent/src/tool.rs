use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{AgentError, ToolOutput};

const MAX_OUTPUT_BYTES: usize = 50_000;
const MAX_OUTPUT_LINES: usize = 2000;
const DEFAULT_BASH_TIMEOUT_SECS: u64 = 120;
const PROCESS_POLL_INTERVAL_MS: u64 = 10;
const TRUNCATED_MARKER: &str = "[truncated]";

fn unknown_tool_msg(name: &str) -> String {
    format!("unknown variant `{name}`")
}

fn timed_out_msg(secs: u64) -> String {
    format!("command timed out after {secs}s")
}

#[derive(Deserialize)]
struct BashInput {
    command: String,
    timeout: Option<u64>,
}

#[derive(Deserialize)]
struct ReadInput {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct WriteInput {
    path: String,
    content: String,
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

fn parse_input<T: serde::de::DeserializeOwned>(input: &Value, tool: &str) -> Result<T, AgentError> {
    serde_json::from_value(input.clone()).map_err(|e| AgentError::Tool {
        tool: tool.to_string(),
        message: e.to_string(),
    })
}

impl ToolCall {
    pub fn from_api(name: &str, input: &Value) -> Result<Self, AgentError> {
        match name {
            "bash" => {
                let i: BashInput = parse_input(input, name)?;
                Ok(Self::Bash {
                    command: i.command,
                    timeout: i.timeout,
                })
            }
            "read" => {
                let i: ReadInput = parse_input(input, name)?;
                Ok(Self::Read {
                    path: i.path,
                    offset: i.offset,
                    limit: i.limit,
                })
            }
            "write" => {
                let i: WriteInput = parse_input(input, name)?;
                Ok(Self::Write {
                    path: i.path,
                    content: i.content,
                })
            }
            _ => Err(AgentError::Tool {
                tool: name.to_string(),
                message: unknown_tool_msg(name),
            }),
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
    fn from_api_parses_valid_and_rejects_invalid() {
        let tool =
            ToolCall::from_api("bash", &json!({"command": "echo hello", "timeout": 5})).unwrap();
        assert!(
            matches!(tool, ToolCall::Bash { ref command, timeout: Some(5) } if command == "echo hello")
        );

        let err = ToolCall::from_api("bash", &json!({})).unwrap_err();
        assert!(err.to_string().contains("command"));

        let err = ToolCall::from_api("unknown", &json!({})).unwrap_err();
        assert!(err.to_string().contains(&unknown_tool_msg("unknown")));
    }

    #[test]
    fn truncate_output_respects_limits() {
        let small = "line1\nline2\nline3".to_string();
        assert_eq!(truncate_output(small.clone()), small);

        let many_lines: String = (0..2500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_output(many_lines);
        assert!(result.ends_with(TRUNCATED_MARKER));
        assert!(result.lines().count() <= MAX_OUTPUT_LINES + 1);

        let many_bytes = "x".repeat(MAX_OUTPUT_BYTES + 1000);
        let result = truncate_output(many_bytes);
        assert!(result.ends_with(TRUNCATED_MARKER));
        assert!(result.len() <= MAX_OUTPUT_BYTES + 20);
    }

    #[test]
    fn execute_bash_success_failure_and_timeout() {
        let ok = execute_bash("echo hello", Some(5));
        assert!(!ok.is_error);
        assert_eq!(ok.content.trim(), "hello");

        let fail = execute_bash("exit 1", Some(5));
        assert!(fail.is_error);

        let timeout = execute_bash("sleep 10", Some(0));
        assert!(timeout.is_error);
        assert!(timeout.content.contains(&timed_out_msg(0)));
    }

    fn temp_file(name: &str) -> (PathBuf, String) {
        let dir = env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("test.txt");
        (dir, path.to_string_lossy().to_string())
    }

    #[test]
    fn read_write_roundtrip_with_offset() {
        let (dir, path) = temp_file("maki_test_rw");
        let content = (1..=10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");

        let w = execute_write(&path, &content);
        assert!(!w.is_error);

        let full = execute_read(&path, None, None);
        assert!(!full.is_error);
        assert!(full.content.contains("1: line1"));
        assert!(full.content.contains("10: line10"));

        let slice = execute_read(&path, Some(3), Some(2));
        assert!(!slice.is_error);
        assert!(slice.content.contains("3: line3"));
        assert!(slice.content.contains("4: line4"));
        assert!(!slice.content.contains("5: line5"));

        let _ = fs::remove_dir_all(&dir);
    }
}
