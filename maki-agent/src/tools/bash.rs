use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use maki_tool_macro::Tool;

use maki_providers::{ToolInput, ToolOutput};

use super::truncate_output;

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const POLL_INTERVAL_MS: u64 = 10;

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

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        let timeout_secs = self.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&self.command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &self.workdir {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn().map_err(|e| format!("failed to spawn: {e}"))?;

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
                        return Err(timed_out_msg(timeout_secs));
                    }
                    thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                }
                Err(e) => return Err(format!("wait error: {e}")),
            }
        }
    }

    pub fn start_summary(&self) -> String {
        let mut s = self
            .description
            .clone()
            .unwrap_or_else(|| self.command.clone());
        if let Some(dir) = &self.workdir {
            s.push_str(" in ");
            s.push_str(dir);
        }
        s
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
    pub fn start_input(&self) -> Option<ToolInput> {
        Some(ToolInput::Code {
            language: "bash",
            code: self.command.clone(),
        })
    }
}

fn read_pipe_lossy(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
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

        let mut bad_dir = bash("echo hi");
        bad_dir.workdir = Some("/nonexistent_dir_12345".into());
        assert!(bad_dir.execute(&ctx).is_err());
    }

    #[test]
    fn large_output_is_truncated() {
        let ctx = stub_ctx(&AgentMode::Build);
        let mut b = bash("yes | head -n 100000");
        b.timeout = Some(10);
        assert!(b.execute(&ctx).unwrap().as_text().contains("[truncated]"));
    }

    #[test_case(None, None, "ls",              "ls"               ; "falls_back_to_command")]
    #[test_case(Some("run tests"), None, "cargo test", "run tests"     ; "prefers_description")]
    #[test_case(Some("build"), Some("/tmp/proj"), "cargo build", "build in /tmp/proj" ; "appends_workdir")]
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
