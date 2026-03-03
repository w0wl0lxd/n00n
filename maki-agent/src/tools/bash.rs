use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use maki_tool_macro::Tool;

use maki_providers::{AgentEvent, Envelope, ToolInput, ToolOutput};

use super::{relative_path, truncate_output};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const POLL_INTERVAL_MS: u64 = 10;
const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(100);

fn timed_out_msg(secs: u64) -> String {
    format!("command timed out after {secs}s")
}

#[derive(Tool, Debug, Clone)]
pub struct Bash {
    #[param(description = "The bash command to execute")]
    command: String,
    #[param(description = "Timeout in seconds (default 120)")]
    timeout: Option<u64>,
    #[param(description = "Working directory (default: cwd)")]
    workdir: Option<String>,
    #[param(description = "Short description (3-5 words) of what the command does")]
    description: Option<String>,
}

impl Bash {
    pub const NAME: &str = "bash";
    pub const DESCRIPTION: &str = include_str!("bash.md");
    pub const EXAMPLES: Option<&str> = Some(
        r#"[
  {"command": "cargo build --release", "description": "Build release binary"},
  {"command": "git diff HEAD~1", "description": "Show last commit diff"},
  {"command": "pytest tests/", "workdir": "/home/user/project", "timeout": 300, "description": "Run test suite"}
]"#,
    );

    fn resolved(&self) -> (&str, Option<&str>) {
        if self.workdir.is_some() {
            return (&self.command, self.workdir.as_deref());
        }
        if let Some(rest) = self.command.strip_prefix("cd ")
            && let Some(idx) = rest.find(" && ")
        {
            let dir = rest[..idx].trim();
            if !dir.is_empty() {
                return (&rest[idx + 4..], Some(dir));
            }
        }
        (&self.command, None)
    }

    pub fn execute(&self, ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        let timeout_secs = self.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let (command, workdir) = self.resolved();
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn().map_err(|e| format!("failed to spawn: {e}"))?;

        let (line_tx, line_rx) = mpsc::channel::<String>();
        if let Some(stdout) = child.stdout.take() {
            spawn_line_reader(stdout, line_tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_line_reader(stderr, line_tx.clone());
        }
        drop(line_tx);

        let mut output = String::new();
        let mut last_len = 0usize;
        let mut last_flush = Instant::now();

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            drain_available(&line_rx, &mut output);

            if let Some(id) = ctx.tool_use_id
                && last_flush.elapsed() >= STREAM_FLUSH_INTERVAL
                && output.len() > last_len
            {
                send_output(ctx.event_tx, id, &output);
                last_len = output.len();
                last_flush = Instant::now();
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    drain_remaining(&line_rx, &mut output);
                    if let Some(id) = ctx.tool_use_id
                        && output.len() > last_len
                    {
                        send_output(ctx.event_tx, id, &output);
                    }

                    let content = truncate_output(output);
                    if !status.success() {
                        if content.is_empty() {
                            return Err(format!(
                                "exited with code {}",
                                status.code().unwrap_or(-1)
                            ));
                        }
                        return Err(content);
                    }
                    return Ok(ToolOutput::Plain(content));
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        drain_remaining(&line_rx, &mut output);
                        let mut msg = timed_out_msg(timeout_secs);
                        if !output.is_empty() {
                            let content = truncate_output(output);
                            msg.push('\n');
                            msg.push_str(&content);
                        }
                        return Err(msg);
                    }
                    thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                }
                Err(e) => return Err(format!("wait error: {e}")),
            }
        }
    }

    pub fn start_summary(&self) -> String {
        let (command, workdir) = self.resolved();
        let mut s = self
            .description
            .clone()
            .unwrap_or_else(|| command.to_string());
        if let Some(dir) = workdir {
            s.push_str(" in ");
            s.push_str(&relative_path(dir));
        }
        s
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }

    pub fn start_output(&self) -> Option<ToolOutput> {
        None
    }

    pub fn start_input(&self) -> Option<ToolInput> {
        let (command, _) = self.resolved();
        Some(ToolInput::Code {
            language: "bash",
            code: command.to_string(),
        })
    }
}

