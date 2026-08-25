use isahc::HttpClient;
use isahc::config::{Configurable, RedirectPolicy};
use isahc::http::Request;
use serde_json::json;

use super::OAuthError;
use crate::mcp::response::{MAX_RESPONSE_BODY, read_bounded_text};

pub struct ClientRegistration {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub client_secret_expires_at: Option<u64>,
}

/// Register this client with the authorization server.
///
/// # Errors
///
/// Returns an error if the registration request fails or the response is invalid.
pub async fn register_client(
    client: &HttpClient,
    registration_endpoint: &str,
    redirect_uri: &str,
) -> Result<ClientRegistration, OAuthError> {
    let body = json!({
        "client_name": "n00n",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });

    let req = Request::post(registration_endpoint)
        .header("Content-Type", "application/json")
        .redirect_policy(RedirectPolicy::None)
        .body(serde_json::to_vec(&body).map_err(|e| OAuthError::Other(e.to_string()))?)
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
            body,
        });
    }

    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| OAuthError::InvalidResponse(e.to_string()))?;

    let client_id = resp["client_id"]
        .as_str()
        .ok_or_else(|| {
            OAuthError::InvalidResponse("missing client_id in registration response".into())
        })?
        .to_string();
    let client_secret = resp["client_secret"].as_str().map(String::from);
    let client_secret_expires_at = resp["client_secret_expires_at"].as_u64();

    Ok(ClientRegistration {
        client_id,
        client_secret,
        client_secret_expires_at,
    })
}
