use isahc::HttpClient;
use isahc::http::Request;
use serde::Deserialize;
use url::{Host, Url};

use super::OAuthError;
use crate::mcp::response::{MAX_RESPONSE_BODY, read_bounded_text};

#[derive(Debug)]
pub struct WwwAuthenticateInfo {
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceMetadata {
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    pub resource: String,
    pub scopes_supported: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct AuthServerMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
}

#[must_use]
pub fn parse_www_authenticate(header: &str) -> Option<WwwAuthenticateInfo> {
    if !header.contains("Bearer") {
        return None;
    }
    Some(WwwAuthenticateInfo {
        resource_metadata: extract_param(header, "resource_metadata"),
        scope: extract_param(header, "scope"),
    })
}

fn extract_param(header: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=\"");
    let start = header.find(&prefix)?;
    let rest = &header[start + prefix.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

struct UrlParts<'a> {
    scheme: &'a str,
    authority: &'a str,
    path: &'a str,
}

fn parse_url(url: &str) -> UrlParts<'_> {
    let base = url.trim_end_matches('/');
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    let after_scheme = &base[scheme_end..];
    let (authority, path) = match after_scheme.find('/') {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, ""),
    };
    UrlParts {
        scheme: &base[..scheme_end],
        authority,
        path,
    }
}

fn origin(url: &str) -> String {
    let parts = parse_url(url);
    format!("{}{}", parts.scheme, parts.authority)
}

fn well_known_url(base_url: &str, well_known: &str) -> String {
    let parts = parse_url(base_url);
    if parts.path.is_empty() || parts.path == "/" {
        format!(
            "{}{}/.well-known/{well_known}",
            parts.scheme, parts.authority
        )
    } else {
        format!(
            "{}{}/.well-known/{well_known}{}",
            parts.scheme, parts.authority, parts.path
        )
    }
}

pub(super) fn server_origin(server_url: &str) -> String {
    origin(server_url)
}

fn validate_endpoint_url(endpoint: &str) -> Result<(), OAuthError> {
    let url = Url::parse(endpoint)
        .map_err(|error| OAuthError::Other(format!("invalid endpoint URL {endpoint}: {error}")))?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(OAuthError::Other(format!(
            "endpoint URL must not contain credentials or a fragment: {endpoint}"
        )));
    }
    if url.scheme() == "https" && url.host().is_some() {
        return Ok(());
    }
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    if url.scheme() == "http" && loopback {
        return Ok(());
    }
    Err(OAuthError::Other(format!(
        "endpoint URL must use HTTPS or an exact HTTP loopback host: {endpoint}"
    )))
}

/// Validates that all required endpoints in auth server metadata use HTTPS.
///
/// # Errors
/// Returns `OAuthError` if any endpoint URL does not use HTTPS.
pub fn validate_auth_server(meta: &AuthServerMetadata) -> Result<(), OAuthError> {
    validate_endpoint_url(&meta.authorization_endpoint)?;
    validate_endpoint_url(&meta.token_endpoint)?;
    if let Some(ref ep) = meta.registration_endpoint {
        validate_endpoint_url(ep)?;
    }
    Ok(())
}

/// Discovers OAuth resource metadata from a server.
///
/// # Errors
/// Returns `OAuthError` if discovery fails or the metadata cannot be fetched.
pub async fn discover_resource_metadata(
    client: &HttpClient,
    server_url: &str,
    www_auth: Option<&WwwAuthenticateInfo>,
) -> Result<ResourceMetadata, OAuthError> {
    if let Some(info) = www_auth
        && let Some(ref url) = info.resource_metadata
        && origin(url) == origin(server_url)
        && let Ok(meta) = fetch_json::<ResourceMetadata>(client, url).await
    {
        return Ok(meta);
    }

    let url = well_known_url(server_url, "oauth-protected-resource");
    fetch_json::<ResourceMetadata>(client, &url)
        .await
        .map_err(|e| OAuthError::Other(format!("resource metadata discovery failed: {e}")))
}

