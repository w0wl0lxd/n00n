use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{AgentError, AgentMode, ToolDoneEvent, ToolStartEvent};

const MAX_OUTPUT_BYTES: usize = 50_000;
const MAX_OUTPUT_LINES: usize = 2000;
const DEFAULT_BASH_TIMEOUT_SECS: u64 = 120;
const PROCESS_POLL_INTERVAL_MS: u64 = 10;
const TRUNCATED_MARKER: &str = "[truncated]";
const SEARCH_RESULT_LIMIT: usize = 100;
const MAX_GREP_LINE_LENGTH: usize = 2000;
const NO_FILES_FOUND: &str = "No files found";
const PLAN_WRITE_RESTRICTED: &str = "write restricted to plan file in plan mode";
const MARKER_COMPLETED: &str = "[x]";
const MARKER_IN_PROGRESS: &str = "[>]";
const MARKER_PENDING: &str = "[ ]";
const MARKER_CANCELLED: &str = "[-]";

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

#[derive(Deserialize)]
struct GlobInput {
    pattern: String,
    path: Option<String>,
}

#[derive(Deserialize)]
struct GrepInput {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Deserialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum TodoPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub priority: TodoPriority,
}

#[derive(Debug, Clone, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
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
    Glob {
        pattern: String,
        path: Option<String>,
    },
    Grep {
        pattern: String,
        path: Option<String>,
        include: Option<String>,
    },
    TodoWrite {
        todos: Vec<TodoItem>,
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
            "glob" => {
                let i: GlobInput = parse_input(input, name)?;
                Ok(Self::Glob {
                    pattern: i.pattern,
                    path: i.path,
                })
            }
            "grep" => {
                let i: GrepInput = parse_input(input, name)?;
                Ok(Self::Grep {
                    pattern: i.pattern,
                    path: i.path,
                    include: i.include,
                })
            }
            "todowrite" => {
                #[derive(Deserialize)]
                struct Input {
                    todos: Vec<TodoItem>,
                }
                let i: Input = parse_input(input, name)?;
                Ok(Self::TodoWrite { todos: i.todos })
            }
            _ => Err(AgentError::Tool {
                tool: name.to_string(),
                message: unknown_tool_msg(name),
            }),
        }
    }

    pub fn name(&self) -> &'static str {
        self.into()
    }

    pub fn start_event(&self) -> ToolStartEvent {
        let summary = match self {
            Self::Bash { command, .. } => command.clone(),
            Self::Read { path, .. } | Self::Write { path, .. } => path.clone(),
            Self::Glob { pattern, .. } | Self::Grep { pattern, .. } => pattern.clone(),
            Self::TodoWrite { todos } => format!("{} todos", todos.len()),
        };
        ToolStartEvent {
            tool: self.name(),
            summary,
        }
    }

    pub fn execute(&self, mode: &AgentMode) -> ToolDoneEvent {
        if let Self::Write { path, .. } = self
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
            Self::Bash { command, timeout } => execute_bash(command, *timeout),
            Self::Read {
                path,
                offset,
                limit,
            } => execute_read(path, *offset, *limit),
            Self::Write { path, content } => execute_write(path, content),
            Self::Glob { pattern, path } => execute_glob(pattern, path.as_deref()),
            Self::Grep {
                pattern,
                path,
                include,
            } => execute_grep(pattern, path.as_deref(), include.as_deref()),
            Self::TodoWrite { todos } => execute_todowrite(todos),
        };
        let (content, is_error) = match result {
            Ok(c) => (c, false),
            Err(c) => (c, true),
        };
        ToolDoneEvent {
            tool: self.name(),
            content,
            is_error,
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
            },
            {
                "name": "glob",
                "description": "Find files by glob pattern. Respects .gitignore. Returns absolute paths sorted by modification time (newest first).",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Glob pattern to match (e.g. **/*.rs)" },
                        "path": { "type": "string", "description": "Directory to search in (default: cwd)" }
                    },
                    "required": ["pattern"]
                }
            },
            {
                "name": "grep",
                "description": "Search file contents using regex via ripgrep. Respects .gitignore. Results grouped by file, sorted by modification time.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex pattern to search for" },
                        "path": { "type": "string", "description": "Directory to search in (default: cwd)" },
                        "include": { "type": "string", "description": "File glob filter (e.g. *.rs)" }
                    },
                    "required": ["pattern"]
                }
            },
            {
                "name": "todowrite",
                "description": "Create or update a structured todo list to track tasks. Send the complete list each time (replace-all semantics). Use this to plan multi-step work, track progress, and show the user what you're doing. Mark items in_progress when starting, completed when done. Only one item should be in_progress at a time.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "content": { "type": "string", "description": "Task description" },
                                    "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"] },
                                    "priority": { "type": "string", "enum": ["high", "medium", "low"] }
                                },
                                "required": ["content", "status", "priority"]
                            }
                        }
                    },
                    "required": ["todos"]
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

fn read_pipe_lossy(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

fn execute_bash(command: &str, timeout: Option<u64>) -> Result<String, String> {
    let timeout_secs = timeout.unwrap_or(DEFAULT_BASH_TIMEOUT_SECS);
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn: {e}"))?;

    let stdout_handle = child.stdout.take().map(read_pipe_lossy);
    let stderr_handle = child.stderr.take().map(read_pipe_lossy);

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = stdout_handle
                    .map(|h| h.join().unwrap_or_default())
                    .unwrap_or_default();
                let stderr = stderr_handle
                    .map(|h| h.join().unwrap_or_default())
                    .unwrap_or_default();
                let mut output = stdout;
                if !stderr.is_empty() {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&stderr);
                }
                let content = truncate_output(output);
                if !status.success() {
                    if content.is_empty() {
                        return Err(format!("exited with code {}", status.code().unwrap_or(-1)));
                    }
                    return Err(content);
                }
                return Ok(content);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(timed_out_msg(timeout_secs));
                }
                thread::sleep(Duration::from_millis(PROCESS_POLL_INTERVAL_MS));
            }
            Err(e) => return Err(format!("wait error: {e}")),
        }
    }
}

fn execute_read(path: &str, offset: Option<usize>, limit: Option<usize>) -> Result<String, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;

    let start = offset.unwrap_or(1).saturating_sub(1);
    let limit = limit.unwrap_or(MAX_OUTPUT_LINES);

    let numbered: String = raw
        .lines()
        .enumerate()
        .skip(start)
        .take(limit)
        .map(|(i, line)| format!("{}: {line}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(truncate_output(numbered))
}

fn execute_write(path: &str, content: &str) -> Result<String, String> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {e}"))?;
    }
    fs::write(path, content).map_err(|e| format!("write error: {e}"))?;
    Ok(format!("wrote {} bytes to {path}", content.len()))
}

fn resolve_search_path(path: Option<&str>) -> Result<String, String> {
    match path {
        Some(p) => Ok(p.to_string()),
        None => std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| format!("cwd error: {e}")),
    }
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn execute_glob(pattern: &str, path: Option<&str>) -> Result<String, String> {
    let search_path = resolve_search_path(path)?;

    let mut overrides = OverrideBuilder::new(&search_path);
    overrides
        .add(pattern)
        .map_err(|e| format!("invalid glob pattern: {e}"))?;
    let overrides = overrides
        .build()
        .map_err(|e| format!("glob build error: {e}"))?;

    let mut entries: Vec<(SystemTime, String)> = WalkBuilder::new(&search_path)
        .hidden(false)
        .overrides(overrides)
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
        .map(|e| {
            let p = e.into_path();
            (mtime(&p), p.to_string_lossy().into_owned())
        })
        .collect();

    if entries.is_empty() {
        return Ok(NO_FILES_FOUND.to_string());
    }

    entries.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    entries.truncate(SEARCH_RESULT_LIMIT);

    Ok(entries
        .into_iter()
        .map(|(_, p)| p)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn execute_grep(
    pattern: &str,
    path: Option<&str>,
    include: Option<&str>,
) -> Result<String, String> {
    let search_path = resolve_search_path(path)?;

    let mut cmd = Command::new("rg");
    cmd.args([
        "-nH",
        "--hidden",
        "--no-messages",
        "--field-match-separator",
        "|",
        "--regexp",
        pattern,
    ]);
    if let Some(glob) = include {
        cmd.args(["--glob", glob]);
    }
    cmd.arg(&search_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd.output().map_err(|e| format!("failed to run rg: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut files: Vec<(String, Vec<String>)> = Vec::new();
    for line in stdout.lines() {
        let Some((file, rest)) = line.split_once('|') else {
            continue;
        };
        let Some((line_num, text)) = rest.split_once('|') else {
            continue;
        };
        let mut text = text.to_string();
        if text.len() > MAX_GREP_LINE_LENGTH {
            text.truncate(MAX_GREP_LINE_LENGTH);
            text.push_str("...");
        }
        let formatted = format!("  Line {line_num}: {text}");
        match files.last_mut().filter(|(f, _)| f == file) {
            Some((_, lines)) => lines.push(formatted),
            None => files.push((file.to_string(), vec![formatted])),
        }
    }

    if files.is_empty() {
        return Ok(NO_FILES_FOUND.to_string());
    }

    files.sort_by(|a, b| mtime(Path::new(&b.0)).cmp(&mtime(Path::new(&a.0))));

    let mut result = String::new();
    let mut total = 0;
    for (file, lines) in &files {
        if total >= SEARCH_RESULT_LIMIT {
            break;
        }
        result.push_str(file);
        result.push_str(":\n");
        for line in lines {
            if total >= SEARCH_RESULT_LIMIT {
                break;
            }
            result.push_str(line);
            result.push('\n');
            total += 1;
        }
    }

    Ok(result.trim_end().to_string())
}

fn execute_todowrite(todos: &[TodoItem]) -> Result<String, String> {
    if todos.is_empty() {
        return Ok("No todos.".to_string());
    }
    Ok(todos
        .iter()
        .map(|t| {
            let marker = match t.status {
                TodoStatus::Completed => MARKER_COMPLETED,
                TodoStatus::InProgress => MARKER_IN_PROGRESS,
                TodoStatus::Pending => MARKER_PENDING,
                TodoStatus::Cancelled => MARKER_CANCELLED,
            };
            format!("{marker} ({}) {}", t.priority, t.content)
        })
        .collect::<Vec<_>>()
        .join("\n"))
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

        let todo = ToolCall::from_api(
            "todowrite",
            &json!({"todos": [{"content": "do stuff", "status": "in_progress", "priority": "high"}]}),
        )
        .unwrap();
        assert!(matches!(todo, ToolCall::TodoWrite { ref todos } if todos.len() == 1));
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
        let ok = execute_bash("echo hello", Some(5)).unwrap();
        assert_eq!(ok.trim(), "hello");

        let fail = execute_bash("exit 1", Some(5)).unwrap_err();
        assert!(!fail.is_empty());

        let timeout = execute_bash("sleep 10", Some(0)).unwrap_err();
        assert!(timeout.contains(&timed_out_msg(0)));
    }

    #[test]
    fn execute_bash_large_output_does_not_deadlock() {
        let pipe_buf_overflow = "yes | head -n 100000";
        let result = execute_bash(pipe_buf_overflow, Some(10)).unwrap();
        assert!(result.contains(TRUNCATED_MARKER));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn read_write_roundtrip_with_offset() {
        let dir = temp_dir("maki_test_rw");
        let path = dir.join("test.txt").to_string_lossy().to_string();
        let content = (1..=10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");

        execute_write(&path, &content).unwrap();

        let full = execute_read(&path, None, None).unwrap();
        assert!(full.contains("1: line1"));
        assert!(full.contains("10: line10"));

        let slice = execute_read(&path, Some(3), Some(2)).unwrap();
        assert!(slice.contains("3: line3"));
        assert!(slice.contains("4: line4"));
        assert!(!slice.contains("5: line5"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn execute_glob_finds_and_misses() {
        let dir = temp_dir("maki_test_glob_find");
        fs::write(dir.join("a.txt"), "hello").unwrap();
        fs::write(dir.join("b.txt"), "world").unwrap();
        fs::write(dir.join("c.rs"), "fn main(){}").unwrap();
        let dir_str = dir.to_string_lossy();

        let hit = execute_glob("*.txt", Some(&dir_str)).unwrap();
        assert!(hit.contains("a.txt"));
        assert!(hit.contains("b.txt"));
        assert!(!hit.contains("c.rs"));

        let miss = execute_glob("*.nope", Some(&dir_str)).unwrap();
        assert_eq!(miss, NO_FILES_FOUND);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn execute_grep_finds_filters_and_misses() {
        let dir = temp_dir("maki_test_grep");
        fs::write(dir.join("a.txt"), "hello world\ngoodbye world").unwrap();
        fs::write(dir.join("b.rs"), "hello rust").unwrap();
        let dir_str = dir.to_string_lossy();

        let hit = execute_grep("hello", Some(&dir_str), None).unwrap();
        assert!(hit.contains("a.txt"));
        assert!(hit.contains("b.rs"));

        let filtered = execute_grep("hello", Some(&dir_str), Some("*.rs")).unwrap();
        assert!(filtered.contains("b.rs"));
        assert!(!filtered.contains("a.txt"));

        let miss = execute_grep("zzzznotfound", Some(&dir_str), None).unwrap();
        assert_eq!(miss, NO_FILES_FOUND);

        let _ = fs::remove_dir_all(&dir);
    }

    fn todo(content: &str, status: TodoStatus, priority: TodoPriority) -> TodoItem {
        TodoItem {
            content: content.to_string(),
            status,
            priority,
        }
    }

    #[test]
    fn todowrite_formats_all_statuses() {
        let todos = vec![
            todo("first", TodoStatus::Completed, TodoPriority::High),
            todo("second", TodoStatus::InProgress, TodoPriority::Medium),
            todo("third", TodoStatus::Pending, TodoPriority::Low),
            todo("fourth", TodoStatus::Cancelled, TodoPriority::High),
        ];
        let result = execute_todowrite(&todos).unwrap();
        let expected = format!(
            "{MARKER_COMPLETED} (high) first\n{MARKER_IN_PROGRESS} (medium) second\n{MARKER_PENDING} (low) third\n{MARKER_CANCELLED} (high) fourth"
        );
        assert_eq!(result, expected);
    }

    #[test]
    fn plan_mode_blocks_write_to_non_plan_path() {
        let call = ToolCall::Write {
            path: "/tmp/some_file.rs".to_string(),
            content: "fn main(){}".to_string(),
        };
        let mode = AgentMode::Plan(".maki/plans/123.md".into());
        let result = call.execute(&mode);
        assert!(result.is_error);
        assert_eq!(result.content, PLAN_WRITE_RESTRICTED);
    }

    #[test]
    fn start_event_summaries() {
        let cases: Vec<(ToolCall, &str, &str)> = vec![
            (
                ToolCall::Bash {
                    command: "echo hi".into(),
                    timeout: None,
                },
                "bash",
                "echo hi",
            ),
            (
                ToolCall::Read {
                    path: "/tmp/f.rs".into(),
                    offset: None,
                    limit: None,
                },
                "read",
                "/tmp/f.rs",
            ),
            (
                ToolCall::Write {
                    path: "/tmp/out.txt".into(),
                    content: "data".into(),
                },
                "write",
                "/tmp/out.txt",
            ),
            (
                ToolCall::Glob {
                    pattern: "**/*.rs".into(),
                    path: None,
                },
                "glob",
                "**/*.rs",
            ),
            (
                ToolCall::Grep {
                    pattern: "TODO".into(),
                    path: None,
                    include: None,
                },
                "grep",
                "TODO",
            ),
            (
                ToolCall::TodoWrite { todos: vec![] },
                "todowrite",
                "0 todos",
            ),
        ];
        for (call, expected_tool, expected_summary) in cases {
            let event = call.start_event();
            assert_eq!(event.tool, expected_tool);
            assert_eq!(event.summary, expected_summary);
        }
    }

    #[test]
    fn build_mode_allows_any_write() {
        let dir = temp_dir("maki_test_build_write");
        let path = dir.join("file.txt").to_string_lossy().to_string();
        let call = ToolCall::Write {
            path: path.clone(),
            content: "hello".into(),
        };
        let result = call.execute(&AgentMode::Build);
        assert!(!result.is_error);
        assert!(result.content.contains("wrote"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn execute_bash_stderr_is_captured() {
        let result = execute_bash("echo err >&2", Some(5)).unwrap();
        assert!(result.contains("err"));
    }

    #[test]
    fn read_nonexistent_file_returns_error() {
        let err = execute_read("/tmp/maki_test_nonexistent_file_xyz", None, None).unwrap_err();
        assert!(err.contains("read error"));
    }

    #[test]
    fn plan_mode_allows_write_to_plan_path() {
        let plan_path = env::temp_dir()
            .join("maki_test_plan.md")
            .to_string_lossy()
            .to_string();
        let call = ToolCall::Write {
            path: plan_path.clone(),
            content: "plan content".to_string(),
        };
        let mode = AgentMode::Plan(plan_path.clone());
        let result = call.execute(&mode);
        assert!(!result.is_error);
        assert!(result.content.contains("wrote"));
        let _ = fs::remove_file(&plan_path);
    }
}
