use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use futures_lite::io::AsyncReadExt;
use isahc::config::{Configurable, RedirectPolicy, ResolveMap};
use isahc::{AsyncBody, HttpClient, Request, Response};
use mlua::{Lua, Result as LuaResult, Table, Value};
use n00n_lua_macro::{lua_fn, lua_table};
use url::{Host, Url};

use crate::plugin_permissions::PluginPermissions;

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;
const DEFAULT_MAX_BYTES: usize = 5 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_RETRIES: u32 = 3;
const MAX_REDIRECTS: usize = 10;
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const CF_MITIGATED: &str = "cf-mitigated";
const CF_CHALLENGE: &str = "challenge";
const FALLBACK_USER_AGENT: &str = "n00n";

struct RequestParams {
    url: Url,
    method: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    timeout: Duration,
    max_bytes: usize,
    retries: u32,
}

struct ResponseData {
    body: String,
    status: u16,
    content_type: String,
}

/// Make an HTTP request and return the response body. Plain `http://`
/// URLs are automatically upgraded to `https://`. Requests to private
/// or metadata IP addresses are blocked for safety.
///
/// {opts} fields:
///   `method` (string) HTTP verb (default `"GET"`).
///   `headers` (table) Header name/value pairs.
///   `body` (string) Request body.
///   `timeout` (integer) Timeout in seconds, max 120 (default 30).
///   `max_bytes` (integer) Max response size in bytes (default 5 MB).
///   `retry` (integer) Retries on 5xx errors (default 3).
///
/// The response table has three fields: `body` (string), `status`
/// (integer), and `content_type` (string).
///
/// @param url string URL starting with `http://` or `https://`.
/// @param opts table? Request options (see above).
/// @return (table?, string?) Response table, or nil plus an error string.
/// @example
/// local res, err = n00n.net.request("https://httpbin.org/get")
/// if err then
///   print("failed: " .. err)
/// else
///   print(res.status, res.body)
/// end
#[lua_fn(guard = Net)]
async fn request(lua: Lua, url: String, opts: Option<Table>) -> LuaResult<(Value, Value)> {
    let params = match extract_request_params(&url, opts.as_ref()) {
        Ok(p) => p,
        Err(e) => return Ok((Value::Nil, Value::String(lua.create_string(&e)?))),
    };
    match do_request(params).await {
        Ok(resp) => {
            let tbl = lua.create_table()?;
            tbl.set("body", resp.body)?;
            tbl.set("status", resp.status)?;
            tbl.set("content_type", resp.content_type)?;
            Ok((Value::Table(tbl), Value::Nil))
        }
        Err(e) => Ok((Value::Nil, Value::String(lua.create_string(&e)?))),
    }
}

lua_table! {
    /// HTTP client for fetching web content. All traffic goes over HTTPS
    /// (plain HTTP is upgraded). Private and metadata IP addresses are
    /// blocked to prevent SSRF. Failed requests (5xx) are retried
    /// automatically.
    ///
    /// ```lua
    /// local res, err = n00n.net.request("https://example.com")
    /// if res then print(res.body) end
    /// ```
    "n00n.net" => pub(crate) fn create_net_table(perms: &PluginPermissions), DOCS [
        request(perms),
    ]
}

fn extract_request_params(url: &str, opts: Option<&Table>) -> Result<RequestParams, String> {
    let url = validate_and_upgrade_url(url)?;
    validate_destination(&url)?;

    let method = opts
        .and_then(|o| o.get::<String>("method").ok())
        .unwrap_or_else(|| "GET".to_string());

    let headers = if let Some(tbl) = opts.and_then(|o| o.get::<Table>("headers").ok()) {
        let mut h = Vec::new();
        for pair in tbl.pairs::<String, String>() {
            let (k, v) = pair.map_err(|e| format!("invalid header: {e}"))?;
            h.push((k, v));
        }
        h
    } else {
        Vec::new()
    };

    let body = opts
        .and_then(|o| o.get::<String>("body").ok())
        .map_or_else(Vec::new, std::string::String::into_bytes);

    let timeout = Duration::from_secs(
        opts.and_then(|o| o.get::<u64>("timeout").ok())
            .unwrap_or_else(|| DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS),
    );

    let max_bytes = opts
        .and_then(|o| o.get::<usize>("max_bytes").ok())
        .unwrap_or_else(|| DEFAULT_MAX_BYTES);
    if max_bytes > MAX_RESPONSE_BYTES {
        return Err(format!("max_bytes exceeds {MAX_RESPONSE_BYTES} byte limit"));
    }

    let retries = opts
        .and_then(|o| o.get::<u32>("retry").ok())
        .unwrap_or_else(|| MAX_RETRIES);

    Ok(RequestParams {
        url,
        method,
        headers,
        body,
        timeout,
        max_bytes,
        retries,
    })
}