/// Discovers OAuth authorization server metadata from an issuer URL.
///
/// # Errors
/// Returns `OAuthError` if discovery fails or the metadata cannot be fetched.
pub async fn discover_auth_server(
    client: &HttpClient,
    issuer_url: &str,
) -> Result<AuthServerMetadata, OAuthError> {
    let parts = parse_url(issuer_url);
    let has_path = !parts.path.is_empty() && parts.path != "/";

    let well_known_names = ["oauth-authorization-server", "openid-configuration"];
    let mut candidates: Vec<String> = well_known_names
        .iter()
        .map(|name| well_known_url(issuer_url, name))
        .collect();

    if has_path {
        for name in &well_known_names {
            candidates.push(format!(
                "{}{}/.well-known/{name}",
                parts.scheme, parts.authority
            ));
        }
    }

    let mut last_err = OAuthError::Other("no candidates".into());
    for url in &candidates {
        match fetch_json::<AuthServerMetadata>(client, url).await {
            Ok(meta) => {
                validate_auth_server(&meta)?;
                return Ok(meta);
            }
            Err(e) => last_err = e,
        }
    }
    Err(OAuthError::Other(format!(
        "auth server discovery failed: {last_err}"
    )))
}

async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &HttpClient,
    url: &str,
) -> Result<T, OAuthError> {
    let req = Request::get(url)
        .header("Accept", "application/json")
        .body(())
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

    serde_json::from_str(&body).map_err(|error| OAuthError::InvalidResponse(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(
        r#"Bearer realm="example", resource_metadata="https://rs.example.com/.well-known/oauth-protected-resource", scope="read write""#,
        Some("https://rs.example.com/.well-known/oauth-protected-resource"),
        Some("read write")
        ; "full_header"
    )]
    #[test_case(
        "Bearer realm=\"example\"",
        None,
        None
        ; "bearer_no_resource_metadata"
    )]
    #[test_case(
        "Basic realm=\"example\"",
        None,
        None
        ; "non_bearer_returns_none"
    )]
    fn parse_www_auth(header: &str, expected_url: Option<&str>, expected_scope: Option<&str>) {
        let result = parse_www_authenticate(header);
        match (result, expected_url, expected_scope) {
            (None, None, None) => {}
            (Some(info), url, scope) => {
                assert_eq!(info.resource_metadata.as_deref(), url);
                assert_eq!(info.scope.as_deref(), scope);
            }
            (None, _, _) => panic!("expected Some, got None"),
        }
    }

    #[test_case("https://example.com",          "https://example.com" ; "bare_origin")]
    #[test_case("https://example.com/",         "https://example.com" ; "trailing_slash")]
    #[test_case("https://example.com/api/v1",   "https://example.com" ; "with_path")]
    fn server_origin_extracts_origin(url: &str, expected: &str) {
        assert_eq!(server_origin(url), expected);
    }

    #[test_case(
        "https://example.com", "oauth-protected-resource",
        "https://example.com/.well-known/oauth-protected-resource"
        ; "no_path"
    )]
    #[test_case(
        "https://example.com/api/v1", "oauth-authorization-server",
        "https://example.com/.well-known/oauth-authorization-server/api/v1"
        ; "with_path"
    )]
    fn well_known_url_construction(base: &str, name: &str, expected: &str) {
        assert_eq!(well_known_url(base, name), expected);
    }

    #[test_case("https://auth.example.com/token" ; "https")]
    #[test_case("http://127.0.0.1:8080/token" ; "ipv4_loopback")]
    #[test_case("http://[::1]:8080/token" ; "ipv6_loopback")]
    #[test_case("http://localhost:8080/token" ; "localhost")]
    fn endpoint_validation_accepts_secure_origins(endpoint: &str) {
        assert!(validate_endpoint_url(endpoint).is_ok());
    }

    #[test_case("http://localhost.evil.example/token" ; "localhost_prefix")]
    #[test_case("http://127.0.0.1.evil.example/token" ; "ipv4_prefix")]
    #[test_case("https://user:pass@auth.example.com/token" ; "credentials")]
    #[test_case("https://auth.example.com/token#fragment" ; "fragment")]
    #[test_case("http://192.0.2.1/token" ; "non_loopback_http")]
    #[test_case("ftp://example.com/token" ; "ftp")]
    fn endpoint_validation_rejects_ambiguous_or_insecure_origins(endpoint: &str) {
        assert!(validate_endpoint_url(endpoint).is_err());
    }
}
