use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use async_process::{Command, Stdio};
use futures_lite::io::AsyncReadExt;
use n00n_agent::{
    AgentConfig, CancelToken, CancelTrigger, ToolDoneEvent, ToolInput, ToolOutput, ToolStartEvent,
};
use n00n_config;
use n00n_providers::Message;

use super::App;

const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(100);
const SHELL_TIMEOUT: Duration = Duration::from_mins(5);
const OUTPUT_QUEUE_CAPACITY: usize = 64;
const OUTPUT_READ_CHUNK_SIZE: usize = 8 * 1024;
const MAX_UTF8_SEQUENCE_BYTES: usize = 4;

struct OutputLimits {
    lines: usize,
    bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellPrefix {
    pub prefix_len: usize,
    pub command: String,
    pub visible: bool,
}

pub(crate) fn parse_shell_prefix(text: &str) -> Option<ShellPrefix> {
    let (sigil_len, visible) = if text.starts_with("!!") {
        (2, false)
    } else if text.starts_with('!') {
        (1, true)
    } else {
        return None;
    };
    let rest = &text[sigil_len..];
    let prefix_len = if rest.starts_with(' ') {
        sigil_len + 1
    } else {
        sigil_len
    };
    let command = rest.trim();
    if command.is_empty() {
        return None;
    }
    Some(ShellPrefix {
        prefix_len,
        command: command.to_owned(),
        visible,
    })
}

pub(crate) enum ShellEvent {
    Start {
        id: String,
        command: String,
    },
    Output {
        id: String,
        content: String,
    },
    Done {
        id: String,
        command: String,
        output: String,
        is_error: bool,
        visible: bool,
    },
}

#[derive(Default)]
pub(crate) struct ShellState {
    cancel_triggers: Vec<CancelTrigger>,
    pending_results: Vec<Message>,
    id_counter: u64,
}

impl ShellState {
    pub fn next_id(&mut self) -> String {
        self.id_counter += 1;
        format!("shell-{}", self.id_counter)
    }

    pub fn add_trigger(&mut self, trigger: CancelTrigger) {
        self.cancel_triggers.push(trigger);
    }

    pub fn cancel_all(&mut self) {
        for trigger in self.cancel_triggers.drain(..) {
            trigger.cancel();
        }
    }

    pub fn push_result(&mut self, msg: Message) {
        self.pending_results.push(msg);
    }

    pub fn drain_results(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.pending_results)
    }

    pub fn restore_results(&mut self, results: Vec<Message>) {
        if results.is_empty() {
            return;
        }
        self.pending_results.splice(0..0, results);
    }
}

impl App {
    pub(crate) fn handle_shell_event(&mut self, event: ShellEvent) {
        match event {
            ShellEvent::Start { id, command } => {
                self.main_chat().shell_tool_start(ToolStartEvent {
                    id,
                    tool: "bash".into(),
                    summary: command.clone(),
                    annotation: None,
                    input: Some(ToolInput::Code {
                        language: "bash".into(),
                        code: command,
                    }),
                    raw_input: None,
                    output: None,
                    render_header: None,
                });
            }
            ShellEvent::Output { id, content } => {
                self.main_chat().shell_tool_output(&id, &content);
            }
            ShellEvent::Done {
                id,
                command,
                output,
                is_error,
                visible,
            } => {
                let result_msg = if visible {
                    let label = if is_error { "Error" } else { "Output" };
                    Some(Message::user(format!(
                        "I ran: $ {command}\n\n{label}:\n{output}"
                    )))
                } else {
                    None
                };
                self.main_chat().shell_tool_done(ToolDoneEvent {
                    id,
                    tool: "bash".into(),
                    output: ToolOutput::Plain(output.into()),
                    is_error,
                    annotation: None,
                    written_path: None,
                });
                if let Some(msg) = result_msg {
                    self.shell.push_result(msg);
                }
            }
        }
    }
}

