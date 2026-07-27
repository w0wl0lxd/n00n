//! NDJSON client for the control plane socket.

use std::path::Path;

use crate::error::{ControlError, ControlResult};
use crate::protocol::{ControlRequest, ControlResponse};

/// Send one request and read one response.
///
/// # Errors
/// Returns connection, encode/decode, or remote error failures.
#[cfg(unix)]
pub async fn call(socket_path: &Path, req: &ControlRequest) -> ControlResult<ControlResponse> {
    use futures_lite::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};
    use smol::net::unix::UnixStream;

    let stream = UnixStream::connect(socket_path)
        .await
        .map_err(ControlError::io)?;
    let (reader, mut writer) = futures_lite::io::split(stream);
    let line = req.to_line().map_err(ControlError::protocol)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(ControlError::io)?;
    writer.write_all(b"\n").await.map_err(ControlError::io)?;
    writer.flush().await.map_err(ControlError::io)?;

    let mut reader = BufReader::new(reader);
    let mut resp_line = String::new();
    reader
        .read_line(&mut resp_line)
        .await
        .map_err(ControlError::io)?;
    ControlResponse::from_line(&resp_line).map_err(ControlError::protocol)
}

#[cfg(not(unix))]
pub async fn call(_socket_path: &Path, _req: &ControlRequest) -> ControlResult<ControlResponse> {
    Err(ControlError::Unavailable(
        "n00n-daemon UDS client requires unix".into(),
    ))
}

/// Blocking helper for CLI / sync callers.
///
/// # Errors
/// Same as [`call`].
pub fn call_blocking(socket_path: &Path, req: &ControlRequest) -> ControlResult<ControlResponse> {
    smol::block_on(call(socket_path, req))
}
