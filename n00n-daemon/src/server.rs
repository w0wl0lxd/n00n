//! Unix NDJSON control server.

use std::path::Path;
use std::sync::Arc;

use crate::auth;
use crate::error::{ControlError, ControlResult};
use crate::lock::{self, DaemonRole};
use crate::paths::daemon_socket_in;
use crate::protocol::{ControlRequest, ControlResponse};
use crate::registry::ControlPlane;
use crate::transport::{self, Endpoint};

async fn exchange(
    reader: impl futures_lite::AsyncRead + Unpin,
    mut writer: impl futures_lite::AsyncWrite + Unpin,
    plane: Arc<ControlPlane>,
) -> ControlResult<()> {
    use futures_lite::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};

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

/// Serve `plane` until `cancel` receives a unit value (or disconnects).
///
/// # Errors
/// Returns if the socket cannot be bound, lock cannot be acquired, or the accept loop fails.
pub async fn serve(
    state_dir: &Path,
    plane: Arc<ControlPlane>,
    cancel: flume::Receiver<()>,
    role: DaemonRole,
) -> ControlResult<()> {
    transport::ensure_can_bind(state_dir, role)?;

    #[cfg(unix)]
    {
        let endpoint = Endpoint::Uds(daemon_socket_in(state_dir));
        let lock = transport::lock_for_endpoint(role, &endpoint);
        lock::write(state_dir, &lock)?;
        let path = match &endpoint {
            Endpoint::Uds(p) => p.clone(),
            Endpoint::Tcp(_) => {
                let _ = lock::remove(state_dir);
                return Err(ControlError::Unavailable(
                    "uds endpoint expected on unix".into(),
                ));
            }
        };
        let result = serve_uds(&path, plane, cancel).await;
        let _ = lock::remove(state_dir);
        result
    }

    #[cfg(windows)]
    {
        use smol::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(ControlError::io)?;
        let addr = listener.local_addr().map_err(ControlError::io)?;
        let endpoint = Endpoint::Tcp(addr);
        let lock = transport::lock_for_endpoint(role, &endpoint);
        lock::write(state_dir, &lock)?;
        let result = serve_tcp(listener, plane, cancel).await;
        let _ = lock::remove(state_dir);
        result
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (state_dir, plane, cancel, role);
        Err(ControlError::Unavailable(
            "n00n-daemon server unsupported on this platform".into(),
        ))
    }
}

#[cfg(unix)]
async fn serve_uds(
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
                if let Err(e) = auth::check_unix_peer_uid(&stream) {
                    tracing::warn!(error = %e, "daemon connection rejected");
                    continue;
                }
                let plane = Arc::clone(&plane);
                smol::spawn(async move {
                    let (reader, writer) = futures_lite::io::split(stream);
                    if let Err(e) = exchange(reader, writer, plane).await {
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

#[cfg(windows)]
async fn serve_tcp(
    listener: smol::net::TcpListener,
    plane: Arc<ControlPlane>,
    cancel: flume::Receiver<()>,
) -> ControlResult<()> {
    use futures_lite::StreamExt;

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
                    let (reader, writer) = futures_lite::io::split(stream);
                    if let Err(e) = exchange(reader, writer, plane).await {
                        tracing::warn!(error = %e, "daemon connection failed");
                    }
                })
                .detach();
            }
        }
    }
    Ok(())
}
