//! Sidecar lock advertising who owns the control plane (`daemon.lock`).

use std::path::{Path, PathBuf};

use rustix::process::{Pid, test_kill_process};
use serde::{Deserialize, Serialize};

use crate::error::{ControlError, ControlResult};

pub const LOCK_FILE_NAME: &str = "daemon.lock";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonRole {
    Tui,
    Worker,
    Headless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Uds,
    Tcp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonLock {
    pub pid: u32,
    pub role: DaemonRole,
    pub started_at: u64,
    pub transport: TransportKind,
    pub endpoint: String,
}

#[must_use]
pub fn lock_path_in(state_dir: &Path) -> PathBuf {
    state_dir.join(LOCK_FILE_NAME)
}

/// Write or replace the lock file for this listener generation.
///
/// # Errors
/// Returns on encode or io failure.
pub fn write(state_dir: &Path, lock: &DaemonLock) -> ControlResult<()> {
    let path = lock_path_in(state_dir);
    let line = sonic_rs::to_string(lock).map_err(ControlError::protocol)?;
    n00n_storage::atomic_write(&path, line.as_bytes()).map_err(ControlError::io)
}

/// Read the lock file when present.
///
/// # Errors
/// Returns on decode or io failure other than missing file.
pub fn read(state_dir: &Path) -> ControlResult<Option<DaemonLock>> {
    let path = lock_path_in(state_dir);
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ControlError::io(e)),
    };
    sonic_rs::from_str(data.trim())
        .map_err(ControlError::protocol)
        .map(Some)
}

/// Remove the lock file; missing file is ok.
///
/// # Errors
/// Returns on io failure other than missing file.
pub fn remove(state_dir: &Path) -> ControlResult<()> {
    let path = lock_path_in(state_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ControlError::io(e)),
    }
}

/// Returns whether `pid` still refers to a live process on this host.
#[must_use]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    let Some(pid) = Pid::from_raw(raw) else {
        return false;
    };
    match test_kill_process(pid) {
        Ok(()) | Err(rustix::io::Errno::PERM) => true,
        Err(rustix::io::Errno::SRCH | _) => false,
    }
}

/// Remove a lock file whose owner pid is no longer alive.
///
/// # Errors
/// Returns on io/decode failure other than a missing lock file.
pub fn sweep_stale(state_dir: &Path) -> ControlResult<()> {
    let Some(lock) = read(state_dir)? else {
        return Ok(());
    };
    if pid_alive(lock.pid) {
        return Ok(());
    }
    remove(state_dir)
}
///
/// TUI may always replace; other roles must not start while any live owner exists.
#[must_use]
pub fn blocks_new_listener(existing: &DaemonLock, incoming: DaemonRole) -> bool {
    if !pid_alive(existing.pid) {
        return false;
    }
    if incoming == DaemonRole::Tui {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_lock(role: DaemonRole) -> DaemonLock {
        DaemonLock {
            pid: std::process::id(),
            role,
            started_at: 1,
            transport: TransportKind::Uds,
            endpoint: "/tmp/daemon.sock".into(),
        }
    }

    #[test]
    fn write_read_roundtrip() -> Result<(), String> {
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let lock = sample_lock(DaemonRole::Tui);
        write(tmp.path(), &lock).map_err(|e| e.to_string())?;
        let decoded = read(tmp.path()).map_err(|e| e.to_string())?;
        assert_eq!(decoded.as_ref(), Some(&lock));
        remove(tmp.path()).map_err(|e| e.to_string())?;
        assert_eq!(read(tmp.path()).map_err(|e| e.to_string())?, None);
        Ok(())
    }

    #[test]
    fn sweep_stale_removes_dead_pid_lock() -> Result<(), String> {
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let mut lock = sample_lock(DaemonRole::Tui);
        lock.pid = 9_999_999;
        write(tmp.path(), &lock).map_err(|e| e.to_string())?;
        sweep_stale(tmp.path()).map_err(|e| e.to_string())?;
        assert_eq!(read(tmp.path()).map_err(|e| e.to_string())?, None);
        Ok(())
    }

    #[test]
    fn tui_blocks_worker_while_alive() {
        let lock = sample_lock(DaemonRole::Tui);
        assert!(blocks_new_listener(&lock, DaemonRole::Worker));
        assert!(blocks_new_listener(&lock, DaemonRole::Headless));
    }

    #[test]
    fn worker_does_not_block_tui() {
        let lock = sample_lock(DaemonRole::Worker);
        assert!(!blocks_new_listener(&lock, DaemonRole::Tui));
    }
}