fn spawn_line_reader(pipe: impl Read + Send + 'static, tx: Sender<String>) {
    thread::spawn(move || {
        let reader = BufReader::new(pipe);
        for line in reader.lines() {
            let line = line.unwrap_or_else(|_| "\u{FFFD}".into());
            if tx.send(line).is_err() {
                break;
            }
        }
    });
}

fn append_line(output: &mut String, line: &str) {
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(line);
}

fn drain_available(rx: &Receiver<String>, output: &mut String) {
    while let Ok(line) = rx.try_recv() {
        append_line(output, &line);
    }
}

fn drain_remaining(rx: &Receiver<String>, output: &mut String) {
    for line in rx.iter() {
        append_line(output, &line);
    }
}

fn send_output(event_tx: &Sender<Envelope>, id: &str, content: &str) {
    let _ = event_tx.send(
        AgentEvent::ToolOutput {
            id: id.to_string(),
            content: content.to_owned(),
        }
        .into(),
    );
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use crate::AgentMode;
    use crate::tools::test_support::stub_ctx;

    use super::*;

    fn bash(cmd: &str) -> Bash {
        Bash {
            command: cmd.into(),
            timeout: Some(5),
            workdir: None,
            description: None,
        }
    }

    #[test]
    fn execute_success_failure_timeout_and_workdir() {
        let ctx = stub_ctx(&AgentMode::Build);

        assert_eq!(
            bash("echo hello").execute(&ctx).unwrap().as_text().trim(),
            "hello"
        );
        assert!(bash("exit 1").execute(&ctx).is_err());

        let mut timeout = bash("sleep 10");
        timeout.timeout = Some(0);
        assert!(timeout.execute(&ctx).unwrap_err().contains("timed out"));

        let dir = tempfile::tempdir().unwrap();
        let mut in_dir = bash("pwd");
        in_dir.workdir = Some(dir.path().to_string_lossy().into());
        let output = in_dir.execute(&ctx).unwrap().as_text().to_string();
        assert!(
            output
                .trim()
                .ends_with(dir.path().file_name().unwrap().to_str().unwrap())
        );
    }

    #[test]
    fn large_output_is_truncated() {
        let ctx = stub_ctx(&AgentMode::Build);
        let mut b = bash("yes | head -n 100000");
        b.timeout = Some(10);
        assert!(b.execute(&ctx).unwrap().as_text().contains("[truncated]"));
    }

    #[test_case("ls",              None,           "ls",              None          ; "no_prefix")]
    #[test_case("cd /tmp && ls",   None,           "ls",              Some("/tmp")  ; "strips_cd")]
    #[test_case("cd /tmp && ls",   Some("/home"),  "cd /tmp && ls",   Some("/home") ; "explicit_workdir_wins")]
    #[test_case("cd  && ls",       None,           "cd  && ls",       None          ; "empty_dir_noop")]
    fn resolved_cases(cmd: &str, workdir: Option<&str>, exp_cmd: &str, exp_dir: Option<&str>) {
        let b = Bash {
            command: cmd.into(),
            timeout: None,
            workdir: workdir.map(Into::into),
            description: None,
        };
        assert_eq!(b.resolved(), (exp_cmd, exp_dir));
    }

    #[test_case(None, None, "ls",              "ls"               ; "falls_back_to_command")]
    #[test_case(Some("run tests"), None, "cargo test", "run tests"     ; "prefers_description")]
    #[test_case(Some("build"), Some("/tmp/proj"), "cargo build", "build in /tmp/proj" ; "appends_workdir")]
    #[test_case(None, None, "cd /tmp && ls", "ls in /tmp" ; "strips_cd_prefix")]
    #[test_case(Some("list"), None, "cd /tmp && ls", "list in /tmp" ; "strips_cd_prefix_with_desc")]
    fn start_summary_cases(desc: Option<&str>, workdir: Option<&str>, cmd: &str, expected: &str) {
        let b = Bash {
            command: cmd.into(),
            timeout: None,
            workdir: workdir.map(Into::into),
            description: desc.map(Into::into),
        };
        assert_eq!(b.start_summary(), expected);
    }
}
