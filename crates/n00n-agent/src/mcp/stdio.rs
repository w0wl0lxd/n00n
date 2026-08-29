#![allow(clippy::cast_possible_truncation)]

use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::{Duration, Instant};

use async_lock::Mutex;

use futures_lite::io::BufReader;
use futures_lite::{AsyncBufReadExt, AsyncWriteExt};
use serde_json::Value;
use smol::channel;
use tracing::{debug, info, warn};

use super::error::McpError;
use super::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use super::transport::{BoxFuture, McpTransport};

type PendingMap = HashMap<u64, channel::Sender<Result<Value, McpError>>>;

const LINE_DELIMITER: u8 = b'\n';

/// Caps memory used while buffering one JSON-RPC line from a server's stdout.
/// A server that exceeds this is malfunctioning or hostile; the line is
/// dropped instead of fully buffered. A live n00n process was observed at
/// 13.9 GiB RSS from an unbounded read before this cap existed.
#[cfg(not(test))]
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
#[cfg(test)]
const MAX_FRAME_BYTES: usize = 128;

use crate::ChildGuard;

fn log_stderr_line(server: &str, line: &str) {
    if looks_like_warning_or_error(line) {
        warn!(server, "{line}");
    } else {
        debug!(server, "{line}");
    }
}

/// A death callback invoked once, from the reader task, when the transport's
/// connection to the server ends for any reason.
pub type DeathCallback = Box<dyn Fn(McpError) + Send + Sync>;

/// Outcome of reading one newline-delimited frame with `read_bounded_line`.
enum BoundedLine {
    /// The stream ended with no bytes read for this frame.
    Eof,
    /// A complete line, at or under the byte cap.
    Line(Vec<u8>),
    /// The line exceeded the cap; excess bytes were discarded, not buffered.
    Truncated { discarded_bytes: usize },
}

/// Reads one newline-delimited frame, buffering at most `max_bytes`. Bytes
/// beyond the cap are consumed from the stream (to resync at the next line)
/// but never copied into memory, bounding RSS regardless of frame size.
async fn read_bounded_line(
    reader: &mut (impl AsyncBufReadExt + Unpin),
    max_bytes: usize,
) -> std::io::Result<BoundedLine> {
    let mut buf: Vec<u8> = Vec::new();
    let mut discarded = 0usize;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() && discarded == 0 {
                BoundedLine::Eof
            } else if discarded > 0 {
                BoundedLine::Truncated {
                    discarded_bytes: discarded,
                }
            } else {
                BoundedLine::Line(buf)
            });
        }
        let delimiter_pos = chunk.iter().position(|&b| b == LINE_DELIMITER);
        let scan_len = match delimiter_pos {
            Some(position) => position,
            None => chunk.len(),
        };
        let room = max_bytes.saturating_sub(buf.len());
        let take = scan_len.min(room);
        buf.extend_from_slice(&chunk[..take]);
        discarded += scan_len - take;
        let consumed = delimiter_pos.map_or(chunk.len(), |pos| pos + 1);
        reader.consume(consumed);
        if delimiter_pos.is_some() {
            return Ok(if discarded > 0 {
                BoundedLine::Truncated {
                    discarded_bytes: discarded,
                }
            } else {
                BoundedLine::Line(buf)
            });
        }
    }
}

fn looks_like_warning_or_error(line: &str) -> bool {
    line.split_whitespace().take(3).any(|token| {
        matches!(
            token.trim_start_matches('[').trim_end_matches(']'),
            "ERROR" | "WARN" | "FATAL" | "CRITICAL"
        )
    })
}

/// What to do with an incoming stderr line: log it, or (if it repeats the
/// previous line verbatim) fold it into a trailing repeat count instead of
/// re-logging a busy child's retry loop one line at a time.
enum StderrDedupAction {
    Log(String),
    Suppress,
    FlushThenLog {
        previous: String,
        repeats: usize,
        current: String,
    },
}

/// Collapses consecutive identical stderr lines from one MCP child into a
/// single log line plus a repeat count, so a child stuck retrying (e.g. a
/// connection refused every poll) does not flood the log one line per retry.
struct StderrDedup {
    last: Option<String>,
    repeats: usize,
}

impl StderrDedup {
    fn new() -> Self {
        Self {
            last: None,
            repeats: 0,
        }
    }

