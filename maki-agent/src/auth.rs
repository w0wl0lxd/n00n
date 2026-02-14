use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::debug;
use ureq::Agent;

use crate::AgentError;

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";
const AUTH_FILE: &str = "auth.json";
const REFRESH_BUFFER_SECS: u64 = 60;
const RESPONSE_TYPE: &str = "response_type=code";
const CHALLENGE_METHOD: &str = "code_challenge_method=S256";

#[derive(Debug, Serialize, Deserialize)]
struct OAuthTokens {
    access: String,
    refresh: String,
    expires: u64,
}

pub struct ResolvedAuth {
    pub api_url: String,
    pub headers: Vec<(String, String)>,
}

fn auth_file_path() -> Result<PathBuf, AgentError> {
    Ok(crate::data_dir()?.join(AUTH_FILE))
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
    let agent: Agent = Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    let resp = agent
        .post(TOKEN_URL)
        .header("content-type", "application/json")
        .send(body.to_string().as_str())
        .map_err(|e| AgentError::Api {
            status: 0,
            message: format!("{context}: {e}"),
        })?;

    if resp.status().as_u16() != 200 {
        let body_text = resp
            .into_body()
            .read_to_string()
            .unwrap_or_else(|_| "unknown error".into());
        return Err(AgentError::Api {
            status: 0,
            message: format!("{context}: {body_text}"),
        });
    }

    let body_text = resp.into_body().read_to_string()?;
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
        .ok_or_else(|| AgentError::Api {
            status: 0,
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

    let resp = post_token_request(body, "token exchange failed")?;
    into_oauth_tokens(resp, None)
}

fn refresh_tokens(tokens: &OAuthTokens) -> Result<OAuthTokens, AgentError> {
    debug!("refreshing OAuth tokens");

    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": tokens.refresh,
        "client_id": CLIENT_ID,
    });

    let resp = post_token_request(body, "token refresh failed")?;
    into_oauth_tokens(resp, Some(&tokens.refresh))
}

fn load_tokens() -> Option<OAuthTokens> {
    let path = auth_file_path().ok()?;
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_tokens(tokens: &OAuthTokens) -> Result<(), AgentError> {
    let path = auth_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(tokens)?;
    let mut file = File::create(&path)?;
    file.write_all(json.as_bytes())?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn is_expired(tokens: &OAuthTokens) -> bool {
    now_millis() + REFRESH_BUFFER_SECS * 1000 >= tokens.expires
}

pub fn resolve() -> Result<ResolvedAuth, AgentError> {
    if let Some(mut tokens) = load_tokens() {
        if is_expired(&tokens) {
            tokens = refresh_tokens(&tokens)?;
            save_tokens(&tokens)?;
        }
        debug!("using OAuth authentication");
        return Ok(ResolvedAuth {
            api_url: "https://api.anthropic.com/v1/messages?beta=true".into(),
            headers: vec![
                ("authorization".into(), format!("Bearer {}", tokens.access)),
                (
                    "anthropic-beta".into(),
                    "oauth-2025-04-20,interleaved-thinking-2025-05-14".into(),
                ),
            ],
        });
    }

    if let Ok(key) = env::var("ANTHROPIC_API_KEY") {
        debug!("using API key authentication");
        return Ok(ResolvedAuth {
            api_url: "https://api.anthropic.com/v1/messages".into(),
            headers: vec![("x-api-key".into(), key)],
        });
    }

    Err(AgentError::Api {
        status: 0,
        message: "not authenticated — run `maki auth login` or set ANTHROPIC_API_KEY".into(),
    })
}

pub fn login() -> Result<(), AgentError> {
    let (verifier, challenge) = generate_pkce();
    let url = build_authorize_url(&challenge);

    println!("Open this URL in your browser:\n\n  {url}\n");
    print!("Paste the authorization code: ");
    io::stdout().flush()?;

    let mut code = String::new();
    io::stdin().read_line(&mut code)?;
    let code = code.trim();

    if code.is_empty() {
        return Err(AgentError::Api {
            status: 0,
            message: "no authorization code provided".into(),
        });
    }

    let tokens = exchange_code(code, &verifier)?;
    save_tokens(&tokens)?;
    println!("Authenticated successfully.");
    Ok(())
}

pub fn logout() -> Result<(), AgentError> {
    let path = auth_file_path()?;
    if path.exists() {
        fs::remove_file(path)?;
        println!("Logged out.");
    } else {
        println!("Not currently logged in.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let (verifier, challenge) = generate_pkce();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
    }

    #[test]
    fn authorize_url_contains_required_params() {
        let (_, challenge) = generate_pkce();
        let url = build_authorize_url(&challenge);
        assert!(url.starts_with(AUTHORIZE_URL));
        assert!(url.contains(&format!("client_id={CLIENT_ID}")));
        assert!(url.contains(RESPONSE_TYPE));
        assert!(url.contains(CHALLENGE_METHOD));
        assert!(url.contains(&format!("code_challenge={challenge}")));
    }

    #[test]
    fn urlenc_encodes_special_characters() {
        assert_eq!(urlenc("a b"), "a%20b");
        assert_eq!(urlenc("a:b"), "a%3Ab");
        assert_eq!(urlenc("abc"), "abc");
    }

    #[test]
    fn is_expired_checks_against_current_time() {
        let expired = OAuthTokens {
            access: "a".into(),
            refresh: "r".into(),
            expires: 0,
        };
        assert!(is_expired(&expired));

        let valid = OAuthTokens {
            access: "a".into(),
            refresh: "r".into(),
            expires: now_millis() + 3_600_000,
        };
        assert!(!is_expired(&valid));
    }
}
