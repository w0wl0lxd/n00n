use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use std::{env, thread};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use isahc::ReadResponseExt;
use isahc::config::Configurable;
use n00n_storage::StateDir;
use n00n_storage::auth::{
    OAuthTokens, ProviderCredentials, delete_tokens, load_provider_credentials, load_tokens,
    now_millis, save_provider_credentials, save_tokens,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::{debug, error};

use crate::AgentError;
use crate::providers::{ResolvedAuth, urlenc};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const PROVIDER: &str = "openai";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEVICE_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const DEVICE_AUTH_URL: &str = "https://auth.openai.com/codex/device";
const REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const POLL_SAFETY_MARGIN: Duration = Duration::from_secs(3);
const TOKEN_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_TIMEOUT: Duration = Duration::from_mins(5);
const DEFAULT_ACCESS_TOKEN_LIFETIME_SECS: u64 = 3_600;
const AUTH_DIR: &str = "auth";
const AUTH_LOCK_FILE: &str = "openai.refresh.lock";
const AUTH_DIR_MODE: u32 = 0o700;
const AUTH_LOCK_MODE: u32 = 0o600;
const AUTH_LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const AUTH_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const CODING_PLAN_ADMISSION_DIR: &str = "coding-plan-admission";
const CODING_PLAN_ADMISSION_NAMESPACE: &str = "openai-coding-plan-admission-namespace";
const CODING_PLAN_ADMISSION_NAMESPACE_HOST: &str = "local";
const CODING_PLAN_ADMISSION_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const CODING_PLAN_MAX_SLOTS: u8 = 8;

type LocalCodingPlanSlots = Arc<Mutex<HashSet<u8>>>;
type LocalCodingPlanSlotRegistry = Mutex<HashMap<PathBuf, Weak<Mutex<HashSet<u8>>>>>;

static LOCAL_CODING_PLAN_SLOTS: OnceLock<LocalCodingPlanSlotRegistry> = OnceLock::new();

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    interval: String,
}

#[derive(Deserialize)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    expires_in: Option<u64>,
}

struct CredentialsLock {
    _file: File,
}

struct LocalCodingPlanAdmission {
    slots: LocalCodingPlanSlots,
    slot: u8,
}

impl Drop for LocalCodingPlanAdmission {
    fn drop(&mut self) {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.slot);
    }
}

pub(crate) struct CodingPlanAdmission {
    _file: File,
    _local: LocalCodingPlanAdmission,
    slot: u8,
    scope_hash: String,
}

impl CodingPlanAdmission {
    pub(crate) fn slot(&self) -> u8 {
        self.slot
    }

    pub(crate) fn scope_hash(&self) -> &str {
        &self.scope_hash
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenSyncOutcome {
    Current,
    Adopted,
    Refreshed,
}

pub(crate) struct TokenSync {
    pub(crate) tokens: OAuthTokens,
    pub(crate) outcome: TokenSyncOutcome,
    pub(crate) lock_wait: Duration,
    pub(crate) same_account: bool,
}

fn invalid_credential_store(message: &'static str) -> AgentError {
    AgentError::Config {
        message: message.into(),
    }
}

fn ensure_auth_dir(dir: &StateDir) -> Result<std::path::PathBuf, AgentError> {
    let auth_dir = dir.path().join(AUTH_DIR);
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(AUTH_DIR_MODE);
    builder.create(&auth_dir)?;
    let metadata = fs::symlink_metadata(&auth_dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_credential_store(
            "OpenAI credential directory is not a regular directory",
        ));
    }
    #[cfg(unix)]
    {
        let directory = File::open(&auth_dir)?;
        let opened_metadata = directory.metadata()?;
        if !opened_metadata.is_dir() || !same_file(&metadata, &opened_metadata) {
            return Err(invalid_credential_store(
                "OpenAI credential directory changed while it was opened",
            ));
        }
        directory
            .set_permissions(fs::Permissions::from_mode(AUTH_DIR_MODE))
            .map_err(|_| {
                invalid_credential_store(
                    "OpenAI credential directory permissions could not be secured",
                )
            })?;
    }
    #[cfg(not(unix))]
    let _ = AUTH_DIR_MODE;
    Ok(auth_dir)
}

fn validate_lock_metadata(metadata: &Metadata) -> Result<(), AgentError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid_credential_store(
            "OpenAI credential lock is not a regular file",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn open_existing_lock(path: &Path) -> Result<File, AgentError> {
    let path_metadata = fs::symlink_metadata(path)?;
    validate_lock_metadata(&path_metadata)?;
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let file_metadata = file.metadata()?;
    validate_lock_metadata(&file_metadata)?;
    #[cfg(unix)]
    if !same_file(&path_metadata, &file_metadata) {
        return Err(invalid_credential_store(
            "OpenAI credential lock changed while it was opened",
        ));
    }
    Ok(file)
}

fn open_credentials_lock(path: &Path) -> Result<File, AgentError> {
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(AUTH_LOCK_MODE);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => open_existing_lock(path)?,
        Err(error) => return Err(error.into()),
    };
    validate_lock_metadata(&file.metadata()?)?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(AUTH_LOCK_MODE))
        .map_err(|_| {
            invalid_credential_store("OpenAI credential lock permissions could not be secured")
        })?;
    #[cfg(not(unix))]
    let _ = AUTH_LOCK_MODE;
    Ok(file)
}

