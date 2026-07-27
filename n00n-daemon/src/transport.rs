//! Client/server transport selection (UDS on Unix, loopback TCP on Windows).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::error::{ControlError, ControlResult};
use crate::lock::{self, DaemonLock, DaemonRole, TransportKind};
use crate::paths::daemon_socket_in;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Uds(PathBuf),
    Tcp(SocketAddr),
}

impl Endpoint {
    #[must_use]
    pub fn from_lock(lock: &DaemonLock) -> Option<Self> {
        match lock.transport {
            TransportKind::Uds => Some(Self::Uds(PathBuf::from(&lock.endpoint))),
            TransportKind::Tcp => lock.endpoint.parse().ok().map(Self::Tcp),
        }
    }
}

/// Resolve the endpoint clients should dial: prefer live lock, else default UDS path.
///
/// # Errors
/// Returns when the lock file is corrupt.
pub fn resolve_client(state_dir: &Path) -> ControlResult<Endpoint> {
    lock::sweep_stale(state_dir)?;
    if let Some(lock) = lock::read(state_dir)?
        && lock::pid_alive(lock.pid)
        && let Some(ep) = Endpoint::from_lock(&lock)
    {
        return Ok(ep);
    }
    client_default(state_dir)
}

/// Default client endpoint when no live lock is present.
///
/// # Errors
/// On non-Unix platforms without a live lock, returns unavailable.
pub fn client_default(state_dir: &Path) -> ControlResult<Endpoint> {
    #[cfg(unix)]
    {
        Ok(Endpoint::Uds(daemon_socket_in(state_dir)))
    }
    #[cfg(not(unix))]
    {
        let _ = state_dir;
        Err(ControlError::Unavailable(
            "no daemon listener advertised (start n00n or n00n agent daemon)".into(),
        ))
    }
}

/// Refuse starting when an incompatible live listener owns the lock.
///
/// # Errors
/// Returns when a live owner blocks this role.
pub fn ensure_can_bind(state_dir: &Path, role: DaemonRole) -> ControlResult<()> {
    lock::sweep_stale(state_dir)?;
    let Some(existing) = lock::read(state_dir)? else {
        return Ok(());
    };
    if lock::blocks_new_listener(&existing, role) {
        return Err(ControlError::Unavailable(format!(
            "daemon already running (pid {}, role {:?}); use n00n agent list",
            existing.pid, existing.role
        )));
    }
    Ok(())
}

/// Build lock metadata for the listener we are about to bind.
#[must_use]
pub fn lock_for_endpoint(role: DaemonRole, endpoint: &Endpoint) -> DaemonLock {
    let (transport, endpoint_str) = match endpoint {
        Endpoint::Uds(path) => (TransportKind::Uds, path.display().to_string()),
        Endpoint::Tcp(addr) => (TransportKind::Tcp, addr.to_string()),
    };
    DaemonLock {
        pid: std::process::id(),
        role,
        started_at: n00n_storage::now_epoch(),
        transport,
        endpoint: endpoint_str,
    }
}