fn build_request(
    url: &Url,
    user_agent: &str,
    method: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
) -> Result<Request<AsyncBody>, String> {
    let mut builder = Request::builder()
        .method(method)
        .uri(url.as_str())
        .header("User-Agent", user_agent);

    for (k, v) in headers {
        builder = builder.header(k.as_str(), v.as_str());
    }

    builder
        .body(AsyncBody::from(body))
        .map_err(|e| format!("request build error: {e}"))
}

async fn do_request(params: RequestParams) -> Result<ResponseData, String> {
    let is_get = params.method.eq_ignore_ascii_case("GET");
    let mut last_err = String::new();

    let mut response = 'retry: {
        for attempt in 0..=params.retries {
            let req = build_request(
                &params.url,
                USER_AGENT,
                &params.method,
                &params.headers,
                params.body.clone(),
            )?;
            match send_with_redirects(&params, USER_AGENT, req).await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let is_cf_challenge = status == 403
                        && resp
                            .headers()
                            .get(CF_MITIGATED)
                            .and_then(|v| v.to_str().ok())
                            .is_some_and(|v| v.contains(CF_CHALLENGE));

                    if is_cf_challenge && is_get {
                        let req = build_request(
                            &params.url,
                            FALLBACK_USER_AGENT,
                            &params.method,
                            &params.headers,
                            params.body.clone(),
                        )?;
                        match send_with_redirects(&params, FALLBACK_USER_AGENT, req).await {
                            Ok(resp) => break 'retry resp,
                            Err(e) => last_err = format!("request failed: {e}"),
                        }
                    } else if status >= 500 && attempt < params.retries {
                        last_err = format!("HTTP {status}");
                    } else {
                        break 'retry resp;
                    }
                }
                Err(e) => last_err = format!("request failed: {e}"),
            }
        }
        return Err(last_err);
    };

    let status = response.status().as_u16();

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_else(|| "")
        .to_string();

    if let Some(len) = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        && len > params.max_bytes
    {
        return Err(format!("response too large: {len} bytes"));
    }

    let mut bytes = Vec::new();
    response
        .body_mut()
        .take((params.max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| format!("read error: {e}"))?;

    if bytes.len() > params.max_bytes {
        return Err(format!("response too large: {} bytes", bytes.len()));
    }

    let body = String::from_utf8_lossy(&bytes).into_owned();
    Ok(ResponseData {
        body,
        status,
        content_type,
    })
}

async fn send_with_redirects(
    params: &RequestParams,
    user_agent: &str,
    initial_request: Request<AsyncBody>,
) -> Result<Response<AsyncBody>, String> {
    let mut current_url = params.url.clone();
    let mut request = initial_request;
    for redirects in 0..=MAX_REDIRECTS {
        let resolved_addresses = validate_destination(&current_url)?;
        let client = pinned_client(&current_url, &resolved_addresses, params.timeout)?;
        let response = client
            .send_async(request)
            .await
            .map_err(|error| format!("request failed: {error}"))?;
        if !response.status().is_redirection() {
            return Ok(response);
        }
        let Some(location) = response.headers().get("location") else {
            return Ok(response);
        };
        if redirects == MAX_REDIRECTS {
            return Err(format!("too many redirects (max {MAX_REDIRECTS})"));
        }
        let location = location
            .to_str()
            .map_err(|error| format!("invalid redirect location: {error}"))?;
        let redirect_url = redirect_url(&current_url, location)?;
        validate_destination(&redirect_url)?;
        let headers = if same_origin(&current_url, &redirect_url) {
            params.headers.as_slice()
        } else {
            &[]
        };
        current_url = redirect_url;
        request = build_request(
            &current_url,
            user_agent,
            &params.method,
            headers,
            params.body.clone(),
        )?;
    }
    Err(format!("too many redirects (max {MAX_REDIRECTS})"))
}