#[allow(clippy::manual_unwrap_or)]
fn lock_credentials_with_timeout(
    dir: &StateDir,
    timeout: Duration,
    retry_interval: Duration,
) -> Result<(CredentialsLock, Duration), AgentError> {
    let started = Instant::now();
    let auth_dir = ensure_auth_dir(dir)?;
    let file = open_credentials_lock(&auth_dir.join(AUTH_LOCK_FILE))?;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok((CredentialsLock { _file: file }, started.elapsed())),
            Err(TryLockError::WouldBlock) if started.elapsed() >= timeout => {
                let millis = match u64::try_from(timeout.as_millis()) {
                    Ok(millis) => millis,
                    Err(_) => u64::MAX,
                };
                return Err(AgentError::CredentialLockTimeout { millis });
            }
            Err(TryLockError::WouldBlock) => {
                thread::sleep(retry_interval.min(timeout.saturating_sub(started.elapsed())));
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

fn lock_credentials(dir: &StateDir) -> Result<(CredentialsLock, Duration), AgentError> {
    lock_credentials_with_timeout(dir, AUTH_LOCK_WAIT_TIMEOUT, AUTH_LOCK_RETRY_INTERVAL)
}

fn ensure_coding_plan_admission_dir(dir: &StateDir) -> Result<std::path::PathBuf, AgentError> {
    let admission_dir = ensure_auth_dir(dir)?.join(CODING_PLAN_ADMISSION_DIR);
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(AUTH_DIR_MODE);
    builder.create(&admission_dir)?;
    let metadata = fs::symlink_metadata(&admission_dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_credential_store(
            "OpenAI Coding Plan admission directory is not a regular directory",
        ));
    }
    #[cfg(unix)]
    fs::set_permissions(&admission_dir, fs::Permissions::from_mode(AUTH_DIR_MODE)).map_err(
        |_| {
            invalid_credential_store(
                "OpenAI Coding Plan admission directory permissions could not be secured",
            )
        },
    )?;
    Ok(admission_dir)
}

pub(crate) fn coding_plan_admission_scope(
    dir: &StateDir,
    auth: &ResolvedAuth,
) -> Result<String, AgentError> {
    if let Some((_, account_id)) = auth
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("chatgpt-account-id"))
    {
        let mut digest = Sha256::new();
        digest.update(CODING_PLAN_BASE_URL.as_bytes());
        digest.update(account_id.len().to_le_bytes());
        digest.update(account_id.as_bytes());
        return Ok(format!("{:x}", digest.finalize()));
    }

    let (_lock, _) = lock_credentials(dir)?;
    if let Some(namespace) = load_provider_credentials(dir, CODING_PLAN_ADMISSION_NAMESPACE) {
        if namespace.host.as_deref() == Some(CODING_PLAN_ADMISSION_NAMESPACE_HOST)
            && admission_scope_hash(&namespace.api_key).is_ok()
        {
            return Ok(namespace.api_key);
        }
        return Err(invalid_credential_store(
            "OpenAI Coding Plan admission namespace is invalid",
        ));
    }

    let mut entropy = [0_u8; 32];
    getrandom::fill(&mut entropy).map_err(|error| AgentError::Config {
        message: format!("could not create OpenAI Coding Plan admission namespace: {error}"),
    })?;
    let namespace = format!("{:x}", Sha256::digest(entropy));
    save_provider_credentials(
        dir,
        CODING_PLAN_ADMISSION_NAMESPACE,
        &ProviderCredentials {
            api_key: namespace.clone(),
            host: Some(CODING_PLAN_ADMISSION_NAMESPACE_HOST.into()),
        },
    )?;
    Ok(namespace)
}

fn admission_scope_hash(scope_hash: &str) -> Result<&str, AgentError> {
    if scope_hash.len() == 64 && scope_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(scope_hash);
    }
    Err(AgentError::Config {
        message: "OpenAI Coding Plan account scope could not be safely identified".into(),
    })
}

