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

/// Candidate `state.vscdb` locations across Cursor install layouts.
///
/// Order: platform-primary first, then common fallbacks (e.g. XDG config or a
/// Linux layout inside a macOS remote/dev container).
pub(crate) fn ide_vscdb_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            out.push(
                PathBuf::from(home)
                    .join("Library/Application Support/Cursor")
                    .join(VSCDB_SUFFIX),
            );
        }
    }
    if cfg!(target_os = "windows") {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            out.push(PathBuf::from(appdata).join("Cursor").join(VSCDB_SUFFIX));
        }
        if let Some(local_appdata) = std::env::var_os("LOCALAPPDATA") {
            out.push(PathBuf::from(local_appdata).join("Cursor").join(VSCDB_SUFFIX));
        }
    }
    if cfg!(not(target_os = "macos")) {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            out.push(PathBuf::from(xdg).join(VSCDB_SUFFIX));
        }
        if let Some(home) = std::env::var_os("HOME") {
            out.push(PathBuf::from(home).join(".config/Cursor").join(VSCDB_SUFFIX));
        }
    }
    out
}

pub(crate) fn ide_vscdb_path() -> Option<PathBuf> {
    let candidates = ide_vscdb_candidates();
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .or_else(|| candidates.into_iter().next())
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

    #[test]
    fn ide_vscdb_candidates_include_linux_layout() {
        if std::env::var_os("HOME").is_none() && std::env::var_os("XDG_CONFIG_HOME").is_none() {
            return;
        }
        let candidates = ide_vscdb_candidates();
        assert!(
            candidates.iter().any(|p| p.ends_with(VSCDB_SUFFIX)),
            "candidates={candidates:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ide_vscdb_candidates_include_macos_layout() {
        if std::env::var_os("HOME").is_none() {
            return;
        }
        let candidates = ide_vscdb_candidates();
        assert!(
            candidates.iter().any(|p| {
                p.to_string_lossy()
                    .contains("Library/Application Support/Cursor")
            }),
            "candidates={candidates:?}"
        );
    }
}
