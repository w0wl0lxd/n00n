use std::time::Duration;

use futures_lite::StreamExt;
use futures_lite::io::{AsyncBufReadExt, BufReader};

use async_process::Command;

use humantime::format_duration;

use maki_tool_macro::Tool;

use crate::{AgentEvent, EventSender, ToolInput, ToolOutput};

use super::{relative_path, truncate_output};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
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

    pub async fn execute(&self, ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        let timeout_secs = self.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let (command, workdir) = self.resolved();
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn().map_err(|e| format!("failed to spawn: {e}"))?;

        let (line_tx, line_rx) = flume::unbounded::<String>();
        if let Some(stdout) = child.stdout.take() {
            spawn_line_reader(BufReader::new(stdout), line_tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_line_reader(BufReader::new(stderr), line_tx.clone());
        }
        drop(line_tx);

        let mut output = String::new();
        let mut last_len = 0usize;
        let mut last_flush = std::time::Instant::now();

        let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
        enum Event {
            Line(Option<String>),
            Timeout,
        }
        loop {
            match futures_lite::future::race(
                async { Event::Line(line_rx.recv_async().await.ok()) },
                async {
                    async_io::Timer::at(deadline).await;
                    Event::Timeout
                },
            )
            .await
            {
                Event::Line(Some(l)) => append_line(&mut output, &l),
                Event::Line(None) => {
                    let status = child
                        .status()
                        .await
                        .map_err(|e| format!("wait error: {e}"))?;
                    flush_output(ctx, &output, &mut last_len);
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
                Event::Timeout => {
                    let _ = child.kill();
                    let _ = child.status().await;
                    drain_remaining(&line_rx, &mut output);
                    let mut msg = timed_out_msg(timeout_secs);
                    if !output.is_empty() {
                        let content = truncate_output(output);
                        msg.push('\n');
                        msg.push_str(&content);
                    }
                    return Err(msg);
                }
            }

            if let Some(ref id) = ctx.tool_use_id
                && last_flush.elapsed() >= STREAM_FLUSH_INTERVAL
                && output.len() > last_len
            {
                send_output(&ctx.event_tx, id, &output);
                last_len = output.len();
                last_flush = std::time::Instant::now();
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
}

impl super::ToolDefaults for Bash {
    fn start_input(&self) -> Option<ToolInput> {
        let (command, _) = self.resolved();
        Some(ToolInput::Code {
            language: "bash",
            code: command.to_string(),
        })
    }

    fn start_annotation(&self) -> Option<String> {
        let timeout = Duration::from_secs(self.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));
        let formatted: String = format_duration(timeout)
            .to_string()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        Some(format!("{formatted} timeout"))
    }
}

fn spawn_line_reader<R: futures_lite::io::AsyncRead + Unpin + Send + 'static>(
    reader: BufReader<R>,
    tx: flume::Sender<String>,
) {
    smol::spawn(async move {
        let mut lines = reader.lines();
        while let Some(line) = lines.next().await {
            let Ok(line) = line else { break };
            if tx.try_send(line).is_err() {
                break;
            }
        }
    })
    .detach();
}

fn append_line(output: &mut String, line: &str) {
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(line);
}

fn drain_remaining(rx: &flume::Receiver<String>, output: &mut String) {
    while let Ok(line) = rx.try_recv() {
        append_line(output, &line);
    }
}

fn flush_output(ctx: &super::ToolContext, output: &str, last_len: &mut usize) {
    if let Some(ref id) = ctx.tool_use_id
        && output.len() > *last_len
    {
        send_output(&ctx.event_tx, id, output);
        *last_len = output.len();
    }
}

fn send_output(event_tx: &EventSender, id: &str, content: &str) {
    event_tx.try_send(AgentEvent::ToolOutput {
        id: id.to_string(),
        content: content.to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use crate::AgentMode;
    use crate::tools::test_support::stub_ctx;

    use super::super::ToolDefaults;
    use super::*;

    fn bash(cmd: &str) -> Bash {
        Bash {
            command: cmd.into(),
            timeout: Some(10),
            workdir: None,
            description: None,
        }
    }

    #[test]
    fn execute_echo() {
        smol::block_on(async {
            let ctx = stub_ctx(&AgentMode::Build);
            let out = bash("echo hello").execute(&ctx).await.unwrap().as_text();
            assert_eq!(out.trim(), "hello");
        });
    }

    #[test]
    fn execute_nonzero_exit_is_error() {
        smol::block_on(async {
            let ctx = stub_ctx(&AgentMode::Build);
            assert!(bash("exit 1").execute(&ctx).await.is_err());
        });
    }

    #[test]
    fn execute_timeout() {
        smol::block_on(async {
            let ctx = stub_ctx(&AgentMode::Build);
            let mut b = bash("sleep 60");
            b.timeout = Some(1);
            assert!(b.execute(&ctx).await.unwrap_err().contains("timed out"));
        });
    }

    #[test]
    fn execute_workdir() {
        smol::block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let ctx = stub_ctx(&AgentMode::Build);
            let mut b = bash("pwd");
            b.workdir = Some(dir.path().to_string_lossy().into());
            let out = b.execute(&ctx).await.unwrap().as_text();
            assert!(
                out.trim()
                    .ends_with(dir.path().file_name().unwrap().to_str().unwrap())
            );
        });
    }

    #[test]
    fn large_output_is_truncated() {
        smol::block_on(async {
            let ctx = stub_ctx(&AgentMode::Build);
            let out = bash("yes | head -n 100000")
                .execute(&ctx)
                .await
                .unwrap()
                .as_text();
            assert!(out.contains("[truncated]"));
        });
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

    #[test_case(None,      "2m timeout"    ; "default_timeout")]
    #[test_case(Some(300), "5m timeout"    ; "custom_timeout")]
    #[test_case(Some(90),  "1m30s timeout" ; "mixed_timeout")]
    fn start_annotation_cases(timeout: Option<u64>, expected: &str) {
        let b = Bash {
            command: "ls".into(),
            timeout,
            workdir: None,
            description: None,
        };
        assert_eq!(b.start_annotation().unwrap(), expected);
    }
}
