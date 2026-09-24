use std::path::{Path, PathBuf};

use n00n_storage::StateDir;

use crate::error::{ControlError, ControlResult};

pub const DAEMON_SOCK_NAME: &str = "daemon.sock";

/// Resolve `state_dir/daemon.sock`.
///
/// # Errors
/// Returns an error if the state directory cannot be resolved.
pub fn daemon_socket_path() -> ControlResult<PathBuf> {
    let dir = StateDir::resolve().map_err(ControlError::io)?;
    Ok(dir.path().join(DAEMON_SOCK_NAME))
}

/// Socket path under an explicit state directory root (tests / overrides).
#[must_use]
pub fn daemon_socket_in(state_dir: &Path) -> PathBuf {
    state_dir.join(DAEMON_SOCK_NAME)
}
