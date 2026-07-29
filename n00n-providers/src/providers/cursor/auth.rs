use std::path::{Path, PathBuf};

use rusqlite::OpenFlags;
use thiserror::Error;

use crate::AgentError;

const ACCESS_TOKEN_KEY: &str = "cursorAuth/accessToken";
const VSCDB_SUFFIX: &str = "Cursor/User/globalStorage/state.vscdb";

#[derive(Debug, Error)]
pub(crate) enum AuthError {
    #[error("cursor IDE token db not found at {path}")]
    DbNotFound { path: PathBuf },
    #[error("cursor access token missing from IDE storage")]
    TokenMissing,
    #[error("failed to read cursor IDE token db: {message}")]
    DbRead { message: String },
}

pub(crate) fn ide_vscdb_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library/Application Support")
                .join(VSCDB_SUFFIX)
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|config| config.join(VSCDB_SUFFIX))
    }
}

pub(crate) fn read_ide_access_token_from(path: &Path) -> Result<String, AuthError> {
    if !path.is_file() {
        return Err(AuthError::DbNotFound {
            path: path.to_path_buf(),
        });
    }
    let conn = rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| AuthError::DbRead {
            message: error.to_string(),
        })?;
    let token: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?1",
            [ACCESS_TOKEN_KEY],
            |row| row.get(0),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => AuthError::TokenMissing,
            other => AuthError::DbRead {
                message: other.to_string(),
            },
        })?;
    if token.is_empty() {
        return Err(AuthError::TokenMissing);
    }
    Ok(token)
}

pub(crate) fn read_ide_access_token() -> Result<String, AuthError> {
    let path = ide_vscdb_path().ok_or_else(|| AuthError::DbNotFound {
        path: PathBuf::from(VSCDB_SUFFIX),
    })?;
    read_ide_access_token_from(&path)
}

#[allow(dead_code)] // wired in Phase 1 native provider
pub(crate) fn resolve_bearer_token(api_key_env: &str) -> Result<String, AgentError> {
    match std::env::var(api_key_env) {
        Ok(key) if !key.is_empty() => Ok(key),
        _ => read_ide_access_token().map_err(|error| AgentError::Config {
            message: error.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ide_vscdb_path_uses_platform_layout() {
        let Some(path) = ide_vscdb_path() else {
            return;
        };
        assert!(path.ends_with("state.vscdb"));
        #[cfg(target_os = "macos")]
        {
            let rendered = path.to_string_lossy();
            assert!(
                rendered.contains("Library/Application Support/Cursor"),
                "macOS IDE db path should use Application Support, got {path:?}"
            );
            assert!(
                !rendered.contains("/.config/Cursor/"),
                "macOS IDE db path must not use Linux .config layout, got {path:?}"
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(
                path.to_string_lossy().contains("Cursor/User/globalStorage"),
                "Linux/Windows IDE db path unexpected: {path:?}"
            );
        }
    }
}