    fn observe(&mut self, line: &str) -> StderrDedupAction {
        if self.last.as_deref() == Some(line) {
            self.repeats += 1;
            return StderrDedupAction::Suppress;
        }
        let repeats = self.repeats;
        self.repeats = 0;
        match self.last.replace(line.to_owned()) {
            Some(previous) if repeats > 0 => StderrDedupAction::FlushThenLog {
                previous,
                repeats,
                current: line.to_owned(),
            },
            _ => StderrDedupAction::Log(line.to_owned()),
        }
    }

    /// Call once the stream ends, to report a trailing run of repeats that
    /// never got interrupted by a different line.
    fn flush(&mut self) -> Option<(String, usize)> {
        if self.repeats == 0 {
            return None;
        }
        let repeats = self.repeats;
        self.repeats = 0;
        self.last.clone().map(|line| (line, repeats))
    }
}

struct PendingRequestGuard {
    pending: Arc<Mutex<PendingMap>>,
    id: u64,
    armed: bool,
}

impl PendingRequestGuard {
    fn new(pending: Arc<Mutex<PendingMap>>, id: u64) -> Self {
        Self {
            pending,
            id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(mut pending) = self.pending.try_lock() {
            pending.remove(&self.id);
            return;
        }
        let pending = Arc::clone(&self.pending);
        let id = self.id;
        smol::spawn(async move {
            pending.lock().await.remove(&id);
        })
        .detach();
    }
}

pub struct StdioTransport {
    name: Arc<str>,
    stdin: Mutex<async_process::ChildStdin>,
    pending: Arc<Mutex<PendingMap>>,
    next_id: AtomicU64,
    timeout: Duration,
    alive: Arc<AtomicBool>,
    established: Arc<AtomicBool>,
    _reader_task: smol::Task<()>,
    _stderr_task: smol::Task<()>,
    child: StdMutex<ChildGuard>,
}

impl StdioTransport {
    /// Spawn a new stdio MCP transport.
    ///
    /// # Errors
    ///
    /// Returns an error if the child process cannot be spawned or initialized.
    #[allow(unsafe_code)]
    pub fn spawn(
        name: &str,
        program: &str,
        args: &[String],
        environment: &HashMap<String, String>,
        timeout: Duration,
        on_death: DeathCallback,
    ) -> Result<Self, McpError> {
        let mut std_cmd = std::process::Command::new(program);
        std_cmd.args(args).envs(environment);

        #[cfg(unix)]
        unsafe {
            std_cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }

        let mut cmd: async_process::Command = std_cmd.into();
        cmd.stdin(async_process::Stdio::piped())
            .stdout(async_process::Stdio::piped())
            .stderr(async_process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| McpError::StartFailed {
            server: name.into(),
            reason: e.to_string(),
        })?;

        let stdin = child.stdin.take().ok_or_else(|| McpError::StartFailed {
            server: name.into(),
            reason: "no stdin".into(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| McpError::StartFailed {
            server: name.into(),
            reason: "no stdout".into(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| McpError::StartFailed {
            server: name.into(),
            reason: "no stderr".into(),
        })?;

        let name: Arc<str> = Arc::from(name);
        let alive = Arc::new(AtomicBool::new(true));
        let established = Arc::new(AtomicBool::new(false));
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));

        let reader_task = {
            let name = Arc::clone(&name);
            let alive = Arc::clone(&alive);
            let established = Arc::clone(&established);
            let pending = Arc::clone(&pending);
            smol::spawn(async move {
                let result = Self::reader_loop(&name, &mut BufReader::new(stdout), &pending).await;
                let terminal_err = result.err().unwrap_or_else(|| McpError::ServerDied {
                    server: (*name).into(),
                });
                // Clear liveness first, whatever the reason. `start_server` can
                // be between the last handshake response and `mark_established`,
                // and a transport still marked alive there would be published as
                // initialized with a dead reader behind it. `mark_established`
                // reads `alive` under the same `SeqCst` order, so one side always
                // observes the other.
                let was_alive = alive.swap(false, Ordering::SeqCst);
                if !established.load(Ordering::SeqCst) {
                    // start_server owns this attempt and reports its own failure via
                    // apply_start_result; an independent warn here would just duplicate it.
                    debug!(server = &*name, error = %terminal_err, "MCP reader loop ended before handshake completed");
                } else if was_alive {
                    warn!(server = &*name, error = %terminal_err, "MCP reader loop ended");
                    on_death(terminal_err.clone());
                } else {
                    debug!(server = &*name, error = %terminal_err, "MCP reader loop ended after shutdown");
                }
                for (_, sender) in pending.lock().await.drain() {
                    let _ = sender.send(Err(terminal_err.clone())).await;
                }
            })
        };

        let stderr_task = {
            let name = Arc::clone(&name);
            smol::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                let mut dedup = StderrDedup::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => {
                            if let Some((previous, repeats)) = dedup.flush() {
                                log_stderr_line(
                                    &name,
                                    &format!("{previous} (repeated {repeats} more times)"),
                                );
                            }
                            break;
                        }
                        Ok(_) => {
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            match dedup.observe(trimmed) {
                                StderrDedupAction::Suppress => {}
                                StderrDedupAction::Log(l) => log_stderr_line(&name, &l),
                                StderrDedupAction::FlushThenLog {
                                    previous,
                                    repeats,
                                    current,
                                } => {
                                    log_stderr_line(
                                        &name,
                                        &format!("{previous} (repeated {repeats} more times)"),
                                    );
                                    log_stderr_line(&name, &current);
                                }
                            }
                        }
                    }
                }
            })
        };

