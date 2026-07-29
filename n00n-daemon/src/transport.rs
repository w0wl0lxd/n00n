//! Client/server transport selection (UDS on Unix, loopback TCP on Windows).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::error::{ControlError, ControlResult};
use crate::lock::{self, DaemonLock, DaemonRole, TransportKind};

#[cfg(unix)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{DaemonLock, DaemonRole, TransportKind};
    use tempfile::TempDir;

    #[test]
    fn endpoint_from_uds_lock() {
        let lock = DaemonLock {
            pid: 1,
            role: DaemonRole::Tui,
            started_at: 0,
            transport: TransportKind::Uds,
            endpoint: "/tmp/daemon.sock".into(),
        };
        let ep = Endpoint::from_lock(&lock).expect("uds endpoint");
        assert_eq!(ep, Endpoint::Uds(PathBuf::from("/tmp/daemon.sock")));
    }

    #[test]
    fn endpoint_from_tcp_lock() {
        let lock = DaemonLock {
            pid: 1,
            role: DaemonRole::Worker,
            started_at: 0,
            transport: TransportKind::Tcp,
            endpoint: "127.0.0.1:1234".into(),
        };
        let ep = Endpoint::from_lock(&lock).expect("tcp endpoint");
        assert_eq!(ep, Endpoint::Tcp("127.0.0.1:1234".parse().unwrap()));
    }

    #[test]
    fn endpoint_from_malformed_tcp_lock_is_none() {
        let lock = DaemonLock {
            pid: 1,
            role: DaemonRole::Headless,
            started_at: 0,
            transport: TransportKind::Tcp,
            endpoint: "not-an-address".into(),
        };
        assert!(Endpoint::from_lock(&lock).is_none());
    }

    #[test]
    fn lock_for_endpoint_roundtrips() {
        let uds = Endpoint::Uds(PathBuf::from("/tmp/n00n.sock"));
        let lock = lock_for_endpoint(DaemonRole::Tui, &uds);
        assert_eq!(lock.role, DaemonRole::Tui);
        assert_eq!(lock.transport, TransportKind::Uds);
        assert_eq!(lock.endpoint, "/tmp/n00n.sock");
        assert_eq!(Endpoint::from_lock(&lock), Some(uds));

        let tcp = Endpoint::Tcp("127.0.0.1:5678".parse().unwrap());
        let lock = lock_for_endpoint(DaemonRole::Worker, &tcp);
        assert_eq!(lock.role, DaemonRole::Worker);
        assert_eq!(lock.transport, TransportKind::Tcp);
        assert_eq!(lock.endpoint, "127.0.0.1:5678");
        assert_eq!(Endpoint::from_lock(&lock), Some(tcp));
    }

    #[cfg(unix)]
    #[test]
    fn client_default_uses_state_dir_daemon_sock() -> Result<(), String> {
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let ep = client_default(tmp.path()).map_err(|e| e.to_string())?;
        assert_eq!(ep, Endpoint::Uds(tmp.path().join("daemon.sock")));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn resolve_client_falls_back_to_default_when_lock_missing() -> Result<(), String> {
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let ep = resolve_client(tmp.path()).map_err(|e| e.to_string())?;
        assert_eq!(ep, Endpoint::Uds(tmp.path().join("daemon.sock")));
        Ok(())
    }

    #[cfg(not(unix))]
    #[test]
    fn resolve_client_returns_error_when_no_lock_and_no_default() -> Result<(), String> {
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        assert!(resolve_client(tmp.path()).is_err());
        Ok(())
    }

    #[test]
    fn ensure_can_bind_when_lock_missing() -> Result<(), String> {
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        ensure_can_bind(tmp.path(), DaemonRole::Tui).map_err(|e| e.to_string())?;
        ensure_can_bind(tmp.path(), DaemonRole::Worker).map_err(|e| e.to_string())?;
        Ok(())
    }
}