pub(crate) fn spawn_shell(
    command: String,
    id: String,
    visible: bool,
    tx: flume::Sender<ShellEvent>,
    cancel: CancelToken,
    config: &AgentConfig,
) {
    let max_output_lines = config.max_output_lines;
    let max_output_bytes = config.max_output_bytes;
    smol::spawn(async move {
        let _ = tx.send(ShellEvent::Start {
            id: id.clone(),
            command: command.clone(),
        });

        let result = run_command(
            &command,
            &id,
            &tx,
            &cancel,
            max_output_lines,
            max_output_bytes,
        )
        .await;

        let (output, is_error) = match result {
            Ok(out) => (out, false),
            Err(err) => (err, true),
        };

        let _ = tx.send(ShellEvent::Done {
            id,
            command,
            output,
            is_error,
            visible,
        });
    })
    .detach();
}
#[allow(unsafe_code)]
async fn run_command(
    command: &str,
    id: &str,
    tx: &flume::Sender<ShellEvent>,
    cancel: &CancelToken,
    max_output_lines: usize,
    max_output_bytes: usize,
) -> Result<String, String> {
    let mut std_cmd: StdCommand = n00n_config::bash_command(command)?;
    std_cmd.env("GIT_TERMINAL_PROMPT", "0");

    #[cfg(unix)]
    unsafe {
        std_cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let mut cmd: Command = std_cmd.into();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("failed to spawn: {e}"))?;

    let (output_tx, output_rx) = flume::bounded::<Vec<u8>>(OUTPUT_QUEUE_CAPACITY);
    if let Some(stdout) = child.stdout.take() {
        spawn_output_reader(stdout, output_tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_output_reader(stderr, output_tx.clone());
    }
    let mut guard = n00n_agent::ChildGuard::new(child);
    drop(output_tx);

    let mut output = Vec::new();
    let mut line_count = 0;
    let mut line_start = true;
    let mut pending_newline = false;
    let mut truncated = false;
    let mut last_flush = Instant::now();
    let deadline = Instant::now() + SHELL_TIMEOUT;

    macro_rules! race_deadline {
        ($future:expr) => {
            futures_lite::future::race(
                $future,
                futures_lite::future::race(
                    async {
                        smol::Timer::at(deadline).await;
                        Err(format!("timed out after {}s", SHELL_TIMEOUT.as_secs()))
                    },
                    async {
                        cancel.cancelled().await;
                        Err("cancelled".to_string())
                    },
                ),
            )
            .await
        };
    }

    loop {
        let chunk = race_deadline!(async { Ok(output_rx.recv_async().await.ok()) });
        match chunk {
            Ok(Some(chunk)) => {
                if !truncated {
                    truncated = append_output(
                        &mut output,
                        &chunk,
                        &mut line_count,
                        &mut line_start,
                        &mut pending_newline,
                        &OutputLimits {
                            lines: max_output_lines,
                            bytes: max_output_bytes,
                        },
                    );
                }
            }
            Ok(None) => break,
            Err(e) => {
                guard.kill_and_reap().await;
                return Err(e);
            }
        }

        if last_flush.elapsed() >= STREAM_FLUSH_INTERVAL && !output.is_empty() {
            flush_output(tx, id, &output_text(&output));
            last_flush = Instant::now();
        }
    }

    let status =
        race_deadline!(async { guard.status().await.map_err(|e| format!("wait error: {e}")) });
    match status {
        Ok(status) => {
            let mut output = output_text(&output);
            flush_output(tx, id, &output);
            if truncated {
                output.push_str("\n[truncated]");
            }
            if !status.success() {
                if output.is_empty() {
                    return Err(format!(
                        "exited with code {}",
                        status.code().unwrap_or_else(|| -1)
                    ));
                }
                return Err(output);
            }
            Ok(output)
        }
        Err(e) => {
            guard.kill_and_reap().await;
            Err(e)
        }
    }
}

fn flush_output(tx: &flume::Sender<ShellEvent>, id: &str, output: &str) {
    let _ = tx.send(ShellEvent::Output {
        id: id.to_string(),
        content: output.to_string(),
    });
}

fn append_output(
    output: &mut Vec<u8>,
    chunk: &[u8],
    line_count: &mut usize,
    line_start: &mut bool,
    pending_newline: &mut bool,
    limits: &OutputLimits,
) -> bool {
    for &byte in chunk {
        if *line_start {
            if *line_count >= limits.lines {
                return true;
            }
            if *pending_newline {
                if output.len() >= limits.bytes {
                    return true;
                }
                output.push(b'\n');
                *pending_newline = false;
            }
            *line_count += 1;
            *line_start = false;
        }

        if byte == b'\n' {
            *line_start = true;
            *pending_newline = true;
        } else {
            if output.len() >= limits.bytes {
                return true;
            }
            output.push(byte);
        }
    }
    false
}

fn output_text(output: &[u8]) -> String {
    let search_start = output.len().saturating_sub(MAX_UTF8_SEQUENCE_BYTES);
    let complete_len = match (search_start..output.len()).find_map(|offset| {
        std::str::from_utf8(&output[offset..])
            .err()
            .filter(|error| error.error_len().is_none())
            .map(|error| offset + error.valid_up_to())
    }) {
        Some(complete_len) => complete_len,
        None => output.len(),
    };
    String::from_utf8_lossy(&output[..complete_len]).into_owned()
}

fn spawn_output_reader<R: futures_lite::io::AsyncRead + Unpin + Send + 'static>(
    reader: R,
    tx: flume::Sender<Vec<u8>>,
) {
    smol::spawn(read_output(reader, tx)).detach();
}

