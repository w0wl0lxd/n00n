//! Unix NDJSON control server.

use std::convert::Infallible;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_lite::{AsyncRead, AsyncWrite, Stream, StreamExt, future, stream};
use smol::Task;

#[cfg(unix)]
use crate::auth;
use crate::error::{ControlError, ControlResult};
use crate::lock::{self, DaemonRole};
#[cfg(unix)]
use crate::paths::daemon_socket_in;
use crate::protocol::{ControlRequest, ControlResponse};
use crate::registry::ControlPlane;
use crate::transport::{self, Endpoint};

const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Reads one request, then handles it and writes the reply. Shutdown cancels
/// only the read: once `plane.handle` runs, the reply is always attempted.
async fn exchange(
    reader: impl AsyncRead + Unpin,
    mut writer: impl AsyncWrite + Unpin,
    plane: Arc<ControlPlane>,
    shutdown: &flume::Receiver<Infallible>,
) -> ControlResult<()> {
    use futures_lite::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let request_read = future::or(
        async {
            shutdown_requested(shutdown).await;
            Ok(false)
        },
        async { reader.read_line(&mut line).await.map(|_| true) },
    )
    .await
    .map_err(ControlError::io)?;
    if !request_read {
        tracing::debug!("daemon connection closed at shutdown before a request arrived");
        return Ok(());
    }
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

/// Resolves once the accept loop drops the shutdown sender.
async fn shutdown_requested(shutdown: &flume::Receiver<Infallible>) {
    match shutdown.recv_async().await {
        Ok(never) => match never {},
        Err(flume::RecvError::Disconnected) => {}
    }
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

    let incoming = stream::unfold(listener, |listener| async move {
        let accepted = listener.accept().await.map(|(stream, _)| stream);
        Some((accepted, listener))
    });
    let result = accept_loop(Box::pin(incoming), plane, cancel, auth::check_unix_peer_uid).await;
    let _ = std::fs::remove_file(socket_path);
    result
}

#[cfg(windows)]
async fn serve_tcp(
    listener: smol::net::TcpListener,
    plane: Arc<ControlPlane>,
    cancel: flume::Receiver<()>,
) -> ControlResult<()> {
    let incoming = stream::unfold(listener, |listener| async move {
        let accepted = listener.accept().await.map(|(stream, _)| stream);
        Some((accepted, listener))
    });
    accept_loop(Box::pin(incoming), plane, cancel, |_| Ok(())).await
}

#[cfg(any(unix, windows))]
async fn accept_loop<S>(
    mut incoming: impl Stream<Item = io::Result<S>> + Unpin,
    plane: Arc<ControlPlane>,
    cancel: flume::Receiver<()>,
    admit: impl Fn(&S) -> ControlResult<()>,
) -> ControlResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Retain per-connection handles so shutdown can wait for exchanges that
    // already handled a request; otherwise those tasks outlive `serve` holding `plane`.
    let mut connections: Vec<Task<()>> = Vec::new();
    let (shutdown_tx, shutdown_rx) = flume::bounded::<Infallible>(0);
    let accepted = loop {
        connections.retain(|task| !task.is_finished());
        let next = future::or(
            async {
                let _ = cancel.recv_async().await;
                None
            },
            async { incoming.next().await },
        )
        .await;

        match next {
            None => break Ok(()),
            Some(Err(e)) => break Err(ControlError::io(e)),
            Some(Ok(stream)) => {
                if let Err(e) = admit(&stream) {
                    tracing::warn!(error = %e, "daemon connection rejected");
                    continue;
                }
                let plane = Arc::clone(&plane);
                let shutdown = shutdown_rx.clone();
                connections.push(smol::spawn(async move {
                    let (reader, writer) = futures_lite::io::split(stream);
                    if let Err(e) = exchange(reader, writer, plane, &shutdown).await {
                        tracing::warn!(error = %e, "daemon connection failed");
                    }
                }));
            }
        }
    };
    // Close the listener first so no new client queues up during the drain.
    drop(incoming);
    drop(shutdown_tx);
    drain_connections(connections).await;
    accepted
}

