//! Peer credential checks for Unix domain sockets.

#[cfg(all(unix, target_os = "linux"))]
use crate::error::ControlError;
#[cfg(unix)]
use crate::error::ControlResult;

/// Reject UDS connections whose peer uid differs from ours (Linux only).
///
/// # Errors
/// Returns [`ControlError::Forbidden`] when the peer uid does not match.
#[cfg(all(unix, target_os = "linux"))]
pub fn check_unix_peer_uid(stream: &smol::net::unix::UnixStream) -> ControlResult<()> {
    use std::os::unix::io::AsFd;

    let cred =
        rustix::net::sockopt::get_socket_peercred(stream.as_fd()).map_err(ControlError::io)?;
    let peer = cred.uid.as_raw();
    let self_uid = rustix::process::getuid().as_raw();
    if peer != self_uid {
        return Err(ControlError::Forbidden(format!(
            "peer uid {peer} != self uid {self_uid}"
        )));
    }
    Ok(())
}

/// Stub for non-Linux Unix: no uid check available.
///
/// # Errors
/// Never returns an error on non-Linux Unix platforms.
#[cfg(all(unix, not(target_os = "linux")))]
pub fn check_unix_peer_uid(_stream: &smol::net::unix::UnixStream) -> ControlResult<()> {
    Ok(())
}
