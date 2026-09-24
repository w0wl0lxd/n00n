use isahc::HttpClient;
use isahc::config::{Configurable, RedirectPolicy};
use isahc::http::Request;
use n00n_storage::auth::{OAuthTokens, now_millis};

use super::OAuthError;
use crate::mcp::response::{MAX_RESPONSE_BODY, read_bounded_text};

const TOKEN_REJECTION_BODY: &str = "token endpoint rejected request";

pub struct OAuthCodeExchange<'a> {
    pub client: &'a HttpClient,
    pub token_endpoint: &'a str,
    pub code: &'a str,
    pub redirect_uri: &'a str,
    pub code_verifier: &'a str,
    pub client_id: &'a str,
    pub client_secret: Option<&'a str>,
    pub resource: &'a str,
}

/// Exchange an authorization code for access/refresh tokens.
///
/// # Errors
///
/// Returns an error if the token request fails or the response is invalid.
pub async fn exchange_code(ctx: OAuthCodeExchange<'_>) -> Result<OAuthTokens, OAuthError> {
    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("code", ctx.code),
        ("redirect_uri", ctx.redirect_uri),
        ("code_verifier", ctx.code_verifier),
        ("client_id", ctx.client_id),
        ("resource", ctx.resource),
    ];
    if let Some(secret) = ctx.client_secret {
        params.push(("client_secret", secret));
    }
    token_request(ctx.client, ctx.token_endpoint, &params).await
}

/// Refresh OAuth tokens using a refresh token.
///
/// # Errors
///
/// Returns an error if the refresh request fails or the response is invalid.
pub async fn refresh_token(
    client: &HttpClient,
    token_endpoint: &str,
    refresh_token: &str,
    client_id: &str,
    client_secret: Option<&str>,
    resource: &str,
) -> Result<OAuthTokens, OAuthError> {
    let mut params = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
        ("resource", resource),
    ];
    if let Some(secret) = client_secret {
        params.push(("client_secret", secret));
    }
    let mut tokens = token_request(client, token_endpoint, &params).await?;
    if tokens.refresh.is_empty() {
        tokens.refresh = refresh_token.to_string();
    }
    Ok(tokens)
}

async fn token_request(
    client: &HttpClient,
    token_endpoint: &str,
    params: &[(&str, &str)],
) -> Result<OAuthTokens, OAuthError> {
    let body = params
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    let req = Request::post(token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .redirect_policy(RedirectPolicy::None)
        .body(body.into_bytes())
        .map_err(|e| OAuthError::Other(e.to_string()))?;

    let mut response = client
        .send_async(req)
        .await
        .map_err(|error| OAuthError::Network(error.to_string()))?;
    let status = response.status();
    let body = read_bounded_text(response.body_mut(), MAX_RESPONSE_BODY)
        .await
        .map_err(|error| OAuthError::InvalidResponse(error.to_string()))?;

    if !status.is_success() {
        return Err(OAuthError::ServerRejected {
            status: status.as_u16(),
            body: TOKEN_REJECTION_BODY.into(),
        });
    }

    parse_token_response(&body)
}

fn parse_token_response(body: &str) -> Result<OAuthTokens, OAuthError> {
    let resp: serde_json::Value =
        serde_json::from_str(body).map_err(|e| OAuthError::InvalidResponse(e.to_string()))?;

    let access = resp["access_token"]
        .as_str()
        .ok_or_else(|| OAuthError::InvalidResponse("missing access_token".into()))?
        .to_string();
    let refresh = resp["refresh_token"]
        .as_str()
        .map_or_else(|| "", |v| v)
        .to_string();
    let expires_in = resp["expires_in"].as_u64().unwrap_or_else(|| 3600);
    let expires = now_millis() + expires_in * 1000;

    Ok(OAuthTokens {
        access,
        refresh,
        expires,
        account_id: None,
    })
}

use std::fmt::Write;

pub(super) fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                let _ = write!(result, "%{b:02X}");
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test]
    fn url_encode_basic() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("foo=bar&baz"), "foo%3Dbar%26baz");
        assert_eq!(url_encode("abc-def_ghi.jkl~mno"), "abc-def_ghi.jkl~mno");
    }

    #[test_case(r#"{"access_token":"a1","refresh_token":"r1","expires_in":60}"#, "a1", "r1" ; "rotated_refresh")]
    #[test_case(r#"{"access_token":"a1","expires_in":60}"#, "a1", "" ; "missing_refresh_is_empty")]
    fn parse_token_response_fields(body: &str, access: &str, refresh: &str) {
        let tokens = parse_token_response(body).unwrap();
        assert_eq!(tokens.access, access);
        assert_eq!(tokens.refresh, refresh);
    }

    #[test]
    fn parse_token_response_missing_access_is_error() {
        assert!(parse_token_response(r#"{"refresh_token":"r1"}"#).is_err());
    }

    #[test]
    fn token_rejection_body_is_sanitized() {
        smol::block_on(async {
            use std::io::{Read, Write as IoWrite};
            use std::net::TcpListener;

            const SECRET_BODY: &str = r#"{"error_description":"secret-token"}"#;
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request);
                write!(
                    stream,
                    "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{SECRET_BODY}",
                    SECRET_BODY.len()
                )
                .unwrap();
            });
            let client = HttpClient::new().unwrap();

            let error = token_request(&client, &endpoint, &[]).await.unwrap_err();
            server.join().unwrap();

            assert!(matches!(
                error,
                OAuthError::ServerRejected { status: 400, ref body }
                    if body == TOKEN_REJECTION_BODY && !body.contains("secret-token")
            ));
        });
    }
}