fn open_admission_slot(path: &Path) -> Result<File, AgentError> {
    let mut create = OpenOptions::new();
    create.create_new(true).read(true).write(true);
    #[cfg(unix)]
    create.mode(AUTH_LOCK_MODE).custom_flags(libc::O_NOFOLLOW);
    match create.open(path) {
        Ok(file) => {
            validate_lock_metadata(&file.metadata()?)?;
            #[cfg(unix)]
            file.set_permissions(fs::Permissions::from_mode(AUTH_LOCK_MODE))
                .map_err(|_| {
                    invalid_credential_store(
                        "OpenAI Coding Plan admission slot permissions could not be secured",
                    )
                })?;
            Ok(file)
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            validate_lock_metadata(&metadata)?;
            let mut open = OpenOptions::new();
            open.read(true).write(true);
            #[cfg(unix)]
            open.custom_flags(libc::O_NOFOLLOW);
            let file = open.open(path)?;
            let opened = file.metadata()?;
            validate_lock_metadata(&opened)?;
            #[cfg(unix)]
            if !same_file(&metadata, &opened) {
                return Err(invalid_credential_store(
                    "OpenAI Coding Plan admission slot changed while it was opened",
                ));
            }
            #[cfg(unix)]
            file.set_permissions(fs::Permissions::from_mode(AUTH_LOCK_MODE))
                .map_err(|_| {
                    invalid_credential_store(
                        "OpenAI Coding Plan admission slot permissions could not be secured",
                    )
                })?;
            Ok(file)
        }
        Err(error) => Err(error.into()),
    }
}

fn local_coding_plan_slots(admission_dir: &Path, scope_hash: &str) -> LocalCodingPlanSlots {
    let registry = LOCAL_CODING_PLAN_SLOTS.get_or_init(|| Mutex::new(HashMap::new()));
    let key = admission_dir.join(scope_hash);
    let mut registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|_, slots| slots.strong_count() > 0);
    if let Some(slots) = registry.get(&key).and_then(Weak::upgrade) {
        return slots;
    }
    let slots = Arc::new(Mutex::new(HashSet::new()));
    registry.insert(key, Arc::downgrade(&slots));
    slots
}

fn reserve_local_coding_plan_slot(
    slots: &LocalCodingPlanSlots,
    slot: u8,
) -> Option<LocalCodingPlanAdmission> {
    let mut occupied = slots
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    occupied.insert(slot).then(|| LocalCodingPlanAdmission {
        slots: Arc::clone(slots),
        slot,
    })
}

pub(crate) fn acquire_coding_plan_admission(
    dir: &StateDir,
    scope_hash: &str,
    slots: u8,
) -> Result<(CodingPlanAdmission, Duration), AgentError> {
    if slots == 0 || slots > CODING_PLAN_MAX_SLOTS {
        return Err(AgentError::Config {
            message: "OpenAI Coding Plan slots must be between 1 and 8".into(),
        });
    }
    let scope_hash = admission_scope_hash(scope_hash)?;
    let admission_dir = ensure_coding_plan_admission_dir(dir)?;
    let local_slots = local_coding_plan_slots(&admission_dir, scope_hash);
    let started = Instant::now();
    let first_slot = fastrand::u8(..slots);
    loop {
        for offset in 0..slots {
            let slot = (first_slot + offset) % slots;
            let Some(local) = reserve_local_coding_plan_slot(&local_slots, slot) else {
                continue;
            };
            let file =
                open_admission_slot(&admission_dir.join(format!("{scope_hash}.{slot}.lock")));
            let file = match file {
                Ok(file) => file,
                Err(error) => {
                    drop(local);
                    return Err(error);
                }
            };
            match file.try_lock() {
                Ok(()) => {
                    return Ok((
                        CodingPlanAdmission {
                            _file: file,
                            _local: local,
                            slot,
                            scope_hash: scope_hash.to_owned(),
                        },
                        started.elapsed(),
                    ));
                }
                Err(TryLockError::WouldBlock) => drop(local),
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        if started.elapsed() >= CODING_PLAN_ADMISSION_WAIT_TIMEOUT {
            #[allow(clippy::manual_unwrap_or)]
            let millis = match u64::try_from(CODING_PLAN_ADMISSION_WAIT_TIMEOUT.as_millis()) {
                Ok(millis) => millis,
                Err(_) => u64::MAX,
            };
            return Err(AgentError::CodingPlanAdmissionTimeout { millis });
        }
        let remaining = CODING_PLAN_ADMISSION_WAIT_TIMEOUT.saturating_sub(started.elapsed());
        let jitter = Duration::from_millis(50 + fastrand::u64(..25));
        thread::sleep(jitter.min(remaining));
    }
}

fn http_client(timeout: Duration) -> Result<isahc::HttpClient, AgentError> {
    isahc::HttpClient::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()
        .map_err(|e| AgentError::Config {
            message: format!("http client: {e}"),
        })
}

fn extract_account_id(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&payload).ok()?;

    claims
        .get("chatgpt_account_id")
        .and_then(|v| v.as_str())
        .or_else(|| {
            claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            claims
                .get("organizations")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|org| org.get("id"))
                .and_then(|v| v.as_str())
        })
        .map(String::from)
}

fn extract_account_id_from_tokens(resp: &TokenResponse) -> Option<String> {
    if let Some(id_token) = &resp.id_token
        && let Some(id) = extract_account_id(id_token)
    {
        return Some(id);
    }
    extract_account_id(&resp.access_token)
}

