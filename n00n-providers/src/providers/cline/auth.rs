//! Cline account authentication: WorkOS device-flow sign-in, API keys, and
//! OAuth token lifecycle against `api.cline.bot`.
//!
//! Protocol constants and flows mirror Cline's own SDK
//! (`sdk/packages/core/src/auth/cline.ts` in `cline/cline`).

use std::thread::sleep;
use std::time::{Duration, Instant};

use isahc::ReadResponseExt;
use isahc::config::Configurable;
use isahc::{HttpClient, Request};
use n00n_storage::StateDir;
use n00n_storage::auth::{
    OAuthTokens, ProviderCredentials, delete_provider_credentials, delete_tokens, load_tokens,
    save_provider_credentials, save_tokens,
};
use serde::Deserialize;
use serde_json::json;
use tracing::debug;

use crate::AgentError;
use crate::providers::{KeyPool, ResolvedAuth, urlenc, user_agent};

pub(crate) const PROVIDER: &str = "cline";
pub(crate) const API_KEY_ENV: &str = "CLINE_API_KEY";
pub(crate) const API_BASE_URL: &str = "https://api.cline.bot";
const USERS_ME_PATH: &str = "/api/v1/users/me";
const REGISTER_PATH: &str = "/api/v1/auth/register";
const REFRESH_PATH: &str = "/api/v1/auth/refresh";
const WORKOS_API_BASE_URL: &str = "https://api.workos.com";
const WORKOS_CLIENT_ID: &str = "client_01K3A541FN8TA3EPPHTD2325AR";
const WORKOS_DEVICE_PATH: &str = "/user_management/authorize/device";
const WORKOS_TOKEN_PATH: &str = "/user_management/authenticate";
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_DEVICE_EXPIRES: Duration = Duration::from_secs(300);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);
const POLL_DEADLINE: Duration = Duration::from_mins(5);
const ERROR_MESSAGE_MAX_LEN: usize = 300;

const NOT_AUTHENTICATED: &str =
    "not authenticated, run `n00n auth login cline` or set CLINE_API_KEY";

fn blocking_client() -> Result<HttpClient, AgentError> {
    HttpClient::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| AgentError::Config {
            message: format!("failed to build HTTP client: {e}"),
        })
}

fn post_text(
    client: &HttpClient,
    url: &str,
    content_type: &str,
    body: &str,
) -> Result<(u16, String), AgentError> {
    let request = Request::builder()
        .method("POST")
        .uri(url)
        .header("content-type", content_type)
        .header("user-agent", user_agent())
        .body(body.as_bytes().to_vec())?;
    let mut response = client.send(request)?;
    let status = response.status().as_u16();
    Ok((status, response.text()?))
}

fn get_text(client: &HttpClient, url: &str, bearer: &str) -> Result<(u16, String), AgentError> {
    let request = Request::builder()
        .method("GET")
        .uri(url)
        .header("authorization", format!("Bearer {bearer}"))
        .header("user-agent", user_agent())
        .body(())?;
    let mut response = client.send(request)?;
    let status = response.status().as_u16();
    Ok((status, response.text()?))
}

fn api_error(status: u16, body: &str) -> AgentError {
    let mut message = body.trim().to_string();
    if message.len() > ERROR_MESSAGE_MAX_LEN {
        message.truncate(ERROR_MESSAGE_MAX_LEN);
    }
    AgentError::Api {
        status,
        message: if message.is_empty() {
            "request failed".into()
        } else {
            message
        },
    }
}

#[derive(Debug, Deserialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

struct DeviceCode {
    device_code: String,
    user_code: String,
    url: String,
    expires: Duration,
    interval: Duration,
}

#[derive(Debug, Deserialize)]
struct WorkOsTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