fn pinned_client(
    url: &Url,
    resolved_addresses: &[IpAddr],
    timeout: Duration,
) -> Result<HttpClient, String> {
    let mut builder = HttpClient::builder()
        .timeout(timeout)
        .redirect_policy(RedirectPolicy::None);
    if !resolved_addresses.is_empty() {
        let host = url
            .host_str()
            .ok_or_else(|| "URL host is required".to_owned())?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| "URL has no usable port".to_owned())?;
        let resolve_map = resolved_addresses
            .iter()
            .copied()
            .fold(ResolveMap::new(), |map, address| {
                map.add(host, port, address)
            });
        builder = builder.dns_resolve(resolve_map);
    }
    builder
        .build()
        .map_err(|error| format!("client error: {error}"))
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host() == right.host()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn redirect_url(current: &Url, location: &str) -> Result<Url, String> {
    let joined = current
        .join(location)
        .map_err(|error| format!("invalid redirect URL: {error}"))?;
    validate_and_upgrade_url(joined.as_str())
}

fn validate_destination(url: &Url) -> Result<Vec<IpAddr>, String> {
    if url.scheme() != "https" {
        return Err(format!("blocked URL scheme: {}", url.scheme()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL credentials are not allowed".to_owned());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL has no usable port".to_owned())?;
    match url
        .host()
        .ok_or_else(|| "URL host is required".to_owned())?
    {
        Host::Ipv4(ip) => {
            validate_ip(IpAddr::V4(ip))?;
            Ok(Vec::new())
        }
        Host::Ipv6(ip) => {
            validate_ip(IpAddr::V6(ip))?;
            Ok(Vec::new())
        }
        Host::Domain(host) => {
            let addrs = (host, port)
                .to_socket_addrs()
                .map_err(|error| format!("failed to resolve {host}: {error}"))?
                .collect::<Vec<_>>();
            if addrs.is_empty() {
                return Err(format!("failed to resolve {host}: no addresses"));
            }
            let mut resolved_addresses = Vec::with_capacity(addrs.len());
            for address in addrs {
                if is_private_ip(&address.ip()) {
                    return Err(format!(
                        "blocked: {host} resolves to private address {}",
                        address.ip()
                    ));
                }
                resolved_addresses.push(address.ip());
            }
            resolved_addresses.sort_unstable();
            resolved_addresses.dedup();
            Ok(resolved_addresses)
        }
    }
}

fn validate_ip(ip: IpAddr) -> Result<(), String> {
    if is_private_ip(&ip) {
        Err(format!("blocked: {ip} is a private/metadata address"))
    } else {
        Ok(())
    }
}

fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return true;
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(&IpAddr::V4(v4));
            }
            if let Some(v4) = v6.to_ipv4() {
                return is_private_ip(&IpAddr::V4(v4));
            }
            let bytes = v6.octets();
            if bytes[0] == 0xfe && (bytes[1] & 0xc0) == 0x80 {
                return true;
            }
            if bytes[0] & 0xfe == 0xfc {
                return true;
            }
            false
        }
    }
}

