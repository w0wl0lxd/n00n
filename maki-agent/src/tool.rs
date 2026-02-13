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

use crate::{AgentError, ToolOutput};

const MAX_OUTPUT_BYTES: usize = 50_000;
const MAX_OUTPUT_LINES: usize = 2000;
const DEFAULT_BASH_TIMEOUT_SECS: u64 = 120;
const PROCESS_POLL_INTERVAL_MS: u64 = 10;
const TRUNCATED_MARKER: &str = "[truncated]";
const SEARCH_RESULT_LIMIT: usize = 100;
const MAX_GREP_LINE_LENGTH: usize = 2000;
const NO_FILES_FOUND: &str = "No files found";

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
    Glob {
        pattern: String,
        path: Option<String>,
    },
    Grep {
        pattern: String,
        path: Option<String>,
        include: Option<String>,
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
            Self::Glob { .. } => "glob",
            Self::Grep { .. } => "grep",
        }
    }

    pub fn input_summary(&self) -> String {
        match self {
            Self::Bash { command, .. } => command.clone(),
            Self::Read { path, .. } => path.clone(),
            Self::Write { path, .. } => path.clone(),
            Self::Glob { pattern, .. } => pattern.clone(),
            Self::Grep { pattern, .. } => pattern.clone(),
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
            Self::Glob { pattern, path } => execute_glob(pattern, path.as_deref()),
            Self::Grep {
                pattern,
                path,
                include,
            } => execute_grep(pattern, path.as_deref(), include.as_deref()),
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

fn execute_bash(command: &str, timeout: Option<u64>) -> ToolOutput {
    let timeout_secs = timeout.unwrap_or(DEFAULT_BASH_TIMEOUT_SECS);
    let mut child = match Command::new("bash")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return ToolOutput::err(format!("failed to spawn: {e}")),
    };

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
                let is_error = !status.success();
                if is_error && content.is_empty() {
                    return ToolOutput::err(format!(
                        "exited with code {}",
                        status.code().unwrap_or(-1)
                    ));
                }
                return ToolOutput { content, is_error };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
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

fn resolve_search_path(path: Option<&str>) -> Result<String, ToolOutput> {
    match path {
        Some(p) => Ok(p.to_string()),
        None => std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| ToolOutput::err(format!("cwd error: {e}"))),
    }
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn execute_glob(pattern: &str, path: Option<&str>) -> ToolOutput {
    let search_path = match resolve_search_path(path) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let mut overrides = OverrideBuilder::new(&search_path);
    if let Err(e) = overrides.add(pattern) {
        return ToolOutput::err(format!("invalid glob pattern: {e}"));
    }
    let overrides = match overrides.build() {
        Ok(o) => o,
        Err(e) => return ToolOutput::err(format!("glob build error: {e}")),
    };

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
        return ToolOutput::ok(NO_FILES_FOUND.to_string());
    }

    entries.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    entries.truncate(SEARCH_RESULT_LIMIT);

    let output = entries
        .into_iter()
        .map(|(_, p)| p)
        .collect::<Vec<_>>()
        .join("\n");
    ToolOutput::ok(output)
}

fn execute_grep(pattern: &str, path: Option<&str>, include: Option<&str>) -> ToolOutput {
    let search_path = match resolve_search_path(path) {
        Ok(p) => p,
        Err(e) => return e,
    };

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

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => return ToolOutput::err(format!("failed to run rg: {e}")),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Group matches by file, preserving insertion order
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
        return ToolOutput::ok(NO_FILES_FOUND.to_string());
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

    ToolOutput::ok(result.trim_end().to_string())
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

    #[test]
    fn execute_bash_large_output_does_not_deadlock() {
        let pipe_buf_overflow = "yes | head -n 100000";
        let result = execute_bash(pipe_buf_overflow, Some(10));
        assert!(!result.is_error);
        assert!(result.content.contains(TRUNCATED_MARKER));
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

    #[test]
    fn execute_glob_finds_and_misses() {
        let dir = temp_dir("maki_test_glob_find");
        fs::write(dir.join("a.txt"), "hello").unwrap();
        fs::write(dir.join("b.txt"), "world").unwrap();
        fs::write(dir.join("c.rs"), "fn main(){}").unwrap();
        let dir_str = dir.to_string_lossy();

        let hit = execute_glob("*.txt", Some(&dir_str));
        assert!(!hit.is_error);
        assert!(hit.content.contains("a.txt"));
        assert!(hit.content.contains("b.txt"));
        assert!(!hit.content.contains("c.rs"));

        let miss = execute_glob("*.nope", Some(&dir_str));
        assert!(!miss.is_error);
        assert_eq!(miss.content, NO_FILES_FOUND);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn execute_grep_finds_filters_and_misses() {
        let dir = temp_dir("maki_test_grep");
        fs::write(dir.join("a.txt"), "hello world\ngoodbye world").unwrap();
        fs::write(dir.join("b.rs"), "hello rust").unwrap();
        let dir_str = dir.to_string_lossy();

        let hit = execute_grep("hello", Some(&dir_str), None);
        assert!(!hit.is_error);
        assert!(hit.content.contains("a.txt"));
        assert!(hit.content.contains("b.rs"));

        let filtered = execute_grep("hello", Some(&dir_str), Some("*.rs"));
        assert!(!filtered.is_error);
        assert!(filtered.content.contains("b.rs"));
        assert!(!filtered.content.contains("a.txt"));

        let miss = execute_grep("zzzznotfound", Some(&dir_str), None);
        assert!(!miss.is_error);
        assert_eq!(miss.content, NO_FILES_FOUND);

        let _ = fs::remove_dir_all(&dir);
    }
}
