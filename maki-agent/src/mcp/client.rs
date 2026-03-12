use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_lock::Mutex;
use async_process::{Child, Command, Stdio};
use futures_lite::AsyncBufReadExt;
use futures_lite::io::BufReader;
use serde_json::Value;
use tracing::{debug, warn};

use futures_lite::AsyncWriteExt;

use super::config::McpServerConfig;
use super::error::McpError;
use super::protocol::{
    CallToolResult, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, ToolInfo,
    ToolsListResult, initialize_params,
};

type PendingMap = HashMap<u64, smol::channel::Sender<Result<Value, McpError>>>;

pub struct McpClient {
    name: Arc<str>,
    stdin: Mutex<async_process::ChildStdin>,
    pending: Arc<Mutex<PendingMap>>,
    next_id: AtomicU64,
    timeout: Duration,
    alive: Arc<AtomicBool>,
    _reader_task: smol::Task<()>,
    _stderr_task: smol::Task<()>,
    _child: Child,
}

impl McpClient {
    pub fn spawn(name: &str, config: &McpServerConfig) -> Result<Self, McpError> {
        let (program, args) =
            config
                .command
                .split_first()
                .ok_or_else(|| McpError::StartFailed {
                    server: name.into(),
                    reason: "empty command".into(),
                })?;

        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .envs(&config.environment);

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
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));

        let reader_task = {
            let name = Arc::clone(&name);
            let alive = Arc::clone(&alive);
            let pending = Arc::clone(&pending);
            smol::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<JsonRpcResponse>(trimmed) {
                        Ok(resp) => {
                            if let Some(id) = resp.id
                                && let Some(sender) = pending.lock().await.remove(&id)
                            {
                                let result = if let Some(err) = resp.error {
                                    Err(McpError::RpcError {
                                        server: (*name).into(),
                                        code: err.code,
                                        message: err.message,
                                    })
                                } else {
                                    Ok(resp.result.unwrap_or(Value::Null))
                                };
                                let _ = sender.send(result).await;
                            }
                        }
                        Err(e) => {
                            debug!(server = &*name, error = %e, line = trimmed, "non-JSON-RPC line from server");
                        }
                    }
                }
                alive.store(false, Ordering::Release);
                let mut pending = pending.lock().await;
                for (_, sender) in pending.drain() {
                    let _ = sender
                        .send(Err(McpError::ServerDied {
                            server: (*name).into(),
                        }))
                        .await;
                }
            })
        };

        let stderr_task = {
            let name = Arc::clone(&name);
            smol::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                warn!(server = &*name, "{trimmed}");
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
            timeout: Duration::from_millis(config.timeout),
            alive,
            _reader_task: reader_task,
            _stderr_task: stderr_task,
            _child: child,
        })
    }

    async fn write_line(&self, line: &[u8]) -> Result<(), McpError> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line)
            .await
            .map_err(|e| McpError::WriteFailed {
                server: (*self.name).into(),
                reason: e.to_string(),
            })?;
        stdin.flush().await.map_err(|e| McpError::WriteFailed {
            server: (*self.name).into(),
            reason: e.to_string(),
        })
    }

    fn server_died(&self) -> McpError {
        McpError::ServerDied {
            server: (*self.name).into(),
        }
    }

    fn serialize(&self, value: &impl serde::Serialize) -> Result<Vec<u8>, McpError> {
        let mut line = serde_json::to_string(value).map_err(|e| McpError::InvalidResponse {
            server: (*self.name).into(),
            reason: e.to_string(),
        })?;
        line.push('\n');
        Ok(line.into_bytes())
    }

    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(self.server_died());
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(id, method, params);

        let (tx, rx) = smol::channel::bounded(1);
        self.pending.lock().await.insert(id, tx);

        self.write_line(&self.serialize(&req)?).await?;

        let result = futures_lite::future::race(
            async { rx.recv().await.unwrap_or(Err(self.server_died())) },
            async {
                async_io::Timer::after(self.timeout).await;
                Err(McpError::Timeout {
                    server: (*self.name).into(),
                    timeout_ms: self.timeout.as_millis() as u64,
                })
            },
        )
        .await;

        if result.is_err() {
            self.pending.lock().await.remove(&id);
        }

        result
    }

    pub async fn initialize(&self) -> Result<(), McpError> {
        let params = initialize_params();
        self.send_request("initialize", Some(params)).await?;
        self.send_notification("notifications/initialized", None)
            .await?;
        Ok(())
    }

    async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        let notif = JsonRpcNotification::new(method, params);
        self.write_line(&self.serialize(&notif)?).await
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, McpError> {
        let result = self.send_request("tools/list", None).await?;
        let list: ToolsListResult =
            serde_json::from_value(result).map_err(|e| McpError::InvalidResponse {
                server: (*self.name).into(),
                reason: e.to_string(),
            })?;
        Ok(list.tools)
    }

    pub async fn call_tool(&self, tool_name: &str, args: &Value) -> Result<String, McpError> {
        let params = serde_json::json!({
            "name": tool_name,
            "arguments": args,
        });
        let result = self.send_request("tools/call", Some(params)).await?;
        let call_result: CallToolResult =
            serde_json::from_value(result).map_err(|e| McpError::InvalidResponse {
                server: (*self.name).into(),
                reason: e.to_string(),
            })?;

        let text = call_result.joined_text();

        if call_result.is_error {
            return Err(McpError::RpcError {
                server: (*self.name).into(),
                code: -1,
                message: text,
            });
        }

        Ok(text)
    }

    pub async fn shutdown(mut self) {
        self.alive.store(false, Ordering::Release);
        let _ = self._child.kill();
    }
}
