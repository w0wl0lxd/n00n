//! Unix NDJSON control server.

use std::path::Path;
use std::sync::Arc;

use crate::error::{ControlError, ControlResult};
use crate::protocol::{ControlRequest, ControlResponse};
use crate::registry::ControlPlane;

/// Serve `plane` on `socket_path` until `cancel` receives a unit value (or disconnects).
///
/// # Errors
/// Returns if the socket cannot be bound or the accept loop fails fatally.
#[cfg(unix)]
pub async fn serve(
    socket_path: &Path,
    plane: Arc<ControlPlane>,
    cancel: flume::Receiver<()>,
) -> ControlResult<()> {
    use futures_lite::StreamExt;
    use smol::net::unix::UnixListener;

    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).map_err(ControlError::io)?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    if socket_path.exists() {
        std::fs::remove_file(socket_path).map_err(ControlError::io)?;
    }

    let listener = UnixListener::bind(socket_path).map_err(ControlError::io)?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600));
    }

    let mut incoming = Box::pin(listener.incoming());
    loop {
        let next = futures_lite::future::or(
            async {
                let _ = cancel.recv_async().await;
                None
            },
            async { incoming.next().await },
        )
        .await;

        match next {
            None => break,
            Some(Err(e)) => return Err(ControlError::io(e)),
            Some(Ok(stream)) => {
                let plane = Arc::clone(&plane);
                smol::spawn(async move {
                    if let Err(e) = handle_conn(stream, plane).await {
                        tracing::warn!(error = %e, "daemon connection failed");
                    }
                })
                .detach();
            }
        }
    }
    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

#[cfg(unix)]
async fn handle_conn(
    stream: smol::net::unix::UnixStream,
    plane: Arc<ControlPlane>,
) -> ControlResult<()> {
    use futures_lite::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};

    let (reader, mut writer) = futures_lite::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(ControlError::io)?;
    let req = ControlRequest::from_line(&line).map_err(ControlError::protocol)?;
    let resp = match plane.handle(req) {
        Ok(r) => r,
        Err(e) => ControlResponse::from_error(&e),
    };
    let out = resp.to_line().map_err(ControlError::protocol)?;
    writer
        .write_all(out.as_bytes())
        .await
        .map_err(ControlError::io)?;
    writer.write_all(b"\n").await.map_err(ControlError::io)?;
    writer.flush().await.map_err(ControlError::io)?;
    Ok(())
}

#[cfg(not(unix))]
pub async fn serve(
    _socket_path: &Path,
    _plane: Arc<ControlPlane>,
    _cancel: flume::Receiver<()>,
) -> ControlResult<()> {
    Err(ControlError::Unavailable(
        "n00n-daemon UDS server requires unix".into(),
    ))
}
