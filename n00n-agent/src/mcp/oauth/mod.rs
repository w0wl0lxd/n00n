pub mod callback;
pub mod discovery;
pub mod manual;
pub mod pkce;
pub mod registration;
pub mod token;

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_lite::future;
use isahc::HttpClient;
use isahc::config::{Configurable, RedirectPolicy};
use n00n_storage::StateDir;
use n00n_storage::auth::{McpAuthData, load_mcp_auth, save_mcp_auth};
use tracing::{info, warn};

use self::callback::{CallbackResult, CallbackServer};
use self::discovery::parse_www_authenticate;
use super::error::McpError;

const AUTH_TIMEOUT: Duration = Duration::from_mins(10);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// In-band refresh blocks requests waiting on the transport's auth lock, so it
/// gets a much tighter budget than the interactive flow.
const SILENT_REFRESH_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("network error: {0}")]
    Network(String),
    #[error("server rejected request: HTTP {status} {body}")]
    ServerRejected { status: u16, body: String },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Clone, Copy)]
pub enum Interaction {
    Cli,
    Background,
}

/// Authenticate with an MCP server using OAuth.
///
/// # Errors
///
/// Returns an error if discovery, registration, code exchange, or token refresh fails.
pub async fn authenticate(
    server_name: &str,
    server_url: &str,
    www_authenticate: Option<&str>,
    storage: &StateDir,
    interaction: Interaction,
) -> Result<McpAuthData, McpError> {
    let wrap = |e: OAuthError| McpError::OAuthFailed {
        server: server_name.into(),
        reason: e.to_string(),
    };
    let client =
        build_http_client(HTTP_TIMEOUT).map_err(|e| wrap(OAuthError::Other(e.to_string())))?;

    if let Some(existing) = load_mcp_auth(storage, server_name, server_url)
        && let Some(ref tokens) = existing.tokens
        && !tokens.is_expired()
    {
        return Ok(existing);
    }

    match silent_refresh(storage, server_name, server_url).await {
        Ok(Some(data)) => return Ok(data),
        Ok(None) => {}
        Err(e) => {
            warn!(server = server_name, error = %e, "token refresh failed, starting full flow");
        }
    }

    let www_auth = www_authenticate.and_then(parse_www_authenticate);
    let (auth_server, resource_meta) =
        discover_and_validate_auth_server(&client, server_url, www_auth.as_ref(), &wrap).await?;

    let (callback, reg, pkce, state, scope) = prepare_oauth_flow(
        &client,
        storage,
        server_name,
        server_url,
        &auth_server,
        &resource_meta,
        www_auth.as_ref(),
        &wrap,
    )
    .await?;

    let redirect_uri = callback.redirect_uri();

    let auth_url = build_authorization_url(
        &auth_server.authorization_endpoint,
        &reg.client_id,
        &redirect_uri,
        &state,
        &pkce.challenge,
        scope.as_deref(),
        server_url,
    );

    info!(server = server_name, endpoint = %auth_server.authorization_endpoint, "starting OAuth authorization");
    let result = run_oauth_flow(
        server_name,
        &auth_url,
        callback,
        &redirect_uri,
        &state,
        interaction,
    )
    .await
    .map_err(|e| wrap(OAuthError::Other(e)))?;

    let tokens = exchange_token(
        &client,
        &auth_server.token_endpoint,
        &result.code,
        &redirect_uri,
        &pkce.verifier,
        &reg.client_id,
        reg.client_secret.as_deref(),
        server_url,
    )
    .await
    .map_err(&wrap)?;

    let data = McpAuthData {
        server_url: server_url.to_string(),
        tokens: Some(tokens),
        client_id: reg.client_id,
        client_secret: reg.client_secret,
        client_secret_expires_at: reg.client_secret_expires_at,
        redirect_uri: Some(redirect_uri),
    };

    save_mcp_auth(storage, server_name, &data)
        .map_err(|e| wrap(OAuthError::Other(e.to_string())))?;
    info!(server = server_name, "OAuth authentication complete");
    Ok(data)
}