fn request_device_code() -> Result<DeviceCodeResponse, AgentError> {
    let client = http_client(TOKEN_EXCHANGE_TIMEOUT)?;
    let body = serde_json::json!({"client_id": CLIENT_ID});
    let json_body = serde_json::to_vec(&body)?;

    let request = isahc::Request::builder()
        .method("POST")
        .uri(DEVICE_CODE_URL)
        .header("content-type", "application/json")
        .body(json_body)?;

    let mut resp = client.send(request).map_err(|e| AgentError::Config {
        message: format!("device code request: {e}"),
    })?;

    if resp.status().as_u16() != 200 {
        let body_text = resp.text().unwrap_or_else(|_| "unknown error".into());
        return Err(AgentError::Config {
            message: format!("device code request failed: {body_text}"),
        });
    }

    let body_text = resp.text()?;
    serde_json::from_str(&body_text).map_err(Into::into)
}

fn poll_device_token(device: &DeviceCodeResponse) -> Result<DeviceTokenResponse, AgentError> {
    let client = http_client(POLL_TIMEOUT)?;
    let interval_secs = device.interval.parse::<u64>().unwrap_or_else(|_| 5).max(1);
    let poll_interval = Duration::from_secs(interval_secs) + POLL_SAFETY_MARGIN;
    let deadline = std::time::Instant::now() + POLL_TIMEOUT;

    let body = serde_json::json!({
        "device_auth_id": device.device_auth_id,
        "user_code": device.user_code,
    });
    let json_body = serde_json::to_vec(&body)?;

    loop {
        if std::time::Instant::now() > deadline {
            return Err(AgentError::Config {
                message: "device authorization timed out".into(),
            });
        }

        thread::sleep(poll_interval);

        let request = isahc::Request::builder()
            .method("POST")
            .uri(DEVICE_TOKEN_URL)
            .header("content-type", "application/json")
            .body(json_body.clone())?;

        let mut resp = client.send(request).map_err(|e| AgentError::Config {
            message: format!("device token poll: {e}"),
        })?;

        if resp.status().as_u16() == 200 {
            let body_text = resp.text()?;
            return serde_json::from_str(&body_text).map_err(Into::into);
        }

        let status = resp.status().as_u16();
        if status != 403 && status != 404 {
            let body_text = resp.text().unwrap_or_else(|_| "unknown error".into());
            return Err(AgentError::Config {
                message: format!("device token poll failed ({status}): {body_text}"),
            });
        }
    }
}

fn exchange_device_token(device_token: &DeviceTokenResponse) -> Result<TokenResponse, AgentError> {
    let client = http_client(TOKEN_EXCHANGE_TIMEOUT)?;

    let form_body = format!(
        "grant_type=authorization_code\
         &code={}\
         &redirect_uri={}\
         &client_id={}\
         &code_verifier={}",
        urlenc(&device_token.authorization_code),
        urlenc(REDIRECT_URI),
        urlenc(CLIENT_ID),
        urlenc(&device_token.code_verifier),
    );

    let request = isahc::Request::builder()
        .method("POST")
        .uri(OAUTH_TOKEN_URL)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_body.into_bytes())?;

    let mut resp = client.send(request).map_err(|e| AgentError::Config {
        message: format!("token exchange: {e}"),
    })?;

    if resp.status().as_u16() != 200 {
        let body_text = resp.text().unwrap_or_else(|_| "unknown error".into());
        return Err(AgentError::Config {
            message: format!("token exchange failed: {body_text}"),
        });
    }

    let body_text = resp.text()?;
    serde_json::from_str(&body_text).map_err(Into::into)
}

fn into_oauth_tokens(
    resp: TokenResponse,
    previous_refresh: Option<&str>,
    previous_account_id: Option<&str>,
) -> Result<OAuthTokens, AgentError> {
    let account_id = extract_account_id_from_tokens(&resp)
        .or_else(|| previous_account_id.map(ToOwned::to_owned));
    #[allow(clippy::manual_unwrap_or)]
    let expires_in = match resp.expires_in {
        Some(expires_in) => expires_in,
        None => DEFAULT_ACCESS_TOKEN_LIFETIME_SECS,
    };
    let lifetime_millis = expires_in
        .checked_mul(1_000)
        .ok_or_else(|| AgentError::Config {
            message: "OpenAI token response contained an invalid expiry".into(),
        })?;
    let expires = now_millis()
        .checked_add(lifetime_millis)
        .ok_or_else(|| AgentError::Config {
            message: "OpenAI token response contained an invalid expiry".into(),
        })?;
    let refresh = resp
        .refresh_token
        .filter(|token| !token.is_empty())
        .or_else(|| previous_refresh.map(ToOwned::to_owned))
        .ok_or_else(|| AgentError::Config {
            message: "OpenAI token response omitted a refresh token".into(),
        })?;
    Ok(OAuthTokens {
        access: resp.access_token,
        refresh,
        expires,
        account_id,
    })
}