async fn read_output<R: futures_lite::io::AsyncRead + Unpin>(
    mut reader: R,
    tx: flume::Sender<Vec<u8>>,
) {
    let mut buffer = [0; OUTPUT_READ_CHUNK_SIZE];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if tx.send_async(buffer[..read].to_vec()).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;

    use futures_lite::future::poll_once;
    use futures_lite::io::Cursor;
    use test_case::test_case;

    use super::*;

    #[test_case("! ls",                     Some(&ShellPrefix { prefix_len: 2, command: "ls".into(), visible: true })           ; "simple_visible")]
    #[test_case("!! ls",                    Some(&ShellPrefix { prefix_len: 3, command: "ls".into(), visible: false })          ; "simple_anonymous")]
    #[test_case("! cargo test --release",   Some(&ShellPrefix { prefix_len: 2, command: "cargo test --release".into(), visible: true })  ; "multi_word_command")]
    #[test_case("!! cargo build",           Some(&ShellPrefix { prefix_len: 3, command: "cargo build".into(), visible: false }) ; "multi_word_anonymous")]
    #[test_case("! ",                       None                        ; "bang_space_only")]
    #[test_case("!",                        None                        ; "bang_alone")]
    #[test_case("!!",                       None                        ; "double_bang_alone")]
    #[test_case("!! ",                      None                        ; "double_bang_space_only")]
    #[test_case("hello ! world",            None                        ; "bang_mid_string")]
    #[test_case(" ! ls",                    None                        ; "leading_space")]
    #[test_case("!echo hi",                 Some(&ShellPrefix { prefix_len: 1, command: "echo hi".into(), visible: true })      ; "no_space_after_bang")]
    #[test_case("!!echo hi",                Some(&ShellPrefix { prefix_len: 2, command: "echo hi".into(), visible: false })     ; "no_space_after_double_bang")]
    #[test_case("!  ls",                    Some(&ShellPrefix { prefix_len: 2, command: "ls".into(), visible: true })           ; "extra_spaces_trimmed")]
    fn parse_shell_prefix_cases(input: &str, expected: Option<&ShellPrefix>) {
        assert_eq!(parse_shell_prefix(input).as_ref(), expected);
    }

    #[test]
    fn output_staging_applies_async_backpressure() {
        smol::block_on(async {
            let input = vec![b'x'; OUTPUT_READ_CHUNK_SIZE * (OUTPUT_QUEUE_CAPACITY + 1)];
            let reader = Cursor::new(input);
            let (tx, rx) = flume::bounded(OUTPUT_QUEUE_CAPACITY);
            let mut read = pin!(read_output(reader, tx));

            assert!(poll_once(read.as_mut()).await.is_none());
            assert_eq!(rx.len(), OUTPUT_QUEUE_CAPACITY);
            assert_eq!(rx.recv_async().await.unwrap().len(), OUTPUT_READ_CHUNK_SIZE);

            assert!(poll_once(read.as_mut()).await.is_some());
            while let Ok(chunk) = rx.try_recv() {
                assert_eq!(chunk.len(), OUTPUT_READ_CHUNK_SIZE);
            }
        });
    }

    #[test]
    fn byte_limit_does_not_overshoot_or_split_utf8() {
        let mut output = Vec::new();
        let mut line_count = 0;
        let mut line_start = true;
        let mut pending_newline = false;

        let truncated = append_output(
            &mut output,
            "aéz".as_bytes(),
            &mut line_count,
            &mut line_start,
            &mut pending_newline,
            &OutputLimits {
                lines: usize::MAX,
                bytes: 2,
            },
        );

        assert!(truncated);
        assert!(output.len() <= 2);
        assert_eq!(output_text(&output), "a");
    }

    #[test]
    fn invalid_utf8_does_not_trim_valid_output_tail() {
        assert_eq!(output_text(&[0xff, b'a', b'b', b'c']), "�abc");
    }

    #[test]
    fn incomplete_utf8_tail_is_removed() {
        assert_eq!(output_text(&[b'a', 0xc3]), "a");
    }

    #[test]
    fn incomplete_utf8_tail_is_removed_after_invalid_bytes() {
        assert_eq!(output_text(&[0xff, b'a', 0xc3]), "�a");
    }

    #[test]
    fn command_output_is_flushed_before_truncated_result() {
        smol::block_on(async {
            let (_trigger, cancel) = CancelToken::new();
            let (tx, rx) = flume::unbounded();
            let result = run_command(
                "printf 'one\\ntwo\\nthree\\n'",
                "test-shell",
                &tx,
                &cancel,
                2,
                usize::MAX,
            )
            .await
            .unwrap();

            assert_eq!(result, "one\ntwo\n[truncated]");
            match rx.recv_async().await.unwrap() {
                ShellEvent::Output { id, content } => {
                    assert_eq!(id, "test-shell");
                    assert_eq!(content, "one\ntwo");
                }
                _ => panic!("expected output before completion"),
            }
        });
    }
}