fn validate_and_upgrade_url(input: &str) -> Result<Url, String> {
    let mut url = Url::parse(input).map_err(|error| format!("invalid URL: {error}"))?;
    match url.scheme() {
        "http" => url
            .set_scheme("https")
            .map_err(|()| "failed to upgrade URL to HTTPS".to_owned())?,
        "https" => {}
        scheme => return Err(format!("URL scheme must be http or https, got: {scheme}")),
    }
    if url.host().is_none() {
        return Err("URL host is required".to_owned());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_permissions::PluginPermissions;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use test_case::test_case;

    #[test_case("https://example.com", "https://example.com/" ; "https_passthrough")]
    #[test_case("http://example.com", "https://example.com/" ; "http_upgraded_to_https")]
    fn validate_and_upgrade_url_valid(input: &str, expected: &str) {
        assert_eq!(validate_and_upgrade_url(input).unwrap().as_str(), expected);
    }

    #[test_case("ftp://example.com" ; "unsupported_scheme")]
    #[test_case("example.com" ; "bare_domain")]
    fn validate_and_upgrade_url_invalid(input: &str) {
        assert!(validate_and_upgrade_url(input).is_err());
    }

    #[test_case("https://8.8.8.8", Ok(()) ; "public_ip_allowed")]
    #[test_case("https://127.0.0.1", Err(()) ; "loopback_blocked")]
    #[test_case("https://192.168.1.1", Err(()) ; "private_blocked")]
    #[test_case("https://10.0.0.1", Err(()) ; "rfc1918_10_blocked")]
    #[test_case("https://172.16.0.1", Err(()) ; "rfc1918_172_blocked")]
    #[test_case("https://169.254.169.254", Err(()) ; "aws_metadata_blocked")]
    #[test_case("https://[::1]", Err(()) ; "ipv6_loopback_blocked")]
    #[test_case("https://[::ffff:127.0.0.1]", Err(()) ; "ipv4_mapped_loopback_blocked")]
    #[test_case("https://0.0.0.0", Err(()) ; "unspecified_blocked")]
    #[test_case("https://[::ffff:169.254.169.254]", Err(()) ; "ipv4_mapped_metadata_blocked")]
    fn validate_destination_cases(url: &str, expected: Result<(), ()>) {
        let url = validate_and_upgrade_url(url).unwrap();
        match expected {
            Ok(()) => assert!(
                validate_destination(&url).is_ok(),
                "{url} should be allowed"
            ),
            Err(()) => assert!(
                validate_destination(&url).is_err(),
                "{url} should be blocked"
            ),
        }
    }

    #[test]
    fn query_suffixed_loopback_is_blocked() {
        let url = validate_and_upgrade_url("https://127.0.0.1?next=example.com").unwrap();
        assert!(validate_destination(&url).is_err());
    }

    #[test]
    fn public_to_private_redirect_is_blocked_before_request() {
        let current = validate_and_upgrade_url("https://8.8.8.8/start").unwrap();
        let redirected = redirect_url(&current, "https://127.0.0.1/private").unwrap();
        assert!(validate_destination(&redirected).is_err());
    }

    #[test_case("https://example.com/start", "https://example.com/next", true ; "same_origin")]
    #[test_case("https://example.com/start", "https://other.example/next", false ; "different_host")]
    #[test_case("https://example.com/start", "https://example.com:8443/next", false ; "different_port")]
    fn redirect_origin_matching(current: &str, redirect: &str, expected: bool) {
        let current = Url::parse(current).unwrap();
        let redirect = Url::parse(redirect).unwrap();
        assert_eq!(same_origin(&current, &redirect), expected);
    }

    #[test_case(IpAddr::V4(Ipv4Addr::UNSPECIFIED), true ; "v4_unspecified")]
    #[test_case(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)), true ; "v4_rfc1918_class_b")]
    #[test_case(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255)), true ; "v4_rfc1918_class_b_upper")]
    #[test_case(IpAddr::V4(Ipv4Addr::new(172, 32, 0, 1)), false ; "v4_172_32_is_public")]
    #[test_case(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0a00, 0x0001)), true ; "ipv4_mapped_private")]
    #[test_case(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0808, 0x0808)), false ; "ipv4_mapped_public")]
    #[test_case(IpAddr::V6(Ipv6Addr::UNSPECIFIED), true ; "v6_unspecified")]
    #[test_case(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), true ; "v6_link_local")]
    #[test_case(IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1)), true ; "v6_unique_local_fc")]
    #[test_case(IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)), true ; "v6_unique_local_fd")]
    #[test_case(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)), false ; "v6_global_unicast")]
    fn is_private_ip_cases(ip: IpAddr, expected: bool) {
        assert_eq!(is_private_ip(&ip), expected);
    }

    #[test]
    fn build_request_get_no_opts() {
        let url = validate_and_upgrade_url("https://example.com").unwrap();
        let req = build_request(&url, "agent", "GET", &[], vec![]).unwrap();
        assert_eq!(req.method(), "GET");
        assert_eq!(req.body().len(), Some(0));
        assert_eq!(req.headers()["User-Agent"], "agent");
    }

    #[test]
    fn build_request_post_with_body_and_headers() {
        let headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        let url = validate_and_upgrade_url("https://example.com").unwrap();
        let req = build_request(&url, "agent", "POST", &headers, b"hello world".to_vec()).unwrap();
        assert_eq!(req.method(), "POST");
        assert_eq!(req.body().len(), Some(b"hello world".len() as u64));
        assert_eq!(req.headers()["Content-Type"], "application/json");
    }

    #[test]
    fn build_request_multiple_headers() {
        let headers = vec![
            ("Accept".to_string(), "text/html".to_string()),
            ("X-Custom".to_string(), "foo".to_string()),
        ];
        let url = validate_and_upgrade_url("https://example.com").unwrap();
        let req = build_request(&url, "agent", "GET", &headers, vec![]).unwrap();
        assert_eq!(req.headers()["Accept"], "text/html");
        assert_eq!(req.headers()["X-Custom"], "foo");
    }

    #[test_case(r#"net.request("https://127.0.0.1")"# ; "ssrf_blocked")]
    #[test_case(r#"net.request("ftp://x")"# ; "invalid_url")]
    fn lua_request_error_returns_nil_and_message(expr: &str) {
        let lua = Lua::new();
        let net = create_net_table(&lua, &PluginPermissions::trusted()).unwrap();
        lua.globals().set("net", net).unwrap();
        let (is_nil, has_err): (bool, bool) = lua
            .load(format!(
                "local r, err = {expr}; return r == nil, err ~= nil"
            ))
            .eval()
            .unwrap();
        assert!(is_nil);
        assert!(has_err);
    }

    #[test]
    fn extract_params_defaults_no_opts() {
        let params = extract_request_params("https://8.8.8.8", None).unwrap();
        assert_eq!(params.url.as_str(), "https://8.8.8.8/");
        assert_eq!(params.method, "GET");
        assert!(params.headers.is_empty());
        assert!(params.body.is_empty());
        assert_eq!(params.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
        assert_eq!(params.max_bytes, DEFAULT_MAX_BYTES);
        assert_eq!(params.retries, MAX_RETRIES);
    }

    #[test]
    fn extract_params_timeout_clamped_to_max() {
        let lua = Lua::new();
        let opts = lua.create_table().unwrap();
        opts.set("timeout", MAX_TIMEOUT_SECS + 100).unwrap();
        let params = extract_request_params("https://8.8.8.8", Some(&opts)).unwrap();
        assert_eq!(params.timeout, Duration::from_secs(MAX_TIMEOUT_SECS));
    }

    #[test]
    fn extract_params_rejects_oversized_response_budget() {
        let lua = Lua::new();
        let opts = lua.create_table().unwrap();
        opts.set("max_bytes", MAX_RESPONSE_BYTES + 1).unwrap();
        let Err(error) = extract_request_params("https://8.8.8.8", Some(&opts)) else {
            panic!("oversized response budget must be rejected");
        };
        assert!(error.contains("max_bytes exceeds"));
    }

    #[test]
    fn extract_params_post_with_body() {
        let lua = Lua::new();
        let opts = lua.create_table().unwrap();
        opts.set("method", "POST").unwrap();
        opts.set("body", r#"{"key":"val"}"#).unwrap();
        let params = extract_request_params("https://8.8.8.8", Some(&opts)).unwrap();
        assert_eq!(params.method, "POST");
        assert_eq!(params.body, br#"{"key":"val"}"#);
    }

    #[test]
    fn extract_params_http_upgraded_to_https() {
        let params = extract_request_params("http://8.8.8.8", None).unwrap();
        assert_eq!(params.url.as_str(), "https://8.8.8.8/");
    }

    #[test]
    fn extract_params_headers_collected() {
        let lua = Lua::new();
        let headers = lua.create_table().unwrap();
        headers.set("Authorization", "Bearer tok").unwrap();
        headers.set("Accept", "text/html").unwrap();
        let opts = lua.create_table().unwrap();
        opts.set("headers", headers).unwrap();
        let params = extract_request_params("https://8.8.8.8", Some(&opts)).unwrap();
        assert_eq!(params.headers.len(), 2);
        assert!(
            params
                .headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer tok")
        );
        assert!(
            params
                .headers
                .iter()
                .any(|(k, v)| k == "Accept" && v == "text/html")
        );
    }
}