/// Waits for open exchanges after shutdown. Idle readers return at once;
/// exchanges past `plane.handle` finish their reply. Tasks still running after
/// [`SHUTDOWN_DRAIN_TIMEOUT`] are cancelled when the vector drops.
#[cfg(any(unix, windows))]
async fn drain_connections(connections: Vec<Task<()>>) {
    let open = connections.len();
    let drained = future::or(
        async {
            for task in connections {
                task.await;
            }
            true
        },
        async {
            smol::Timer::after(SHUTDOWN_DRAIN_TIMEOUT).await;
            false
        },
    )
    .await;
    if !drained {
        tracing::warn!(
            open,
            timeout_ms = SHUTDOWN_DRAIN_TIMEOUT.as_millis(),
            "daemon shutdown cancelled exchanges that did not finish in time"
        );
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use flume::r#async::RecvFut;
    use futures_lite::io::Cursor;

    use super::*;

    const HEALTH_LINE: &[u8] = b"{\"op\":\"health\"}\n";

    /// Connection whose response write parks until `release` fires, so the
    /// test can hold an exchange between `plane.handle` and the reply.
    struct ParkedConn {
        request: Cursor<Vec<u8>>,
        write_started: Option<flume::Sender<()>>,
        release: Option<Pin<Box<RecvFut<'static, ()>>>>,
        response: flume::Sender<Vec<u8>>,
    }

    impl AsyncRead for ParkedConn {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.request).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for ParkedConn {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            if let Some(started) = self.write_started.take()
                && let Err(e) = started.send(())
            {
                return Poll::Ready(Err(io::Error::other(e)));
            }
            if let Some(release) = self.release.as_mut() {
                if release.as_mut().poll(cx).is_pending() {
                    return Poll::Pending;
                }
                self.release = None;
            }
            Poll::Ready(
                self.response
                    .send(buf.to_vec())
                    .map(|()| buf.len())
                    .map_err(io::Error::other),
            )
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn shutdown_delivers_response_for_request_already_handled() {
        let (write_started_tx, write_started_rx) = flume::bounded(1);
        let (release_tx, release_rx) = flume::bounded::<()>(1);
        let (response_tx, response_rx) = flume::unbounded();
        let conn = ParkedConn {
            request: Cursor::new(HEALTH_LINE.to_vec()),
            write_started: Some(write_started_tx),
            release: Some(Box::pin(release_rx.into_recv_async())),
            response: response_tx,
        };
        let (conn_tx, conn_rx) = flume::unbounded();
        conn_tx.send(conn).unwrap();
        // Dropping the stream drops `listener_open_tx`, which tells the test that
        // the accept loop released its listener.
        let (listener_open_tx, listener_open_rx) = flume::bounded::<()>(0);
        let incoming = stream::unfold((conn_rx, listener_open_tx), |state| async move {
            let conn = state.0.recv_async().await.unwrap();
            Some((Ok(conn), state))
        });
        let (cancel_tx, cancel_rx) = flume::bounded(1);
        let plane = Arc::new(ControlPlane::new(None, None));

        smol::block_on(async {
            let server = smol::spawn(accept_loop(
                Box::pin(incoming),
                plane,
                cancel_rx,
                |_| Ok(()),
            ));
            write_started_rx.recv_async().await.unwrap();
            cancel_tx.send(()).unwrap();
            assert!(listener_open_rx.recv_async().await.is_err());
            drop(release_tx);
            server.await.unwrap();
        });

        let response: Vec<u8> = response_rx.drain().flatten().collect();
        let line = String::from_utf8(response).unwrap();
        assert_eq!(
            ControlResponse::from_line(&line).unwrap(),
            ControlResponse::health_ok()
        );
    }
}
