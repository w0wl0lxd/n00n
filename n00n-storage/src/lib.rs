//! Persistent storage. `atomic_write` writes to a `tempfile` in the same
//! directory then persists (atomic rename) for crash safety.
//! `atomic_write_permissions` sets file mode before persist (for auth keys at 0600).

pub mod auth;
pub mod id;
pub mod input_history;
pub mod log;
pub mod model;
pub mod paths;
pub mod plans;
pub mod sessions;
pub mod theme;
pub mod toon;
pub mod version;

use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;

use paths::state_dir;

#[derive(Debug, Clone)]
pub struct StateDir(PathBuf);

impl StateDir {
    /// # Errors
    /// Returns an error if the state directory cannot be determined or created.
    pub fn resolve() -> Result<Self, StorageError> {
        let dir = state_dir()?;
        Ok(Self(dir))
    }

    #[must_use]
    pub fn from_path(path: PathBuf) -> Self {
        Self(path)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// # Errors
    /// Returns an error if the subdirectory cannot be created.
    pub fn ensure_subdir(&self, name: &str) -> Result<PathBuf, StorageError> {
        let dir = self.0.join(name);
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("home directory not found")]
    HomeNotSet,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("slug collision after max attempts")]
    SlugCollision,
    #[error("toon error: {0}")]
    Toon(String),
    #[error("random generation failed: {0}")]
    GetRandom(String),
}

/// # Errors
/// Returns an error if the parent directory does not exist, the temporary
/// file cannot be created or written, or the atomic rename fails.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), StorageError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    tmp.as_file().sync_data()?;
    // `into_parts` drops the auto-cleanup-on-drop guarantee, but we need the
    // File handle closed (Windows can't rename an open file) and `persist()`
    // doesn't support the fibonacci backoff retry that Windows virus scanners
    // require. On failure below, we manually clean up the temp file.
    let (_, tmp_path) = tmp.into_parts();
    retry_rename(&tmp_path, path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        StorageError::Io(e)
    })?;
    if let Err(e) = sync_dir(parent) {
        tracing::warn!(error = %e, "failed to sync parent directory");
    }
    Ok(())
}

pub(crate) fn atomic_write_permissions(
    path: &Path,
    data: &[u8],
    mode: u32,
) -> Result<(), StorageError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    #[cfg(unix)]
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(mode))?;
    #[cfg(not(unix))]
    let _ = mode;
    tmp.as_file().sync_all()?;
    // See `atomic_write` for the `into_parts` tradeoff.
    let (_, tmp_path) = tmp.into_parts();
    retry_rename(&tmp_path, path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        StorageError::Io(e)
    })?;
    if let Err(e) = sync_dir(parent) {
        tracing::warn!(error = %e, "failed to sync parent directory");
    }
    Ok(())
}

/// Rename with fibonacci backoff to handle transient `PermissionDenied` from
/// virus scanners on Windows. 20 steps from 1ms sums to ~18 seconds.
/// Matches the pattern used by juliaup and rustup.
///
/// On non-Windows platforms, `PermissionDenied` from rename is a real
/// permissions problem (different user, immutable flag, etc.) that
/// retrying will not fix, so we just call rename once.
#[cfg(windows)]
fn retry_rename(src: &Path, dest: &Path) -> std::io::Result<()> {
    let mut a: u64 = 0;
    let mut b: u64 = 1;
    for _ in 0..20 {
        match fs::rename(src, dest) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                thread::sleep(Duration::from_millis(b));
                let next = a.saturating_add(b);
                a = b;
                b = next;
            }
            Err(e) => return Err(e),
        }
    }
    fs::rename(src, dest)
}

#[cfg(not(windows))]
fn retry_rename(src: &Path, dest: &Path) -> std::io::Result<()> {
    fs::rename(src, dest)
}

/// Flush a directory's metadata so a file created/renamed inside it is
/// guaranteed to be reachable after a crash. No-op on platforms where this is
/// not meaningful or not supported.
#[allow(clippy::unnecessary_wraps)]
fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()?;
    }
    Ok(())
}

/// Returns the current time in seconds since the UNIX epoch.
///
/// # Panics
/// Panics if the system time is before the UNIX epoch. This condition indicates
/// a system clock misconfiguration that should not occur in normal operation.
#[must_use]
pub fn now_epoch() -> u64 {
    #[allow(clippy::expect_used)]
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch is not supported")
        .as_secs()
}