async fn discover_and_validate_auth_server(
    client: &HttpClient,
    server_url: &str,
    www_auth: Option<&discovery::WwwAuthenticateInfo>,
    wrap: &impl Fn(OAuthError) -> McpError,
) -> Result<(discovery::AuthServerMetadata, discovery::ResourceMetadata), McpError> {
    let resource_meta = discovery::discover_resource_metadata(client, server_url, www_auth)
        .await
        .map_err(wrap)?;

    let auth_server_url = resource_meta
        .authorization_servers
        .first()
        .cloned()
        .unwrap_or_else(|| discovery::server_origin(server_url));

    let auth_server = discovery::discover_auth_server(client, &auth_server_url)
        .await
        .map_err(wrap)?;

    if !auth_server.code_challenge_methods_supported.is_empty()
        && !auth_server
            .code_challenge_methods_supported
            .iter()
            .any(|m| m == "S256")
    {
        return Err(wrap(OAuthError::Other(
            "server does not support S256 PKCE".into(),
        )));
    }

    Ok((auth_server, resource_meta))
}

async fn prepare_oauth_flow(
    client: &HttpClient,
    storage: &StateDir,
    server_name: &str,
    server_url: &str,
    auth_server: &discovery::AuthServerMetadata,
    resource_meta: &discovery::ResourceMetadata,
    www_auth: Option<&discovery::WwwAuthenticateInfo>,
    wrap: &impl Fn(OAuthError) -> McpError,
) -> Result<
    (
        CallbackServer,
        registration::ClientRegistration,
        pkce::PkceChallenge,
        String,
        Option<String>,
    ),
    McpError,
> {
    let callback = CallbackServer::bind()
        .await
        .map_err(|e| wrap(OAuthError::Other(e)))?;
    let redirect_uri = callback.redirect_uri();

    let reg = obtain_client_registration(
        client,
        storage,
        server_name,
        server_url,
        &redirect_uri,
        auth_server.registration_endpoint.as_ref(),
        wrap,
    )
    .await?;

    let pkce = pkce::generate().map_err(wrap)?;

    let mut state_buf = [0u8; 16];
    getrandom::fill(&mut state_buf)
        .map_err(|e| wrap(OAuthError::Other(format!("CSPRNG unavailable: {e}"))))?;
    let state = URL_SAFE_NO_PAD.encode(state_buf);

    let scope = www_auth
        .and_then(|w| w.scope.clone())
        .or_else(|| resource_meta.scopes_supported.as_ref().map(|s| s.join(" ")));

    Ok((callback, reg, pkce, state, scope))
}

async fn exchange_token(
    client: &HttpClient,
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    client_id: &str,
    client_secret: Option<&str>,
    resource: &str,
) -> Result<n00n_storage::auth::OAuthTokens, OAuthError> {
    let exchange_ctx = token::OAuthCodeExchange {
        client,
        token_endpoint,
        code,
        redirect_uri,
        code_verifier,
        client_id,
        client_secret,
        resource,
    };
    token::exchange_code(exchange_ctx).await
}

async fn obtain_client_registration(
    client: &HttpClient,
    storage: &StateDir,
    server_name: &str,
    server_url: &str,
    redirect_uri: &str,
    registration_endpoint: Option<&String>,
    wrap: &impl Fn(OAuthError) -> McpError,
) -> Result<registration::ClientRegistration, McpError> {
    if let Some(existing) = load_mcp_auth(storage, server_name, server_url)
        && existing.redirect_uri.as_deref() == Some(redirect_uri)
    {
        return Ok(registration::ClientRegistration {
            client_id: existing.client_id,
            client_secret: existing.client_secret,
            client_secret_expires_at: existing.client_secret_expires_at,
        });
    }

    if let Some(endpoint) = registration_endpoint {
        return registration::register_client(client, endpoint, redirect_uri)
            .await
            .map_err(wrap);
    }

    Err(wrap(OAuthError::Other(
        "no stored client and server has no registration endpoint".into(),
    )))
}