        Ok(Self {
            name,
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            timeout,
            alive,
            established,
            _reader_task: reader_task,
            _stderr_task: stderr_task,
            child: StdMutex::new(ChildGuard::new(child)),
        })
    }

    async fn reader_loop(
        name: &Arc<str>,
        reader: &mut (impl AsyncBufReadExt + Unpin),
        pending: &Mutex<PendingMap>,
    ) -> Result<(), McpError> {
        loop {
            let outcome = read_bounded_line(reader, MAX_FRAME_BYTES)
                .await
                .map_err(|e| McpError::ServerDied {
                    server: format!("{}: read failed: {e}", &**name),
                })?;

            let bytes = match outcome {
                BoundedLine::Eof => {
                    return Err(McpError::ServerDied {
                        server: (**name).into(),
                    });
                }
                BoundedLine::Truncated { discarded_bytes } => {
                    warn!(
                        server = &**name,
                        discarded_bytes,
                        limit_bytes = MAX_FRAME_BYTES,
                        "MCP response exceeded frame size limit; dropping connection"
                    );
                    return Err(McpError::ResponseTooLarge {
                        server: (**name).into(),
                        limit_bytes: MAX_FRAME_BYTES,
                    });
                }
                BoundedLine::Line(bytes) => bytes,
            };

            let line = String::from_utf8_lossy(&bytes);
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            match serde_json::from_str::<JsonRpcResponse>(trimmed) {
                Ok(resp) => {
                    if let Some(id) = resp.id {
                        if let Some(sender) = pending.lock().await.remove(&id) {
                            let result = if let Some(err) = resp.error {
                                Err(McpError::RpcError {
                                    server: (**name).into(),
                                    code: err.code,
                                    message: err.message,
                                })
                            } else {
                                let result = resp.result.unwrap_or_else(|| Value::Null);
                                Ok(result)
                            };
                            let _ = sender.send(result).await;
                        } else {
                            debug!(server = &**name, id, "response for unknown request id");
                        }
                    } else {
                        debug!(server = &**name, "received notification (no id)");
                    }
                }
                Err(e) => {
                    debug!(server = &**name, error = %e, line = trimmed, "non-JSON-RPC line from server");
                }
            }
        }
    }

    fn server(&self) -> String {
        self.name.to_string()
    }

    fn child_guard(&self) -> MutexGuard<'_, ChildGuard> {
        match self.child.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!(server = %self.name, "child guard lock poisoned");
                poisoned.into_inner()
            }
        }
    }

    async fn write_line(&self, line: &[u8]) -> Result<(), McpError> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line)
            .await
            .map_err(|e| McpError::WriteFailed {
                server: self.server(),
                reason: e.to_string(),
            })?;
        stdin.flush().await.map_err(|e| McpError::WriteFailed {
            server: self.server(),
            reason: e.to_string(),
        })
    }

    fn server_died(&self) -> McpError {
        McpError::ServerDied {
            server: self.server(),
        }
    }

    fn serialize(&self, value: &impl serde::Serialize) -> Result<Vec<u8>, McpError> {
        let mut buf = serde_json::to_vec(value).map_err(|e| McpError::InvalidResponse {
            server: self.server(),
            reason: e.to_string(),
        })?;
        buf.push(LINE_DELIMITER);
        Ok(buf)
    }
}

