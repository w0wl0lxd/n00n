use std::env;
use std::io::{self, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use maki_storage::DataDir;
use maki_storage::auth::delete_tokens;
pub(crate) use maki_storage::auth::{OAuthTokens, load_tokens, save_tokens};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::{debug, error, warn};

use isahc::ReadResponseExt;
use isahc::config::Configurable;

use crate::AgentError;
use crate::providers::CONNECT_TIMEOUT;

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";
const REFRESH_BUFFER_SECS: u64 = 60;
const BETA_ADVANCED_TOOL_USE: &str = "advanced-tool-use-2025-11-20";
const RESPONSE_TYPE: &str = "response_type=code";
const CHALLENGE_METHOD: &str = "code_challenge_method=S256";

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

pub struct ResolvedAuth {
    pub api_url: String,
    pub headers: Vec<(String, String)>,
}

pub enum AuthKind {
    OAuth,
    ApiKey,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
        &{RESPONSE_TYPE}\
        &redirect_uri={}\
        &scope={}\
        &code_challenge={challenge}\
        &{CHALLENGE_METHOD}\
        &state={challenge}",
        urlenc(REDIRECT_URI),
        urlenc(SCOPES),
    )
}

fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

fn post_token_request(body: serde_json::Value, context: &str) -> Result<TokenResponse, AgentError> {
    let client = isahc::HttpClient::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| AgentError::Config {
            message: format!("{context}: {e}"),
        })?;

    let json_body = serde_json::to_vec(&body).map_err(|e| AgentError::Config {
        message: format!("{context}: {e}"),
    })?;

    let request = isahc::Request::builder()
        .method("POST")
        .uri(TOKEN_URL)
        .header("content-type", "application/json")
        .body(json_body)
        .map_err(|e| AgentError::Config {
            message: format!("{context}: {e}"),
        })?;

    let mut resp = client.send(request).map_err(|e| AgentError::Config {
        message: format!("{context}: {e}"),
    })?;

    if resp.status().as_u16() != 200 {
        let body_text = resp.text().unwrap_or_else(|_| "unknown error".into());
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
    })
}

fn exchange_code(code: &str, verifier: &str) -> Result<OAuthTokens, AgentError> {
    let parts: Vec<&str> = code.split('#').collect();
    let auth_code = parts[0];
    let state = parts.get(1).unwrap_or(&"");

    let body = serde_json::json!({
        "code": auth_code,
        "state": state,
        "grant_type": "authorization_code",
        "client_id": CLIENT_ID,
        "redirect_uri": REDIRECT_URI,
        "code_verifier": verifier,
    });

    let resp = post_token_request(body, "token exchange failed").map_err(|e| {
        error!(error = %e, "OAuth token exchange failed");
        e
    })?;
    into_oauth_tokens(resp, None)
}

pub(crate) fn refresh_tokens(tokens: &OAuthTokens) -> Result<OAuthTokens, AgentError> {
    let expired = is_expired(tokens);
    debug!(expired, "refreshing OAuth tokens");

    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": tokens.refresh,
        "client_id": CLIENT_ID,
    });

    let resp = post_token_request(body, "token refresh failed").map_err(|e| {
        error!(error = %e, "OAuth token refresh failed");
        e
    })?;
    into_oauth_tokens(resp, Some(&tokens.refresh))
}

fn is_expired(tokens: &OAuthTokens) -> bool {
    now_millis() + REFRESH_BUFFER_SECS * 1000 >= tokens.expires
}

pub(crate) fn build_oauth_resolved(tokens: &OAuthTokens) -> ResolvedAuth {
    ResolvedAuth {
        api_url: "https://api.anthropic.com/v1/messages?beta=true".into(),
        headers: vec![
            ("authorization".into(), format!("Bearer {}", tokens.access)),
            (
                "anthropic-beta".into(),
                format!(
                    "oauth-2025-04-20,interleaved-thinking-2025-05-14,{BETA_ADVANCED_TOOL_USE}"
                ),
            ),
        ],
    }
}

pub fn resolve(dir: &DataDir) -> Result<(ResolvedAuth, AuthKind), AgentError> {
    if let Some(mut tokens) = load_tokens(dir) {
        if is_expired(&tokens) {
            tokens = refresh_tokens(&tokens)?;
            save_tokens(dir, &tokens)?;
        }
        debug!("using OAuth authentication");
        return Ok((build_oauth_resolved(&tokens), AuthKind::OAuth));
    }

    if let Ok(key) = env::var("ANTHROPIC_API_KEY") {
        debug!("using API key authentication");
        return Ok((
            ResolvedAuth {
                api_url: "https://api.anthropic.com/v1/messages".into(),
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

    println!("Open this URL in your browser:\n\n  {url}\n");
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
    save_tokens(dir, &tokens)?;
    println!("Authenticated successfully.");
    Ok(())
}

pub fn logout(dir: &DataDir) -> Result<(), AgentError> {
    if delete_tokens(dir)? {
        println!("Logged out.");
    } else {
        println!("Not currently logged in.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("a b", "a%20b" ; "space")]
    #[test_case("a:b", "a%3Ab" ; "colon")]
    #[test_case("abc", "abc"   ; "passthrough")]
    fn urlenc_encodes(input: &str, expected: &str) {
        assert_eq!(urlenc(input), expected);
    }

    #[test_case(0,                              true  ; "epoch_is_expired")]
    #[test_case(now_millis() + 3_600_000,       false ; "future_is_valid")]
    fn token_expiry(expires: u64, expected: bool) {
        let tokens = OAuthTokens {
            access: "a".into(),
            refresh: "r".into(),
            expires,
        };
        assert_eq!(is_expired(&tokens), expected);
    }
}