async fn run_oauth_flow(
    server_name: &str,
    auth_url: &str,
    callback: CallbackServer,
    redirect_uri: &str,
    state: &str,
    interaction: Interaction,
) -> Result<CallbackResult, String> {
    match interaction {
        Interaction::Cli => {
            eprintln!("\nOpen this URL in your browser:\n\n  {auth_url}\n");

            if is_headless() {
                info!(
                    server = server_name,
                    "no display detected, skipping browser open"
                );
            } else if let Err(e) = open::that(auth_url) {
                warn!(server = server_name, error = %e, "failed to open browser");
            }

            eprintln!("Waiting for callback on 127.0.0.1:{}...", callback.port);
            eprintln!("If this machine has no browser, log in on another device and paste");
            eprintln!("the full redirect URL ({redirect_uri}?...) here:");

            let callback_or_paste = future::race(
                callback.wait_for_callback(state),
                manual::wait_for_paste(state),
            );

            future::race(callback_or_paste, auth_timeout()).await
        }
        Interaction::Background => {
            let cause = if is_headless() {
                Some("no display to open a browser".to_string())
            } else {
                open::that(auth_url)
                    .err()
                    .map(|e| format!("failed to open browser: {e}"))
            };
            match cause {
                Some(cause) => Err(format!("{cause}; run 'n00n mcp auth {server_name}'")),
                None => future::race(callback.wait_for_callback(state), auth_timeout()).await,
            }
        }
    }
}

/// Refresh stored tokens without any user interaction. `Ok(None)` means an
/// interactive flow is required (no stored auth or no refresh token).
///
/// # Errors
/// Returns `OAuthError` if token refresh fails or discovery fails.
pub async fn silent_refresh(
    storage: &StateDir,
    server_name: &str,
    server_url: &str,
) -> Result<Option<McpAuthData>, OAuthError> {
    let Some(existing) = load_mcp_auth(storage, server_name, server_url) else {
        return Ok(None);
    };

    let Some(ref tokens) = existing.tokens else {
        return Ok(None);
    };

    if tokens.refresh.is_empty() {
        return Ok(None);
    }

    let client = build_http_client(SILENT_REFRESH_HTTP_TIMEOUT)
        .map_err(|e| OAuthError::Other(e.to_string()))?;

    let auth_server = discover_auth_server_for(&client, server_url, None).await?;

    let new_tokens = token::refresh_token(
        &client,
        &auth_server.token_endpoint,
        &tokens.refresh,
        &existing.client_id,
        existing.client_secret.as_deref(),
        server_url,
    )
    .await?;

    let data = McpAuthData {
        tokens: Some(new_tokens),
        ..existing
    };

    save_mcp_auth(storage, server_name, &data).map_err(|e| OAuthError::Other(e.to_string()))?;
    info!(server = server_name, "MCP OAuth tokens refreshed");

    Ok(Some(data))
}

async fn discover_auth_server_for(
    client: &HttpClient,
    server_url: &str,
    www_auth: Option<&discovery::WwwAuthenticateInfo>,
) -> Result<discovery::AuthServerMetadata, OAuthError> {
    let resource_meta = discovery::discover_resource_metadata(client, server_url, www_auth).await?;
    let auth_server_url = resource_meta
        .authorization_servers
        .first()
        .cloned()
        .unwrap_or_else(|| discovery::server_origin(server_url));
    discovery::discover_auth_server(client, &auth_server_url).await
}

async fn auth_timeout() -> Result<CallbackResult, String> {
    smol::Timer::after(AUTH_TIMEOUT).await;
    Err(format!(
        "OAuth flow timed out after {} minutes",
        AUTH_TIMEOUT.as_secs() / 60
    ))
}

fn is_headless() -> bool {
    cfg!(target_os = "linux")
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
}

fn build_http_client(timeout: Duration) -> Result<HttpClient, isahc::Error> {
    HttpClient::builder()
        .redirect_policy(RedirectPolicy::Limit(super::http::MAX_REDIRECTS))
        .timeout(timeout)
        .build()
}

fn build_authorization_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
    scope: Option<&str>,
    resource: &str,
) -> String {
    let mut url = format!(
        "{authorization_endpoint}?response_type=code&client_id={}&redirect_uri={}&state={state}&code_challenge={code_challenge}&code_challenge_method=S256&resource={}",
        token::url_encode(client_id),
        token::url_encode(redirect_uri),
        token::url_encode(resource),
    );
    if let Some(s) = scope {
        url.push_str("&scope=");
        url.push_str(&token::url_encode(s));
    }
    url
}

