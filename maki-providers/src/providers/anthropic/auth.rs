use std::env;
use std::io::{self, Write};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use maki_storage::DataDir;
use maki_storage::auth::{OAuthTokens, delete_tokens, load_tokens, now_millis, save_tokens};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::{debug, error, warn};

use isahc::ReadResponseExt;
use isahc::config::Configurable;

use crate::AgentError;
use crate::providers::{AuthKind, CONNECT_TIMEOUT, ResolvedAuth, urlenc};

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const USER_AGENT: &str = "claude-code/2.1.80";
const BETA_ADVANCED_TOOL_USE: &str = "advanced-tool-use-2025-11-20";
pub(crate) const PROVIDER: &str = "anthropic";

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

fn generate_pkce() -> (String, String) {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("failed to generate random bytes");
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn build_authorize_url(challenge: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?code=true\
        &client_id={CLIENT_ID}\
        &response_type=code\
        &redirect_uri={}\
        &scope={}\
        &code_challenge={challenge}\
        &code_challenge_method=S256\
        &state={challenge}",
        urlenc(REDIRECT_URI),
        urlenc(SCOPES),
    )
}

fn post_token_request(params: &[(&str, &str)], context: &str) -> Result<TokenResponse, AgentError> {
    let client = isahc::HttpClient::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| AgentError::Config {
            message: format!("{context}: {e}"),
        })?;

    let json_body = {
        let mut map = serde_json::Map::new();
        for (k, v) in params {
            map.insert(
                (*k).to_string(),
                serde_json::Value::String((*v).to_string()),
            );
        }
        serde_json::to_vec(&map).expect("failed to serialize token request")
    };

    let request = isahc::Request::builder()
        .method("POST")
        .uri(TOKEN_URL)
        .header("content-type", "application/json")
        .header("user-agent", "axios/6.7.1")
        .body(json_body)
        .map_err(|e| AgentError::Config {
            message: format!("{context}: {e}"),
        })?;

    let mut resp = client.send(request).map_err(|e| AgentError::Config {
        message: format!("{context}: {e}"),
    })?;

    let status = resp.status().as_u16();
    if status != 200 {
        let body_text = resp.text().unwrap_or_else(|_| "unknown error".into());
        debug!(status, body = %body_text, "{context}");
        return Err(AgentError::Config {
            message: format!("{context}: {body_text}"),
        });
    }

    let body_text = resp.text()?;
    serde_json::from_str(&body_text).map_err(Into::into)
}

fn into_oauth_tokens(
    resp: TokenResponse,
    fallback_refresh: Option<&str>,
) -> Result<OAuthTokens, AgentError> {
    let refresh = resp
        .refresh_token
        .filter(|s| !s.is_empty())
        .or_else(|| fallback_refresh.map(String::from))
        .ok_or_else(|| AgentError::Config {
            message: "missing refresh_token in token response".into(),
        })?;

    Ok(OAuthTokens {
        access: resp.access_token,
        refresh,
        expires: now_millis() + resp.expires_in * 1000,
        account_id: None,
    })
}

fn exchange_code(code: &str, verifier: &str) -> Result<OAuthTokens, AgentError> {
    let parts: Vec<&str> = code.split('#').collect();
    let auth_code = parts[0];
    let state = parts.get(1).unwrap_or(&"");

    let params: Vec<(&str, &str)> = vec![
        ("code", auth_code),
        ("state", state),
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("code_verifier", verifier),
    ];

    let resp = post_token_request(&params, "token exchange failed").map_err(|e| {
        error!(error = %e, "OAuth token exchange failed");
        e
    })?;
    into_oauth_tokens(resp, None)
}

pub(crate) fn refresh_tokens(tokens: &OAuthTokens) -> Result<OAuthTokens, AgentError> {
    let expired = tokens.is_expired();
    debug!(expired, "refreshing OAuth tokens");

    let params: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", &tokens.refresh),
        ("client_id", CLIENT_ID),
        ("scope", SCOPES),
    ];

    let resp = post_token_request(&params, "token refresh failed").map_err(|e| {
        error!(error = %e, "OAuth token refresh failed");
        e
    })?;
    into_oauth_tokens(resp, Some(&tokens.refresh))
}

pub(crate) fn build_oauth_resolved(tokens: &OAuthTokens) -> ResolvedAuth {
    ResolvedAuth {
        base_url: Some("https://api.anthropic.com/v1/messages?beta=true".into()),
        headers: vec![
            ("authorization".into(), format!("Bearer {}", tokens.access)),
            (
                "anthropic-beta".into(),
                format!(
                    "oauth-2025-04-20,interleaved-thinking-2025-05-14,{BETA_ADVANCED_TOOL_USE}"
                ),
            ),
            ("user-agent".into(), USER_AGENT.into()),
        ],
    }
}

pub fn resolve(dir: &DataDir) -> Result<(ResolvedAuth, AuthKind), AgentError> {
    if let Some(tokens) = load_tokens(dir, PROVIDER) {
        if !tokens.is_expired() {
            debug!("using OAuth authentication");
            return Ok((build_oauth_resolved(&tokens), AuthKind::OAuth));
        }
        match refresh_tokens(&tokens) {
            Ok(fresh) => {
                save_tokens(dir, PROVIDER, &fresh)?;
                debug!("using OAuth authentication");
                return Ok((build_oauth_resolved(&fresh), AuthKind::OAuth));
            }
            Err(e) => {
                warn!(error = %e, "OAuth token refresh failed, keeping tokens on disk");
                if !tokens.is_hard_expired() {
                    debug!("access token not yet expired, using existing token");
                    return Ok((build_oauth_resolved(&tokens), AuthKind::OAuth));
                }
            }
        }
    }

    if let Ok(key) = env::var("ANTHROPIC_API_KEY") {
        debug!("using API key authentication");
        return Ok((
            ResolvedAuth {
                base_url: Some("https://api.anthropic.com/v1/messages".into()),
                headers: vec![
                    ("x-api-key".into(), key),
                    ("anthropic-beta".into(), BETA_ADVANCED_TOOL_USE.into()),
                ],
            },
            AuthKind::ApiKey,
        ));
    }

    warn!("no OAuth tokens or API key found");
    Err(AgentError::Config {
        message: "not authenticated, run `maki auth login` or set ANTHROPIC_API_KEY".into(),
    })
}

pub fn login(dir: &DataDir) -> Result<(), AgentError> {
    let (verifier, challenge) = generate_pkce();
    let url = build_authorize_url(&challenge);

    println!("Open this URL in your browser:\n\n{url}\n");
    print!("Paste the authorization code: ");
    io::stdout().flush()?;

    let mut code = String::new();
    io::stdin().read_line(&mut code)?;
    let code = code.trim();

    if code.is_empty() {
        return Err(AgentError::Config {
            message: "no authorization code provided".into(),
        });
    }

    let tokens = exchange_code(code, &verifier)?;
    save_tokens(dir, PROVIDER, &tokens)?;
    println!("Authenticated successfully.");
    Ok(())
}

pub fn logout(dir: &DataDir) -> Result<(), AgentError> {
    if delete_tokens(dir, PROVIDER)? {
        println!("Logged out.");
    } else {
        println!("Not currently logged in.");
    }
    Ok(())
}
