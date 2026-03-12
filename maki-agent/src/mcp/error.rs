use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("server {server} failed to start: {reason}")]
    StartFailed { server: String, reason: String },

    #[error("server {server} is not running")]
    ServerDied { server: String },

    #[error("server {server} timed out after {timeout_ms}ms")]
    Timeout { server: String, timeout_ms: u64 },

    #[error("server {server} returned error {code}: {message}")]
    RpcError {
        server: String,
        code: i64,
        message: String,
    },

    #[error("invalid response from server {server}: {reason}")]
    InvalidResponse { server: String, reason: String },

    #[error("unknown MCP tool: {name}")]
    UnknownTool { name: String },

    #[error("config error: {0}")]
    Config(String),

    #[error("write to server {server} failed: {reason}")]
    WriteFailed { server: String, reason: String },
}