/// First wait after a failed background OAuth attempt. Doubles per attempt.
const BACKOFF_BASE: Duration = Duration::from_secs(30);
/// Ceiling on the doubling above.
const BACKOFF_MAX: Duration = Duration::from_mins(30);

struct BackoffEntry {
    /// `None` while an attempt is in flight.
    retry_after: Option<std::time::Instant>,
    attempt: u32,
}

/// Negative cache for background OAuth attempts, one entry per server.
///
/// Lives on [`crate::mcp::McpHandle`], which every agent loop clones, so the
/// loops share one cache: a `NeedsAuth` server that fails once stops the
/// other open sessions from each retrying it. Cloning shares the same state.
#[derive(Clone, Default)]
pub struct BackgroundAuthBackoff(
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, BackoffEntry>>>,
);

impl std::fmt::Debug for BackgroundAuthBackoff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BackgroundAuthBackoff")
    }
}

impl BackgroundAuthBackoff {
    fn entries(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, BackoffEntry>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Claims the right to authenticate `server` in the background.
    ///
    /// Returns `false` when an attempt is already in flight or the server is
    /// still inside its backoff window. The check and the claim happen under
    /// one lock, so several agent loops seeing the same `NeedsAuth` server at
    /// once produce a single attempt rather than one each.
    ///
    /// Every claim must be released by [`Self::record_failure`] or
    /// [`Self::record_success`], or the server stays blocked for the process
    /// lifetime.
    pub fn try_claim(&self, server: &str) -> bool {
        let mut entries = self.entries();
        let attempt = match entries.get(server) {
            Some(entry) => match entry.retry_after {
                None => return false,
                Some(retry_after) if std::time::Instant::now() < retry_after => return false,
                Some(_) => entry.attempt,
            },
            None => 0,
        };
        entries.insert(
            server.to_owned(),
            BackoffEntry {
                retry_after: None,
                attempt,
            },
        );
        true
    }

    /// Releases the claim and starts the next backoff window.
    pub fn record_failure(&self, server: &str) {
        let mut entries = self.entries();
        let attempt = entries.get(server).map_or(0, |entry| entry.attempt) + 1;
        let delay = BACKOFF_BASE
            .saturating_mul(1_u32 << attempt.min(10))
            .min(BACKOFF_MAX);
        entries.insert(
            server.to_owned(),
            BackoffEntry {
                retry_after: Some(std::time::Instant::now() + delay),
                attempt,
            },
        );
    }

    /// Releases the claim and clears the backoff.
    pub fn record_success(&self, server: &str) {
        self.entries().remove(server);
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::BackgroundAuthBackoff;

    #[test]
    fn a_claim_excludes_every_other_caller_until_it_is_released() {
        let backoff = BackgroundAuthBackoff::default();

        assert!(backoff.try_claim("srv"), "the first caller must win");
        assert!(
            !backoff.try_claim("srv"),
            "a second loop must not launch a concurrent attempt"
        );

        backoff.record_failure("srv");
        assert!(
            !backoff.try_claim("srv"),
            "the backoff window must hold after a failure"
        );

        backoff.record_success("srv");
        assert!(
            backoff.try_claim("srv"),
            "success must clear the entry so a later NeedsAuth can retry"
        );
    }

    #[test]
    fn repeated_failures_extend_the_window() {
        let backoff = BackgroundAuthBackoff::default();
        let retry_after = |server: &str| backoff.entries().get(server).unwrap().retry_after;

        assert!(backoff.try_claim("srv"));
        backoff.record_failure("srv");
        let first = retry_after("srv");

        // A claim after the window would be denied, so drive the second
        // failure straight from the recorded state.
        backoff.record_failure("srv");
        let second = retry_after("srv");

        assert!(
            second > first,
            "a second consecutive failure must extend the backoff further than the first"
        );
    }

    #[test]
    fn each_server_is_claimed_independently() {
        let backoff = BackgroundAuthBackoff::default();
        assert!(backoff.try_claim("a"));
        assert!(backoff.try_claim("b"));
    }
}