pub(crate) fn refresh_tokens(tokens: &OAuthTokens) -> Result<OAuthTokens, AgentError> {
    let expired = tokens.is_expired();
    debug!(expired, "refreshing OpenAI OAuth tokens");

    let client = http_client(TOKEN_EXCHANGE_TIMEOUT)?;
    let form_body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        urlenc(&tokens.refresh),
        urlenc(CLIENT_ID),
    );

    let request = isahc::Request::builder()
        .method("POST")
        .uri(OAUTH_TOKEN_URL)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_body.into_bytes())?;

    let mut resp = client.send(request).map_err(|e| AgentError::Config {
        message: format!("OpenAI token refresh: {e}"),
    })?;

    if resp.status().as_u16() != 200 {
        return Err(AgentError::Api {
            status: resp.status().as_u16(),
            message: "OpenAI token refresh was rejected".into(),
        });
    }

    let body_text = resp.text()?;
    let token_resp: TokenResponse = serde_json::from_str(&body_text)?;
    into_oauth_tokens(
        token_resp,
        Some(&tokens.refresh),
        tokens.account_id.as_deref(),
    )
}

pub(crate) fn synchronize_tokens<F>(
    dir: &StateDir,
    observed: &OAuthTokens,
    force_refresh: bool,
    refresh: F,
) -> Result<TokenSync, AgentError>
where
    F: FnOnce(&OAuthTokens) -> Result<OAuthTokens, AgentError>,
{
    let (_lock, lock_wait) = lock_credentials(dir)?;
    let current = load_tokens(dir, PROVIDER).ok_or_else(|| AgentError::Api {
        status: 401,
        message: "OpenAI OAuth credentials are no longer available".into(),
    })?;
    let changed = current.access != observed.access
        || current.refresh != observed.refresh
        || current.expires != observed.expires
        || current.account_id != observed.account_id;
    let same_account = current.account_id.is_some() && current.account_id == observed.account_id;
    if !current.is_expired() && (!force_refresh || changed) {
        return Ok(TokenSync {
            tokens: current,
            outcome: if changed {
                TokenSyncOutcome::Adopted
            } else {
                TokenSyncOutcome::Current
            },
            lock_wait,
            same_account,
        });
    }

    let fresh = refresh(&current)?;
    save_tokens(dir, PROVIDER, &fresh)?;
    Ok(TokenSync {
        tokens: fresh,
        outcome: TokenSyncOutcome::Refreshed,
        lock_wait,
        same_account,
    })
}

pub(crate) fn save_tokens_synchronized(
    dir: &StateDir,
    tokens: &OAuthTokens,
) -> Result<(), AgentError> {
    let (_lock, _) = lock_credentials(dir)?;
    save_tokens(dir, PROVIDER, tokens)?;
    Ok(())
}

pub(crate) fn delete_tokens_synchronized(dir: &StateDir) -> Result<bool, AgentError> {
    let (_lock, _) = lock_credentials(dir)?;
    delete_tokens(dir, PROVIDER).map_err(Into::into)
}

pub(crate) const CODING_PLAN_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

pub(crate) fn build_oauth_resolved(tokens: &OAuthTokens) -> ResolvedAuth {
    ResolvedAuth {
        base_url: None,
        headers: vec![("authorization".into(), format!("Bearer {}", tokens.access))],
    }
}

pub(crate) fn build_coding_plan_resolved(tokens: &OAuthTokens) -> ResolvedAuth {
    let mut headers = vec![("authorization".into(), format!("Bearer {}", tokens.access))];
    if let Some(account_id) = &tokens.account_id {
        headers.push(("chatgpt-account-id".into(), account_id.clone()));
    }
    ResolvedAuth {
        base_url: Some(CODING_PLAN_BASE_URL.into()),
        headers,
    }
}

pub(crate) fn is_oauth(dir: &StateDir) -> bool {
    load_tokens(dir, PROVIDER).is_some()
}

/// Resolve cached `OpenAI` authentication without network access.
///
/// # Errors
///
/// Returns an `AgentError` if no API key or valid OAuth tokens are available.
pub fn resolve_cached(dir: &StateDir) -> Result<ResolvedAuth, AgentError> {
    if let Some(tokens) = load_tokens(dir, PROVIDER) {
        debug!(
            expired = tokens.is_expired(),
            "using cached OpenAI OAuth authentication"
        );
        return Ok(build_oauth_resolved(&tokens));
    }

    if let Ok(key) = env::var("OPENAI_API_KEY") {
        debug!("using OpenAI API key authentication");
        return Ok(ResolvedAuth {
            base_url: None,
            headers: vec![("authorization".into(), format!("Bearer {key}"))],
        });
    }

    if let Some(creds) = n00n_storage::auth::load_provider_credentials(dir, PROVIDER) {
        debug!("using OpenAI saved API key");
        return Ok(ResolvedAuth {
            base_url: None,
            headers: vec![("authorization".into(), format!("Bearer {}", creds.api_key))],
        });
    }

    Err(AgentError::Config {
        message: "not authenticated, run `n00n auth login openai` or set OPENAI_API_KEY".into(),
    })
}