struct WorkOsTokens {
    access: String,
    refresh: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenResponse {
    success: bool,
    data: TokenData,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenData {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    user_info: Option<UserInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInfo {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    cline_user_id: Option<String>,
}

/// Parse an RFC 3339 timestamp (Cline's `expiresAt`) into epoch milliseconds.
fn parse_rfc3339_millis(value: &str) -> Option<u64> {
    let ts: jiff::Timestamp = value.parse().ok()?;
    u64::try_from(ts.as_millisecond()).ok()
}

fn into_tokens(data: TokenData) -> Result<OAuthTokens, AgentError> {
    let refresh = data.refresh_token.ok_or_else(|| AgentError::Config {
        message: "Cline token response did not include a refresh token".into(),
    })?;
    let expires_at = data
        .expires_at
        .as_deref()
        .ok_or_else(|| AgentError::Config {
            message: "Cline token response did not include an expiry".into(),
        })?;
    let expires = parse_rfc3339_millis(expires_at).ok_or_else(|| AgentError::Config {
        message: format!("Cline token response has an invalid expiry: {expires_at}"),
    })?;
    Ok(OAuthTokens {
        access: data.access_token,
        refresh,
        expires,
        account_id: data.user_info.and_then(|u| u.cline_user_id),
    })
}

fn parse_token_response(status: u16, body: &str) -> Result<OAuthTokens, AgentError> {
    if !(200..300).contains(&status) {
        return Err(api_error(status, body));
    }
    let parsed: TokenResponse = serde_json::from_str(body).map_err(|e| AgentError::Config {
        message: format!("invalid Cline token response: {e}"),
    })?;
    if !parsed.success {
        return Err(AgentError::Config {
            message: "Cline token exchange was rejected".into(),
        });
    }
    into_tokens(parsed.data)
}

fn request_device_code(client: &HttpClient) -> Result<DeviceCode, AgentError> {
    let body = format!("client_id={}", urlenc(WORKOS_CLIENT_ID));
    let url = format!("{WORKOS_API_BASE_URL}{WORKOS_DEVICE_PATH}");
    let (status, text) = post_text(client, &url, "application/x-www-form-urlencoded", &body)?;
    if !(200..300).contains(&status) {
        return Err(api_error(status, &text));
    }
    let parsed: DeviceAuthorization =
        serde_json::from_str(&text).map_err(|e| AgentError::Config {
            message: format!("invalid device authorization response: {e}"),
        })?;
    if let Some(error) = parsed.error {
        return Err(AgentError::Config {
            message: parsed
                .error_description
                .unwrap_or_else(|| format!("device authorization failed: {error}")),
        });
    }
    Ok(DeviceCode {
        device_code: parsed.device_code,
        user_code: parsed.user_code,
        url: parsed
            .verification_uri_complete
            .unwrap_or(parsed.verification_uri),
        expires: parsed
            .expires_in
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_DEVICE_EXPIRES),
        interval: parsed
            .interval
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_POLL_INTERVAL),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollAction {
    Wait,
    SlowDown,
    Denied,
    Failed,
}

fn classify_poll_error(error: Option<&str>) -> PollAction {
    match error {
        Some("authorization_pending") => PollAction::Wait,
        Some("slow_down") => PollAction::SlowDown,
        Some("access_denied" | "expired_token" | "invalid_grant") => PollAction::Denied,
        _ => PollAction::Failed,
    }
}

fn poll_device_tokens(
    client: &HttpClient,
    device: &DeviceCode,
) -> Result<WorkOsTokens, AgentError> {
    let mut interval = device.interval;
    let deadline = Instant::now() + POLL_DEADLINE.min(device.expires);
    while Instant::now() < deadline {
        let body = format!(
            "grant_type={}&device_code={}&client_id={}",
            urlenc(DEVICE_GRANT_TYPE),
            urlenc(&device.device_code),
            urlenc(WORKOS_CLIENT_ID),
        );
        let url = format!("{WORKOS_API_BASE_URL}{WORKOS_TOKEN_PATH}");
        let (status, text) = post_text(client, &url, "application/x-www-form-urlencoded", &body)?;
        if (200..300).contains(&status) {
            let parsed: WorkOsTokenResponse =
                serde_json::from_str(&text).map_err(|e| AgentError::Config {
                    message: format!("invalid WorkOS token response: {e}"),
                })?;
            match (parsed.access_token, parsed.refresh_token) {
                (Some(access), Some(refresh)) => return Ok(WorkOsTokens { access, refresh }),
                _ => {
                    return Err(AgentError::Config {
                        message: "WorkOS token response missing access or refresh token".into(),
                    });
                }
            }
        }
        let parsed: WorkOsTokenResponse =
            serde_json::from_str(&text).map_err(|e| AgentError::Config {
                message: format!("invalid WorkOS error response: {e}"),
            })?;
        match classify_poll_error(parsed.error.as_deref()) {
            PollAction::Wait => sleep(interval),
            PollAction::SlowDown => {
                interval += Duration::from_secs(1);
                sleep(interval);
            }
            PollAction::Denied => {
                return Err(AgentError::Config {
                    message: parsed
                        .error_description
                        .unwrap_or_else(|| "WorkOS authorization was denied".into()),
                });
            }
            PollAction::Failed => return Err(api_error(status, &text)),
        }
    }
    Err(AgentError::SetupRequired {
        message: "Cline device authorization timed out; run `n00n auth login cline` again".into(),
    })
}

fn register_tokens(client: &HttpClient, tokens: &WorkOsTokens) -> Result<OAuthTokens, AgentError> {
    let body = json!({
        "accessToken": tokens.access,
        "refreshToken": tokens.refresh,
    })
    .to_string();
    let (status, text) = post_text(
        client,
        &format!("{API_BASE_URL}{REGISTER_PATH}"),
        "application/json",
        &body,
    )?;
    parse_token_response(status, &text)
}

fn register_and_store(
    client: &HttpClient,
    dir: &StateDir,
    tokens: &WorkOsTokens,
) -> Result<OAuthTokens, AgentError> {
    let oauth = register_tokens(client, tokens)?;
    save_tokens(dir, PROVIDER, &oauth)?;
    Ok(oauth)
}

fn fetch_user_email(client: &HttpClient, access: &str) -> Option<String> {
    let (status, text) =
        get_text(client, &format!("{API_BASE_URL}{USERS_ME_PATH}"), access).ok()?;
    if status != 200 {
        debug!(status, "Cline user profile unavailable after sign-in");
        return None;
    }
    let me: UserInfo = serde_json::from_str(&text).ok()?;
    Some(match (me.name, me.email) {
        (Some(name), Some(email)) => format!("{name} <{email}>"),
        (Some(name), None) => name,
        (None, Some(email)) => email,
        (None, None) => "Cline account".into(),
    })
}

/// Interactive browser sign-in via the WorkOS device flow (the same flow the
/// Cline CLI uses). Blocks until the user completes sign-in in the browser.
///
/// # Errors
///
/// Returns an `AgentError` when the device code request, browser
/// authorization, token exchange, or credential storage fails.
pub fn login_device(dir: &StateDir) -> Result<(), AgentError> {
    let client = blocking_client()?;
    let device = request_device_code(&client)?;

    println!("Open this URL in your browser:\n\n  {}\n", device.url);
    println!("Enter code: {}\n", device.user_code);
    println!("Waiting for authorization...");

    let workos = poll_device_tokens(&client, &device)?;
    let oauth = register_and_store(&client, dir, &workos)?;
    if let Some(identity) = fetch_user_email(&client, &oauth.access) {
        println!("Signed in as {identity}");
    }
    println!("Authenticated successfully.");
    Ok(())
}

/// Store an API key created at app.cline.bot (Settings > API Keys). The key
/// is verified against the account API before it is stored.
///
/// # Errors
///
/// Returns an `AgentError` when the key cannot be verified or stored.
pub fn save_api_key(dir: &StateDir, key: &str) -> Result<(), AgentError> {
    let client = blocking_client()?;
    let (status, text) = get_text(&client, &format!("{API_BASE_URL}{USERS_ME_PATH}"), key)?;
    if status != 200 {
        return Err(api_error(status, &text));
    }
    save_provider_credentials(
        dir,
        PROVIDER,
        &ProviderCredentials {
            api_key: key.to_string(),
            host: None,
        },
    )?;
    Ok(())
}

/// Remove stored Cline credentials (OAuth tokens and API key).
///
/// # Errors
///
/// Returns an `AgentError` when credential deletion fails.
pub fn logout(dir: &StateDir) -> Result<(), AgentError> {
    let mut removed = delete_tokens(dir, PROVIDER)?;
    if delete_provider_credentials(dir, PROVIDER)? {
        removed = true;
    }
    if removed {
        println!("Logged out of Cline.");
    } else {
        println!("Not currently logged in to Cline.");
    }
    Ok(())
}

/// Whether OAuth account tokens are stored for Cline.
#[must_use]
pub fn has_oauth_tokens(dir: &StateDir) -> bool {
    load_tokens(dir, PROVIDER).is_some()
}

/// Resolve authentication for API requests: `CLINE_API_KEY` (env, saved
/// credentials, or providers.toml) first, then stored OAuth tokens.
pub(crate) fn resolve_runtime_auth() -> Result<ResolvedAuth, AgentError> {
    match KeyPool::resolve(PROVIDER, API_KEY_ENV) {
        Ok(pool) => Ok(resolved_auth(pool.current())),
        Err(key_err) => {
            let Ok(dir) = StateDir::resolve() else {
                return Err(key_err);
            };
            match ensure_fresh_tokens(&dir) {
                Ok(Some(tokens)) => Ok(resolved_auth(&tokens.access)),
                Ok(None) => Err(key_err),
                Err(err) => Err(err),
            }
        }
    }
}

pub(crate) fn resolved_auth(key: &str) -> ResolvedAuth {
    ResolvedAuth {
        base_url: None,
        headers: vec![
            ("authorization".into(), format!("Bearer {key}")),
            ("x-title".into(), "n00n".into()),
            (
                "http-referer".into(),
                "https://github.com/w0wl0lxd/n00n".into(),
            ),
        ],
    }
}

/// Load stored OAuth tokens, refreshing them when close to expiry.
///
/// Mirrors the Cline SDK contract: a transient refresh failure keeps the
/// current token while it is still plausibly valid, and only a hard-expired
/// token (or a rejected refresh token) fails the caller.
pub(crate) fn ensure_fresh_tokens(dir: &StateDir) -> Result<Option<OAuthTokens>, AgentError> {
    let Some(tokens) = load_tokens(dir, PROVIDER) else {
        return Ok(None);
    };
    if !tokens.is_expired() {
        return Ok(Some(tokens));
    }
    debug!("cline access token expired, refreshing");
    let refresh_result = blocking_client().and_then(|client| {
        let body = json!({
            "refreshToken": tokens.refresh,
            "grantType": "refresh_token",
        })
        .to_string();
        let (status, text) = post_text(
            &client,
            &format!("{API_BASE_URL}{REFRESH_PATH}"),
            "application/json",
            &body,
        )?;
        parse_token_response(status, &text)
    });
    match refresh_result {
        Ok(fresh) => {
            save_tokens(dir, PROVIDER, &fresh)?;
            Ok(Some(fresh))
        }
        Err(err) if tokens.is_hard_expired() => {
            tracing::warn!(
                error = %err,
                "cline token refresh failed with an expired token; re-authentication required"
            );
            Err(AgentError::SetupRequired {
                message: NOT_AUTHENTICATED.into(),
            })
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                "cline token refresh failed; retrying with the current token"
            );
            Ok(Some(tokens))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TOKEN_BODY: &str = r#"{
        "success": true,
        "data": {
            "accessToken": "access-1",
            "refreshToken": "refresh-1",
            "tokenType": "Bearer",
            "expiresAt": "2026-01-15T10:30:00.500Z",
            "userInfo": {
                "subject": "sub-1",
                "email": "dev@example.com",
                "name": "Dev",
                "clineUserId": "user_123",
                "accounts": ["org_1"]
            }
        }
    }"#;

    #[test]
    fn token_response_parses_into_oauth_tokens() {
        let oauth = parse_token_response(200, SAMPLE_TOKEN_BODY).unwrap();
        assert_eq!(oauth.access, "access-1");
        assert_eq!(oauth.refresh, "refresh-1");
        assert_eq!(oauth.account_id.as_deref(), Some("user_123"));
    }

    #[test]
    fn token_response_rejects_missing_refresh() {
        let body = r#"{"success": true, "data": {"accessToken": "a", "expiresAt": "2026-01-15T10:30:00Z"}}"#;
        let err = parse_token_response(200, body).unwrap_err();
        assert!(err.to_string().contains("refresh token"));
    }

    #[test]
    fn token_response_rejects_missing_expiry() {
        let body = r#"{"success": true, "data": {"accessToken": "a", "refreshToken": "r"}}"#;
        let err = parse_token_response(200, body).unwrap_err();
        assert!(err.to_string().contains("expiry"));
    }

    #[test]
    fn token_response_rejects_failure_flag() {
        let body = r#"{
            "success": false,
            "data": {
                "accessToken": "a",
                "refreshToken": "r",
                "expiresAt": "2026-01-15T10:30:00Z"
            }
        }"#;
        let err = parse_token_response(200, body).unwrap_err();
        assert!(err.to_string().contains("rejected"));
    }

    #[test]
    fn token_response_rejects_error_status() {
        let err = parse_token_response(401, r#"{"code": "unauthorized"}"#).unwrap_err();
        assert!(err.to_string().contains("401"));
    }

    #[test]
    fn token_response_rejects_invalid_expiry() {
        let body = r#"{
            "success": true,
            "data": {
                "accessToken": "a",
                "refreshToken": "r",
                "expiresAt": "not-a-timestamp"
            }
        }"#;
        let err = parse_token_response(200, body).unwrap_err();
        assert!(err.to_string().contains("invalid expiry"));
    }

    #[test_case("2026-01-15T10:30:00Z" => Some(1_768_473_000_000); "seconds_utc")]
    #[test_case("2026-01-15T10:30:00.500Z" => Some(1_768_473_000_500); "millis_utc")]
    #[test_case("2026-01-15T10:30:00+05:30" => Some(1_768_453_200_000); "positive_offset")]
    #[test_case("2026-01-15T10:30:00-01:00" => Some(1_768_476_600_000); "negative_offset")]
    #[test_case("1970-01-01T00:00:00Z" => Some(0); "epoch")]
    #[test_case("not-a-date" => None; "garbage")]
    #[test_case("" => None; "empty")]
    fn rfc3339_expires_parse(value: &str) -> Option<u64> {
        parse_rfc3339_millis(value)
    }

    #[test_case(Some("authorization_pending") => PollAction::Wait; "pending")]
    #[test_case(Some("slow_down") => PollAction::SlowDown; "slow_down")]
    #[test_case(Some("access_denied") => PollAction::Denied; "denied")]
    #[test_case(Some("expired_token") => PollAction::Denied; "expired")]
    #[test_case(Some("invalid_grant") => PollAction::Denied; "invalid_grant")]
    #[test_case(Some("server_error") => PollAction::Failed; "unknown_error")]
    #[test_case(None => PollAction::Failed; "no_error")]
    fn poll_errors_classify(error: Option<&str>) -> PollAction {
        classify_poll_error(error)
    }

    #[test]
    fn resolved_auth_carries_identity_headers() {
        let auth = resolved_auth("sk-test");
        assert!(auth.base_url.is_none());
        assert_eq!(
            auth.headers,
            vec![
                ("authorization".to_string(), "Bearer sk-test".to_string()),
                ("x-title".to_string(), "n00n".to_string()),
                (
                    "http-referer".to_string(),
                    "https://github.com/w0wl0lxd/n00n".to_string(),
                ),
            ]
        );
    }
}
