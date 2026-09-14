use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{StateDir, StorageError, atomic_write_permissions};

const AUTH_DIR: &str = "auth";
const AUTH_FILE_MODE: u32 = 0o600;
const REFRESH_BUFFER_SECS: u64 = 60;
const MASK_VISIBLE_CHARS: usize = 4;
const MASK_MIN_CHARS: usize = MASK_VISIBLE_CHARS * 2;

#[derive(Debug, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access: String,
    pub refresh: String,
    pub expires: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl OAuthTokens {
    #[must_use]
    pub fn is_expired(&self) -> bool {
        now_millis() + REFRESH_BUFFER_SECS * 1000 >= self.expires
    }

    #[must_use]
    pub fn is_hard_expired(&self) -> bool {
        now_millis() >= self.expires
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct McpAuthData {
    pub server_url: String,
    pub tokens: Option<OAuthTokens>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub client_secret_expires_at: Option<u64>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCredentials {
    pub api_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl ProviderCredentials {
    #[must_use]
    pub fn masked_api_key(&self) -> String {
        let char_count = self.api_key.chars().count();
        if char_count > MASK_MIN_CHARS {
            let prefix: String = self.api_key.chars().take(MASK_VISIBLE_CHARS).collect();
            let suffix: String = self
                .api_key
                .chars()
                .skip(char_count.saturating_sub(MASK_VISIBLE_CHARS))
                .collect();
            format!("{prefix}...{suffix}")
        } else {
            "****".to_string()
        }
    }
}

#[must_use]
pub fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or_else(
        |_| 0,
        |d| u64::try_from(d.as_millis()).unwrap_or_else(|_| u64::MAX),
    )
}

/// Auth file names become path components, so they must not contain
/// separators or parent-directory components.
fn valid_auth_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

fn auth_path(dir: &StateDir, filename: &str) -> Option<PathBuf> {
    valid_auth_name(filename).then(|| dir.path().join(AUTH_DIR).join(format!("{filename}.json")))
}

fn auth_path_checked(dir: &StateDir, filename: &str) -> Result<PathBuf, StorageError> {
    auth_path(dir, filename).ok_or_else(|| StorageError::InvalidFileName(filename.to_owned()))
}

fn load_auth<T: DeserializeOwned>(path: &Path) -> Option<T> {
    fs::read_to_string(path)
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
}

fn save_auth(path: &Path, data: &impl Serialize) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(data)?;
    atomic_write_permissions(path, json.as_bytes(), AUTH_FILE_MODE)?;
    debug!("auth data saved");
    Ok(())
}

fn delete_auth(path: &Path) -> Result<bool, StorageError> {
    if path.exists() {
        fs::remove_file(path)?;
        return Ok(true);
    }
    Ok(false)
}

#[must_use]
pub fn load_tokens(dir: &StateDir, provider: &str) -> Option<OAuthTokens> {
    load_auth(&auth_path(dir, provider)?)
}

/// # Errors
/// Returns an error if the file cannot be written or its permissions cannot be set.
pub fn save_tokens(
    dir: &StateDir,
    provider: &str,
    tokens: &OAuthTokens,
) -> Result<(), StorageError> {
    save_auth(&auth_path_checked(dir, provider)?, tokens)
}

/// # Errors
/// Returns an error if the file cannot be removed.
pub fn delete_tokens(dir: &StateDir, provider: &str) -> Result<bool, StorageError> {
    delete_auth(&auth_path_checked(dir, provider)?)
}

#[must_use]
pub fn load_mcp_auth(dir: &StateDir, server_name: &str, expected_url: &str) -> Option<McpAuthData> {
    let data: McpAuthData = load_auth(&auth_path(dir, &format!("mcp-{server_name}"))?)?;
    if data.server_url != expected_url {
        return None;
    }
    if let Some(expires_at) = data.client_secret_expires_at
        && now_millis() / 1000 >= expires_at
    {
        return None;
    }
    Some(data)
}

/// # Errors
/// Returns an error if the file cannot be written or its permissions cannot be set.
pub fn save_mcp_auth(
    dir: &StateDir,
    server_name: &str,
    data: &McpAuthData,
) -> Result<(), StorageError> {
    save_auth(
        &auth_path_checked(dir, &format!("mcp-{server_name}"))?,
        data,
    )
}

/// # Errors
/// Returns an error if the file cannot be removed.
pub fn delete_mcp_auth(dir: &StateDir, server_name: &str) -> Result<bool, StorageError> {
    delete_auth(&auth_path_checked(dir, &format!("mcp-{server_name}"))?)
}

#[must_use]
pub fn load_provider_credentials(dir: &StateDir, slug: &str) -> Option<ProviderCredentials> {
    load_auth(&auth_path(dir, slug)?)
}

/// # Errors
/// Returns an error if the file cannot be written or its permissions cannot be set.
pub fn save_provider_credentials(
    dir: &StateDir,
    slug: &str,
    creds: &ProviderCredentials,
) -> Result<(), StorageError> {
    save_auth(&auth_path_checked(dir, slug)?, creds)
}

/// # Errors
/// Returns an error if the file cannot be removed.
pub fn delete_provider_credentials(dir: &StateDir, slug: &str) -> Result<bool, StorageError> {
    delete_auth(&auth_path_checked(dir, slug)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;
    use test_case::test_case;

    const TEST_URL: &str = "https://mcp.example.com";

    fn test_mcp_data() -> McpAuthData {
        McpAuthData {
            server_url: TEST_URL.into(),
            tokens: None,
            client_id: "client-123".into(),
            client_secret: None,
            client_secret_expires_at: None,
            redirect_uri: None,
        }
    }

    #[test_case(0,                              true  ; "epoch_is_expired")]
    #[test_case(now_millis() + 3_600_000,       false ; "future_is_valid")]
    fn token_expiry(expires: u64, expected: bool) {
        let tokens = OAuthTokens {
            access: "a".into(),
            refresh: "r".into(),
            expires,
            account_id: None,
        };
        assert_eq!(tokens.is_expired(), expected);
    }

    #[test]
    fn save_load_delete_round_trip() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let tokens = OAuthTokens {
            access: "access_tok".into(),
            refresh: "refresh_tok".into(),
            expires: 9_999_999_999,
            account_id: None,
        };
        save_tokens(&dir, "anthropic", &tokens).unwrap();

        let loaded = load_tokens(&dir, "anthropic").unwrap();
        assert_eq!(loaded.access, "access_tok");
        assert_eq!(loaded.refresh, "refresh_tok");
        assert_eq!(loaded.expires, 9_999_999_999);

        #[cfg(unix)]
        {
            let metadata = fs::metadata(auth_path(&dir, "anthropic").unwrap()).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, AUTH_FILE_MODE);
        }

        assert!(delete_tokens(&dir, "anthropic").unwrap());
        assert!(load_tokens(&dir, "anthropic").is_none());
        assert!(!delete_tokens(&dir, "anthropic").unwrap());
    }

    #[test]
    fn mcp_auth_round_trip() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let data = McpAuthData {
            tokens: Some(OAuthTokens {
                access: "acc".into(),
                refresh: "ref".into(),
                expires: 9_999_999_999,
                account_id: None,
            }),
            ..test_mcp_data()
        };
        save_mcp_auth(&dir, "srv", &data).unwrap();
        let loaded = load_mcp_auth(&dir, "srv", TEST_URL).unwrap();
        assert_eq!(loaded.client_id, "client-123");
        assert_eq!(loaded.tokens.unwrap().access, "acc");
    }

    #[test_case(
        &test_mcp_data(),
        "https://other.example.com"
        ; "url_mismatch"
    )]
    #[test_case(
        &McpAuthData {
            client_secret: Some("s".into()),
            client_secret_expires_at: Some(1),
            ..test_mcp_data()
        },
        TEST_URL
        ; "expired_client_secret"
    )]
    fn mcp_auth_load_returns_none(data: &McpAuthData, lookup_url: &str) {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        save_mcp_auth(&dir, "srv", data).unwrap();
        assert!(load_mcp_auth(&dir, "srv", lookup_url).is_none());
    }

    #[test_case("123é56789", "123é...6789" ; "non_ascii_prefix_boundary")]
    #[test_case("1234567éabc", "1234...éabc" ; "non_ascii_suffix_boundary")]
    #[test_case("sk-abcdef-1234", "sk-a...1234" ; "ascii_keys_keep_masking")]
    #[test_case("short", "****" ; "short_keys_are_fully_masked")]
    fn masked_api_key_never_slices_inside_a_character(api_key: &str, expected: &str) {
        let creds = ProviderCredentials {
            api_key: api_key.into(),
            host: None,
        };
        assert_eq!(creds.masked_api_key(), expected);
    }

    #[test]
    fn auth_names_cannot_escape_the_auth_directory() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let stem = format!("n00n-escape-test-{}", std::process::id());
        let escaped = tmp.path().parent().unwrap().join(format!("{stem}.json"));
        let traversal = format!("srv/../../../{stem}");

        let result = save_mcp_auth(&dir, &traversal, &test_mcp_data());

        assert!(
            !escaped.exists(),
            "auth write escaped the state directory: {}",
            escaped.display()
        );
        let error = result.expect_err("traversal name must be rejected");
        assert!(matches!(error, StorageError::InvalidFileName(_)));
        assert!(load_mcp_auth(&dir, &traversal, TEST_URL).is_none());
        assert!(delete_mcp_auth(&dir, &traversal).is_err());
    }
}
