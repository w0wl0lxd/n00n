use std::fs;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{DataDir, StorageError, atomic_write_permissions};

const AUTH_FILE: &str = "auth.json";
const AUTH_FILE_MODE: u32 = 0o600;

#[derive(Debug, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access: String,
    pub refresh: String,
    pub expires: u64,
}

pub fn load_tokens(dir: &DataDir) -> Option<OAuthTokens> {
    let path = dir.path().join(AUTH_FILE);
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

pub fn save_tokens(dir: &DataDir, tokens: &OAuthTokens) -> Result<(), StorageError> {
    let path = dir.path().join(AUTH_FILE);
    let json = serde_json::to_string_pretty(tokens)?;
    atomic_write_permissions(&path, json.as_bytes(), AUTH_FILE_MODE)?;
    debug!(path = %path.display(), "OAuth tokens saved");
    Ok(())
}

pub fn delete_tokens(dir: &DataDir) -> Result<bool, StorageError> {
    let path = dir.path().join(AUTH_FILE);
    if path.exists() {
        fs::remove_file(path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[test]
    fn save_load_delete_round_trip() {
        let tmp = TempDir::new().unwrap();
        let dir = DataDir::from_path(tmp.path().to_path_buf());
        let tokens = OAuthTokens {
            access: "access_tok".into(),
            refresh: "refresh_tok".into(),
            expires: 9999999999,
        };
        save_tokens(&dir, &tokens).unwrap();

        let loaded = load_tokens(&dir).unwrap();
        assert_eq!(loaded.access, "access_tok");
        assert_eq!(loaded.refresh, "refresh_tok");
        assert_eq!(loaded.expires, 9999999999);

        let metadata = fs::metadata(dir.path().join(AUTH_FILE)).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, AUTH_FILE_MODE);

        assert!(delete_tokens(&dir).unwrap());
        assert!(load_tokens(&dir).is_none());
        assert!(!delete_tokens(&dir).unwrap());
    }
}