impl McpTransport for StdioTransport {
    fn send_request<'a>(
        &'a self,
        method: &'a str,
        params: Option<Value>,
    ) -> BoxFuture<'a, Result<Value, McpError>> {
        Box::pin(async move {
            if !self.alive.load(Ordering::Acquire) {
                return Err(self.server_died());
            }

            let start = Instant::now();
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let req = JsonRpcRequest::new(id, method, params);

            let (tx, rx) = smol::channel::bounded(1);
            self.pending.lock().await.insert(id, tx);
            let mut pending_guard = PendingRequestGuard::new(Arc::clone(&self.pending), id);
            if let Err(e) = self.write_line(&self.serialize(&req)?).await {
                self.pending.lock().await.remove(&id);
                pending_guard.disarm();
                return Err(e);
            }

            let timeout_ms = u64::try_from(self.timeout.as_millis()).unwrap_or_else(|_| u64::MAX);
            let result = futures_lite::future::race(
                async { rx.recv().await.unwrap_or_else(|_| Err(self.server_died())) },
                async {
                    async_io::Timer::after(self.timeout).await;
                    Err(McpError::Timeout {
                        server: self.server(),
                        timeout_ms,
                    })
                },
            )
            .await;

            if result.is_err() {
                self.pending.lock().await.remove(&id);
            } else {
                let duration_ms =
                    u64::try_from(start.elapsed().as_millis()).unwrap_or_else(|_| u64::MAX);
                info!(server = %self.server(), method, id, duration_ms, "MCP stdio response");
            }
            pending_guard.disarm();

            result
        })
    }

    fn send_notification<'a>(
        &'a self,
        method: &'a str,
        params: Option<Value>,
    ) -> BoxFuture<'a, Result<(), McpError>> {
        Box::pin(async move {
            let notif = JsonRpcNotification::new(method, params);
            self.write_line(&self.serialize(&notif)?).await
        })
    }

    fn begin_shutdown(&self) {
        self.alive.store(false, Ordering::Release);
        self.child_guard().begin_shutdown();
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        self.begin_shutdown();
        let reap = {
            let mut child = self.child_guard();
            child.reap()
        };
        Box::pin(reap)
    }

    fn server_name(&self) -> &Arc<str> {
        &self.name
    }

    fn transport_kind(&self) -> &'static str {
        "stdio"
    }

    fn mark_established(&self) -> bool {
        self.established.store(true, Ordering::SeqCst);
        self.alive.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::io::Cursor;
    use test_case::test_case;

    async fn read_single_response(input: &str) -> Result<Value, McpError> {
        let pending: Mutex<PendingMap> = Mutex::new(HashMap::new());
        let name: Arc<str> = Arc::from("test");

        let (tx, rx) = channel::bounded(1);
        pending.lock().await.insert(1, tx);

        let mut reader = BufReader::new(Cursor::new(input.as_bytes().to_vec()));
        let _ = StdioTransport::reader_loop(&name, &mut reader, &pending).await;

        rx.try_recv().unwrap_or_else(|_| {
            Err(McpError::ServerDied {
                server: "no response received".into(),
            })
        })
    }

    #[test]
    fn dropped_pending_request_removes_pending_entry() {
        smol::block_on(async {
            let pending = Arc::new(Mutex::new(HashMap::new()));
            let (sender, _receiver) = channel::bounded(1);
            pending.lock().await.insert(1, sender);

            drop(PendingRequestGuard::new(Arc::clone(&pending), 1));

            assert!(pending.lock().await.is_empty());
        });
    }

    #[test_case("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n" ; "lf_terminated")]
    #[test_case("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\r\n" ; "crlf_terminated")]
    #[test_case("  {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}  \n" ; "whitespace_padded")]
    #[test_case("\n\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n" ; "blank_lines_before")]
    #[test_case("not json\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n" ; "invalid_json_before")]
    fn reader_parses_valid_response(input: &str) {
        smol::block_on(async {
            assert!(read_single_response(input).await.is_ok());
        });
    }

    #[test]
    fn reader_returns_rpc_error() {
        let input =
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32600,\"message\":\"bad\"}}\n";
        smol::block_on(async {
            assert!(matches!(
                read_single_response(input).await,
                Err(McpError::RpcError { code: -32600, .. })
            ));
        });
    }

    #[test]
    fn reader_loop_drops_frame_exceeding_size_cap() {
        let oversized = "x".repeat(MAX_FRAME_BYTES * 2);
        let input = format!("{oversized}\n");
        smol::block_on(async {
            let pending: Mutex<PendingMap> = Mutex::new(HashMap::new());
            let name: Arc<str> = Arc::from("test");
            let mut reader = BufReader::new(Cursor::new(input.into_bytes()));
            let result = StdioTransport::reader_loop(&name, &mut reader, &pending).await;
            assert!(matches!(
                result,
                Err(McpError::ResponseTooLarge { limit_bytes, .. }) if limit_bytes == MAX_FRAME_BYTES
            ));
        });
    }

    #[test]
    fn read_bounded_line_returns_line_at_cap() {
        let input = "hello\n";
        smol::block_on(async {
            let mut reader = BufReader::new(Cursor::new(input.as_bytes().to_vec()));
            let outcome = read_bounded_line(&mut reader, 16).await.unwrap();
            assert!(matches!(outcome, BoundedLine::Line(bytes) if bytes == b"hello"));
        });
    }

    #[test]
    fn read_bounded_line_truncates_oversized_frame_without_unbounded_growth() {
        let input = format!("{}\n", "x".repeat(64));
        smol::block_on(async {
            let mut reader = BufReader::new(Cursor::new(input.into_bytes()));
            let outcome = read_bounded_line(&mut reader, 16).await.unwrap();
            assert!(matches!(
                outcome,
                BoundedLine::Truncated { discarded_bytes } if discarded_bytes == 48
            ));
        });
    }

    #[test]
    fn read_bounded_line_reports_eof() {
        smol::block_on(async {
            let mut reader = BufReader::new(Cursor::new(Vec::new()));
            let outcome = read_bounded_line(&mut reader, 16).await.unwrap();
            assert!(matches!(outcome, BoundedLine::Eof));
        });
    }

    #[cfg(unix)]
    #[test]
    #[allow(unsafe_code)]
    fn shutdown_kills_and_reaps_owned_child_while_transport_clone_exists() {
        smol::block_on(async {
            let transport = Arc::new(
                StdioTransport::spawn(
                    "test",
                    "sleep",
                    &["60".into()],
                    &HashMap::new(),
                    Duration::from_secs(1),
                    Box::new(|_| {}),
                )
                .unwrap(),
            );
            let outstanding_clone = Arc::clone(&transport);
            let pid = i32::try_from(transport.child_guard().id()).unwrap();

            // SAFETY: pid was captured before reaping; kill(pid, 0) only checks process liveness without sending a signal.
            assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
            transport.shutdown().await;
            // SAFETY: pid was captured before reaping; kill(pid, 0) only checks process liveness without sending a signal.
            assert_ne!(unsafe { libc::kill(pid, 0) }, 0);

            outstanding_clone.shutdown().await;
            // SAFETY: pid was captured before reaping; kill(pid, 0) only checks process liveness without sending a signal.
            assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
        });
    }

    #[test]
    fn stderr_dedup_logs_distinct_lines_individually() {
        let mut dedup = StderrDedup::new();
        assert!(matches!(dedup.observe("a"), StderrDedupAction::Log(l) if l == "a"));
        assert!(matches!(dedup.observe("b"), StderrDedupAction::Log(l) if l == "b"));
        assert!(dedup.flush().is_none());
    }

    #[test]
    fn stderr_dedup_suppresses_consecutive_repeats() {
        let mut dedup = StderrDedup::new();
        assert!(matches!(dedup.observe("boom"), StderrDedupAction::Log(l) if l == "boom"));
        assert!(matches!(dedup.observe("boom"), StderrDedupAction::Suppress));
        assert!(matches!(dedup.observe("boom"), StderrDedupAction::Suppress));
    }

    #[test]
    fn stderr_dedup_flushes_repeat_count_when_line_changes() {
        let mut dedup = StderrDedup::new();
        dedup.observe("boom");
        dedup.observe("boom");
        dedup.observe("boom");
        match dedup.observe("different") {
            StderrDedupAction::FlushThenLog {
                previous,
                repeats,
                current,
            } => {
                assert_eq!(previous, "boom");
                assert_eq!(repeats, 2, "two suppressed repeats after the first log");
                assert_eq!(current, "different");
            }
            _ => panic!("expected FlushThenLog, got a different action"),
        }
    }

    #[test]
    fn stderr_dedup_flush_reports_trailing_repeats_on_stream_end() {
        let mut dedup = StderrDedup::new();
        dedup.observe("boom");
        dedup.observe("boom");
        dedup.observe("boom");
        let (line, repeats) = dedup.flush().expect("trailing repeats to flush");
        assert_eq!(line, "boom");
        assert_eq!(repeats, 2);
        assert!(dedup.flush().is_none(), "flush must not double-report");
    }

    #[test]
    fn stderr_dedup_flush_is_none_without_repeats() {
        let mut dedup = StderrDedup::new();
        dedup.observe("only-once");
        assert!(dedup.flush().is_none());
    }
    #[cfg(unix)]
    #[test]
    fn explicit_shutdown_does_not_invoke_death_callback() {
        smol::block_on(async {
            let death_called = Arc::new(AtomicBool::new(false));
            let death_called_write = Arc::clone(&death_called);
            let transport = StdioTransport::spawn(
                "test",
                "sleep",
                &["60".into()],
                &HashMap::new(),
                Duration::from_secs(5),
                Box::new(move |_| {
                    death_called_write.store(true, Ordering::Release);
                }),
            )
            .unwrap();

            transport.shutdown().await;
            let StdioTransport {
                _reader_task: reader_task,
                ..
            } = transport;
            reader_task.await;

            assert!(!death_called.load(Ordering::Acquire));
        });
    }

    #[cfg(unix)]
    #[test]
    fn reader_loop_end_invokes_death_callback() {
        smol::block_on(async {
            let died: Arc<StdMutex<Option<McpError>>> = Arc::new(StdMutex::new(None));
            let died_write = Arc::clone(&died);
            let transport = StdioTransport::spawn(
                "test",
                "sh",
                // The child waits for a line rather than exiting at once, so
                // the handshake window is closed by this test and not by the
                // scheduler. An `exit 0` child can clear `alive` before
                // `mark_established` runs and fail the assertion below on a
                // loaded runner.
                &["-c".into(), "read -r _ || true".into()],
                &HashMap::new(),
                Duration::from_secs(5),
                Box::new(move |e| {
                    *died_write
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e);
                }),
            )
            .unwrap();
            assert!(
                transport.mark_established(),
                "the child must still be alive when the handshake completes"
            );
            // Release the child now that the handshake is on record.
            transport.write_line(b"\n").await.unwrap();

            let StdioTransport {
                _reader_task: reader_task,
                ..
            } = transport;
            reader_task.await;
            assert!(
                died.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_some()
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn death_before_handshake_established_does_not_invoke_callback() {
        smol::block_on(async {
            let died = Arc::new(AtomicBool::new(false));
            let died_write = Arc::clone(&died);
            // Never calls `mark_established` — matches a real `start_server` still mid
            // handshake when the child dies, which apply_start_result reports itself.
            let transport = StdioTransport::spawn(
                "test",
                "sh",
                &["-c".into(), "exit 0".into()],
                &HashMap::new(),
                Duration::from_secs(5),
                Box::new(move |_| {
                    died_write.store(true, Ordering::Release);
                }),
            )
            .unwrap();

            let StdioTransport {
                _reader_task: reader_task,
                ..
            } = transport;
            reader_task.await;

            assert!(!died.load(Ordering::Acquire));
        });
    }

    /// A child can answer the last handshake request and close stdout before
    /// `start_server` calls `mark_established`. The reader suppresses its own
    /// report in that window, so it must at least clear liveness — otherwise
    /// `mark_established` returns `true` and a dead transport is published as
    /// initialized, with its tools advertised and no reconnect.
    #[cfg(unix)]
    #[test]
    fn a_death_during_the_handshake_window_clears_liveness() {
        smol::block_on(async {
            let transport = StdioTransport::spawn(
                "test",
                "sh",
                &["-c".into(), "exit 0".into()],
                &HashMap::new(),
                Duration::from_secs(5),
                Box::new(|_| {}),
            )
            .unwrap();

            let StdioTransport {
                _reader_task: reader_task,
                alive,
                established,
                ..
            } = transport;
            reader_task.await;

            assert!(
                !established.load(Ordering::SeqCst),
                "the handshake never completed in this test"
            );
            assert!(
                !alive.load(Ordering::SeqCst),
                "the reader must clear liveness even before the handshake completes"
            );
        });
    }
}
