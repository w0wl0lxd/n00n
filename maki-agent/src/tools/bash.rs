use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use maki_tool_macro::Tool;

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
}

impl Bash {
    pub const NAME: &str = "bash";
    pub const DESCRIPTION: &str = include_str!("bash.md");

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<String, String> {
        let timeout_secs = self.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(&self.command)
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
                            return Err(format!(
                                "exited with code {}",
                                status.code().unwrap_or(-1)
                            ));
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
                    thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                }
                Err(e) => return Err(format!("wait error: {e}")),
            }
        }
    }

    pub fn start_summary(&self) -> String {
        self.command.clone()
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
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
    use crate::AgentMode;
    use crate::tools::test_support::stub_ctx;

    use super::*;

    #[test]
    fn bash_success_failure_and_timeout() {
        let ctx = stub_ctx(&AgentMode::Build);

        let ok = Bash {
            command: "echo hello".into(),
            timeout: Some(5),
        };
        assert_eq!(ok.execute(&ctx).unwrap().trim(), "hello");

        let fail = Bash {
            command: "exit 1".into(),
            timeout: Some(5),
        };
        assert!(fail.execute(&ctx).is_err());

        let timeout = Bash {
            command: "sleep 10".into(),
            timeout: Some(0),
        };
        assert!(
            timeout
                .execute(&ctx)
                .unwrap_err()
                .contains(&timed_out_msg(0))
        );
    }

    #[test]
    fn bash_large_output_does_not_deadlock() {
        let ctx = stub_ctx(&AgentMode::Build);
        let bash = Bash {
            command: "yes | head -n 100000".into(),
            timeout: Some(10),
        };
        assert!(bash.execute(&ctx).unwrap().contains("[truncated]"));
    }
}
