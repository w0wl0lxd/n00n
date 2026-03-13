pub mod auth;
pub mod input_history;
pub mod log;
pub mod plans;
pub mod sessions;
pub mod theme;

use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DATA_DIR_NAME: &str = ".maki";

#[derive(Debug, Clone)]
pub struct DataDir(PathBuf);

impl DataDir {
    pub fn resolve() -> Result<Self, StorageError> {
        let home = env::var("HOME").map_err(|_| StorageError::HomeNotSet)?;
        let dir = PathBuf::from(home).join(DATA_DIR_NAME);
        fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    pub fn from_path(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn ensure_subdir(&self, name: &str) -> Result<PathBuf, StorageError> {
        let dir = self.0.join(name);
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("HOME environment variable not set")]
    HomeNotSet,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
}

pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> Result<(), StorageError> {
    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub(crate) fn atomic_write_permissions(
    path: &Path,
    data: &[u8],
    mode: u32,
) -> Result<(), StorageError> {
    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(data)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub(crate) fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
