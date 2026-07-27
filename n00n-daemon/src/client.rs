//! NDJSON client for the control plane.

use std::path::Path;

use crate::error::{ControlError, ControlResult};
use crate::protocol::{ControlRequest, ControlResponse};
use crate::transport::{self, Endpoint};

async fn exchange(
    reader: impl futures_lite::AsyncRead + Unpin,
    mut writer: impl futures_lite::AsyncWrite + Unpin,
    req: &ControlRequest,
) -> ControlResult<ControlResponse> {
    use futures_lite::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};

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

/// Send one request and read one response via the resolved endpoint.
///
/// # Errors
/// Returns connection, encode/decode, or remote error failures.
pub async fn call(state_dir: &Path, req: &ControlRequest) -> ControlResult<ControlResponse> {
    let endpoint = transport::resolve_client(state_dir)?;
    call_endpoint(&endpoint, req).await
}

async fn call_endpoint(
    endpoint: &Endpoint,
    req: &ControlRequest,
) -> ControlResult<ControlResponse> {
    match endpoint {
        #[cfg(unix)]
        Endpoint::Uds(path) => {
            use smol::net::unix::UnixStream;
            let stream = UnixStream::connect(path).await.map_err(ControlError::io)?;
            let (reader, writer) = futures_lite::io::split(stream);
            exchange(reader, writer, req).await
        }
        #[cfg(windows)]
        Endpoint::Tcp(addr) => {
            use smol::net::TcpStream;
            let stream = TcpStream::connect(*addr).await.map_err(ControlError::io)?;
            let (reader, writer) = futures_lite::io::split(stream);
            exchange(reader, writer, req).await
        }
        #[cfg(all(unix, not(windows)))]
        Endpoint::Tcp(_) => Err(ControlError::Unavailable(
            "tcp daemon transport is windows-only".into(),
        )),
        #[cfg(all(windows, not(unix)))]
        Endpoint::Uds(_) => Err(ControlError::Unavailable(
            "uds daemon transport is unix-only".into(),
        )),
    }
}

/// Blocking helper for CLI / sync callers.
///
/// # Errors
/// Same as [`call`].
pub fn call_blocking(state_dir: &Path, req: &ControlRequest) -> ControlResult<ControlResponse> {
    smol::block_on(call(state_dir, req))
}

/// Legacy path-based helper: infer state dir from `.../daemon.sock` parent.
///
/// # Errors
/// Same as [`call`].
pub fn call_blocking_at_socket(
    socket_path: &Path,
    req: &ControlRequest,
) -> ControlResult<ControlResponse> {
    let state_dir = socket_path
        .parent()
        .ok_or_else(|| ControlError::Unavailable("invalid daemon socket path".into()))?;
    call_blocking(state_dir, req)
}