/// Authenticate with `OpenAI` via OAuth device flow.
///
/// # Errors
///
/// Returns an `AgentError` if device code, polling, or token exchange fails.
pub fn login(dir: &StateDir) -> Result<(), AgentError> {
    let device = request_device_code()?;

    println!("Open this URL in your browser:\n\n  {DEVICE_AUTH_URL}\n");
    println!("Enter code: {}\n", device.user_code);
    println!("Waiting for authorization...");

    let device_token = poll_device_token(&device).map_err(|e| {
        error!(error = %e, "OpenAI device authorization failed");
        e
    })?;

    let token_resp = exchange_device_token(&device_token).map_err(|e| {
        error!(error = %e, "OpenAI token exchange failed");
        e
    })?;

    let tokens = into_oauth_tokens(token_resp, None, None)?;
    save_tokens_synchronized(dir, &tokens)?;
    println!("Authenticated successfully.");
    Ok(())
}

/// Clear `OpenAI` OAuth tokens.
///
/// # Errors
///
/// Returns an `AgentError` if token deletion fails.
pub fn logout(dir: &StateDir) -> Result<(), AgentError> {
    if delete_tokens_synchronized(dir)? {
        println!("Logged out of OpenAI.");
    } else {
        println!("Not currently logged in to OpenAI.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use super::*;

    const ADMISSION_SCOPE_A: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ADMISSION_SCOPE_B: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn coding_plan_admission_bounds_multi_provider_concurrency() {
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let state = Arc::new((
            std::sync::Mutex::new((0_usize, false)),
            std::sync::Condvar::new(),
        ));
        let mut workers = Vec::new();

        for _ in 0..8 {
            let worker_dir = dir.clone();
            let worker_active = Arc::clone(&active);
            let worker_peak = Arc::clone(&peak);
            let worker_state = Arc::clone(&state);
            workers.push(std::thread::spawn(move || {
                let (_admission, _) =
                    acquire_coding_plan_admission(&worker_dir, ADMISSION_SCOPE_A, 4).unwrap();
                let current = worker_active.fetch_add(1, Ordering::SeqCst) + 1;
                worker_peak.fetch_max(current, Ordering::SeqCst);
                let (lock, ready) = &*worker_state;
                let mut state = lock.lock().unwrap();
                state.0 += 1;
                ready.notify_all();
                while !state.1 {
                    state = ready.wait(state).unwrap();
                }
                worker_active.fetch_sub(1, Ordering::SeqCst);
            }));
        }

        let (lock, ready) = &*state;
        let mut state = lock.lock().unwrap();
        while state.0 < 4 {
            let (next, timeout) = ready.wait_timeout(state, Duration::from_secs(2)).unwrap();
            state = next;
            assert!(
                !timeout.timed_out() || state.0 >= 4,
                "admission slots were not acquired in time; acquired={}",
                state.0
            );
        }
        assert_eq!(active.load(Ordering::SeqCst), 4);
        assert!(peak.load(Ordering::SeqCst) <= 4);
        state.1 = true;
        ready.notify_all();
        drop(state);
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= 4);
    }

    #[test]
    fn coding_plan_admission_scope_without_account_is_stable_across_bearer_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let first = ResolvedAuth {
            base_url: Some(CODING_PLAN_BASE_URL.into()),
            headers: vec![("authorization".into(), "Bearer first".into())],
        };
        let refreshed = ResolvedAuth {
            base_url: Some(CODING_PLAN_BASE_URL.into()),
            headers: vec![("authorization".into(), "Bearer refreshed".into())],
        };

        let first_scope = coding_plan_admission_scope(&dir, &first).unwrap();
        let refreshed_scope = coding_plan_admission_scope(&dir, &refreshed).unwrap();

        assert_eq!(first_scope, refreshed_scope);
        assert_ne!(first_scope, "first");
        assert_ne!(refreshed_scope, "refreshed");
    }

    #[test]
    fn coding_plan_admission_isolated_by_account_scope() {
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let (first, _) = acquire_coding_plan_admission(&dir, ADMISSION_SCOPE_A, 1).unwrap();
        let (second, _) = acquire_coding_plan_admission(&dir, ADMISSION_SCOPE_B, 1).unwrap();

        assert_eq!(first.slot(), 0);
        assert_eq!(second.slot(), 0);
    }

    #[test]
    fn coding_plan_admission_releases_on_drop_and_error() {
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let (admission, _) = acquire_coding_plan_admission(&dir, ADMISSION_SCOPE_A, 1).unwrap();
        drop(admission);
        let result = (|| -> Result<(), AgentError> {
            let (_admission, _) = acquire_coding_plan_admission(&dir, ADMISSION_SCOPE_A, 1)?;
            Err(AgentError::Cancelled)
        })();

        assert!(matches!(result, Err(AgentError::Cancelled)));
        assert!(acquire_coding_plan_admission(&dir, ADMISSION_SCOPE_A, 1).is_ok());
    }
    fn test_tokens(access: &str, refresh: &str, expires: u64) -> OAuthTokens {
        OAuthTokens {
            access: access.into(),
            refresh: refresh.into(),
            expires,
            account_id: Some("test-account".into()),
        }
    }

    fn copy_tokens(tokens: &OAuthTokens) -> OAuthTokens {
        OAuthTokens {
            access: tokens.access.clone(),
            refresh: tokens.refresh.clone(),
            expires: tokens.expires,
            account_id: tokens.account_id.clone(),
        }
    }

    #[test]
    fn cached_resolution_uses_expired_oauth_without_refreshing() {
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let tokens = OAuthTokens {
            access: "cached-access".into(),
            refresh: "cached-refresh".into(),
            expires: 0,
            account_id: Some("cached-account".into()),
        };
        save_tokens(&dir, PROVIDER, &tokens).unwrap();

        let resolved = resolve_cached(&dir).unwrap();

        assert_eq!(
            resolved.headers,
            vec![("authorization".into(), "Bearer cached-access".into())]
        );
        let persisted = load_tokens(&dir, PROVIDER).unwrap();
        assert_eq!(persisted.refresh, "cached-refresh");
        assert_eq!(persisted.expires, 0);
    }

    #[test]
    fn synchronized_refresh_has_one_winner_and_one_adopter() {
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let expired = test_tokens("old-access", "old-refresh", 0);
        save_tokens(&dir, PROVIDER, &expired).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();

        for _ in 0..2 {
            let worker_dir = dir.clone();
            let observed = copy_tokens(&expired);
            let worker_barrier = Arc::clone(&barrier);
            let worker_refreshes = Arc::clone(&refreshes);
            workers.push(std::thread::spawn(move || {
                worker_barrier.wait();
                synchronize_tokens(&worker_dir, &observed, false, |_| {
                    worker_refreshes.fetch_add(1, Ordering::SeqCst);
                    Ok(test_tokens(
                        "fresh-access",
                        "fresh-refresh",
                        now_millis() + 3_600_000,
                    ))
                })
                .unwrap()
                .outcome
            }));
        }

        barrier.wait();
        let outcomes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == TokenSyncOutcome::Refreshed)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == TokenSyncOutcome::Adopted)
                .count(),
            1
        );
    }

    #[test]
    fn refresh_failures_preserve_shared_credentials() {
        let errors = vec![
            AgentError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "dns unavailable",
            )),
            AgentError::Timeout { secs: 30 },
            AgentError::Api {
                status: 429,
                message: "rate limited".into(),
            },
            AgentError::Api {
                status: 503,
                message: "unavailable".into(),
            },
            AgentError::Api {
                status: 401,
                message: "rejected".into(),
            },
            AgentError::Json(serde_json::from_str::<serde_json::Value>("{").unwrap_err()),
        ];

        for error in errors {
            let temp = tempfile::tempdir().unwrap();
            let dir = StateDir::from_path(temp.path().to_path_buf());
            let expired = test_tokens("old-access", "old-refresh", 0);
            save_tokens(&dir, PROVIDER, &expired).unwrap();

            let result = synchronize_tokens(&dir, &expired, false, |_| Err(error));

            assert!(result.is_err());
            let persisted = load_tokens(&dir, PROVIDER).unwrap();
            assert_eq!(persisted.access, expired.access);
            assert_eq!(persisted.refresh, expired.refresh);
            assert_eq!(persisted.expires, expired.expires);
            assert_eq!(persisted.account_id, expired.account_id);
        }
    }

    #[test]
    fn successful_refresh_retains_omitted_refresh_token() {
        let response: TokenResponse =
            serde_json::from_str(r#"{"access_token":"fresh-access","expires_in":3600}"#).unwrap();

        let tokens =
            into_oauth_tokens(response, Some("old-refresh"), Some("test-account")).unwrap();

        assert_eq!(tokens.refresh, "old-refresh");
        assert_eq!(tokens.account_id.as_deref(), Some("test-account"));
    }

    #[test]
    fn logout_serializes_with_inflight_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let expired = test_tokens("old-access", "old-refresh", 0);
        save_tokens(&dir, PROVIDER, &expired).unwrap();
        let (refresh_started_tx, refresh_started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let refresh_dir = dir.clone();
        let refresh_observed = expired;
        let refresh = std::thread::spawn(move || {
            synchronize_tokens(&refresh_dir, &refresh_observed, false, |_| {
                refresh_started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(test_tokens(
                    "refreshed-access",
                    "refreshed-refresh",
                    now_millis() + 3_600_000,
                ))
            })
            .unwrap();
        });
        refresh_started_rx.recv().unwrap();
        let logout_dir = dir.clone();
        let logout = std::thread::spawn(move || delete_tokens_synchronized(&logout_dir).unwrap());
        release_tx.send(()).unwrap();
        refresh.join().unwrap();
        assert!(logout.join().unwrap());
        assert!(load_tokens(&dir, PROVIDER).is_none());
    }

    #[test]
    fn newer_login_wins_inflight_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let expired = test_tokens("old-access", "old-refresh", 0);
        save_tokens(&dir, PROVIDER, &expired).unwrap();
        let (refresh_started_tx, refresh_started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let refresh_dir = dir.clone();
        let refresh_observed = expired;
        let refresh = std::thread::spawn(move || {
            synchronize_tokens(&refresh_dir, &refresh_observed, false, |_| {
                refresh_started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(test_tokens(
                    "refreshed-access",
                    "refreshed-refresh",
                    now_millis() + 3_600_000,
                ))
            })
            .unwrap();
        });
        refresh_started_rx.recv().unwrap();
        let login = test_tokens("login-access", "login-refresh", now_millis() + 3_600_000);
        let login_dir = dir.clone();
        let expected = copy_tokens(&login);
        let save_login = std::thread::spawn(move || {
            save_tokens_synchronized(&login_dir, &login).unwrap();
        });
        release_tx.send(()).unwrap();
        refresh.join().unwrap();
        save_login.join().unwrap();

        let persisted = load_tokens(&dir, PROVIDER).unwrap();
        assert_eq!(persisted.access, expected.access);
        assert_eq!(persisted.refresh, expected.refresh);
        assert_eq!(persisted.expires, expected.expires);
        assert_eq!(persisted.account_id, expected.account_id);
    }

    #[test]
    fn credential_lock_wait_is_bounded_and_sanitized() {
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let auth_dir = ensure_auth_dir(&dir).unwrap();
        let held = open_credentials_lock(&auth_dir.join(AUTH_LOCK_FILE)).unwrap();
        held.lock().unwrap();

        let Err(error) = lock_credentials_with_timeout(
            &dir,
            Duration::from_millis(40),
            Duration::from_millis(5),
        ) else {
            panic!("contended credential lock unexpectedly succeeded");
        };

        assert!(matches!(
            &error,
            AgentError::CredentialLockTimeout { millis: 40 }
        ));
        assert!(
            !error
                .to_string()
                .contains(temp.path().to_string_lossy().as_ref())
        );
    }

    #[cfg(unix)]
    #[test]
    fn credential_paths_are_private_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let (_lock, _) = lock_credentials(&dir).unwrap();
        let auth_dir = temp.path().join(AUTH_DIR);
        let lock_path = auth_dir.join(AUTH_LOCK_FILE);

        assert_eq!(
            fs::metadata(&auth_dir).unwrap().permissions().mode() & 0o777,
            AUTH_DIR_MODE
        );
        assert_eq!(
            fs::metadata(lock_path).unwrap().permissions().mode() & 0o777,
            AUTH_LOCK_MODE
        );
    }

    #[cfg(unix)]
    #[test]
    fn credential_directory_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        symlink(target.path(), temp.path().join(AUTH_DIR)).unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());

        assert!(matches!(
            lock_credentials(&dir),
            Err(AgentError::Config { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn credential_lock_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let auth_dir = ensure_auth_dir(&dir).unwrap();
        let target = auth_dir.join("target");
        File::create(&target).unwrap();
        symlink(&target, auth_dir.join(AUTH_LOCK_FILE)).unwrap();

        let Err(error) = lock_credentials(&dir) else {
            panic!("credential lock symlink unexpectedly succeeded");
        };

        assert!(matches!(&error, AgentError::Config { .. }));
        assert!(
            !error
                .to_string()
                .contains(temp.path().to_string_lossy().as_ref())
        );
    }

    #[test]
    fn credential_lock_rejects_non_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = StateDir::from_path(temp.path().to_path_buf());
        let auth_dir = ensure_auth_dir(&dir).unwrap();
        fs::create_dir(auth_dir.join(AUTH_LOCK_FILE)).unwrap();

        assert!(matches!(
            lock_credentials(&dir),
            Err(AgentError::Config { .. })
        ));
    }

    #[test]
    fn extract_account_id_from_jwt() {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;

        let header = URL_SAFE_NO_PAD.encode(b"{}");
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::json!({"chatgpt_account_id": "acct_123"})
                .to_string()
                .as_bytes(),
        );
        let token = format!("{header}.{payload}.sig");
        assert_eq!(extract_account_id(&token).as_deref(), Some("acct_123"));

        assert_eq!(extract_account_id("not.a.jwt"), None);
        assert_eq!(extract_account_id("invalid"), None);
    }
}
