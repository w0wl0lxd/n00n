use std::{
    future::Future,
    net::{IpAddr, ToSocketAddrs},
    pin::Pin,
    time::Duration,
};

use futures_lite::io::{AsyncRead, AsyncReadExt};
use isahc::{
    AsyncBody, HttpClient, Request,
    config::{Configurable, RedirectPolicy, ResolveMap},
};

use crate::{
    error::SearchError,
    url_policy::{UrlPolicy, ValidatedUrl},
};

pub const DEFAULT_MAX_REDIRECTS: usize = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = "n00n-search/1";

pub type ResponseBody = Pin<Box<dyn AsyncRead + Send>>;
pub type TransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<TransportResponse, SearchError>> + Send + 'a>>;

#[derive(Clone, Debug)]
pub struct TransportRequest {
    pub url: ValidatedUrl,
    pub max_response_bytes: usize,
}

pub struct TransportResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: ResponseBody,
}

pub trait Transport: Send + Sync {
    /// Implementations must resolve once, call `ValidatedUrl::validate_resolved_ip`,
    /// pin an accepted address for the connection, and disable automatic redirects.
    fn send(&self, request: TransportRequest) -> TransportFuture<'_>;
}

/// HTTP transport that validates DNS answers and pins them into libcurl.
///
/// Redirects and environment proxies are disabled so every network target is
/// validated by [`Fetcher`] and connected without a second DNS lookup.
#[derive(Clone, Debug, Default)]
pub struct HttpTransport;

impl Transport for HttpTransport {
    fn send(&self, request: TransportRequest) -> TransportFuture<'_> {
        Box::pin(async move {
            let url = request.url.as_url().clone();
            let resolve_map = resolve_and_validate(&request.url).await?;
            let mut builder = HttpClient::builder()
                .redirect_policy(RedirectPolicy::None)
                .proxy(None::<isahc::http::Uri>)
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(REQUEST_TIMEOUT);
            if let Some(resolve_map) = resolve_map {
                builder = builder.dns_resolve(resolve_map);
            }
            let client = builder.build().map_err(transport_error)?;
            let http_request = Request::get(url.as_str())
                .header("user-agent", USER_AGENT)
                .body(AsyncBody::empty())
                .map_err(|error| SearchError::Transport {
                    message: error.to_string(),
                })?;
            let response = client
                .send_async(http_request)
                .await
                .map_err(transport_error)?;
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    value
                        .to_str()
                        .map(|value| (name.as_str().to_owned(), value.to_owned()))
                        .map_err(|error| SearchError::Parse {
                            message: format!("invalid HTTP header value: {error}"),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TransportResponse {
                status,
                headers,
                body: Box::pin(response.into_body()),
            })
        })
    }
}

async fn resolve_and_validate(url: &ValidatedUrl) -> Result<Option<ResolveMap>, SearchError> {
    let Some(host) = url.as_url().host_str() else {
        return Err(SearchError::validation("url", "host is required"));
    };
    if host.parse::<IpAddr>().is_ok() {
        return Ok(None);
    }
    let port = url
        .as_url()
        .port_or_known_default()
        .ok_or_else(|| SearchError::validation("url", "port is required"))?;
    let host_for_resolution = host.to_owned();
    let addresses = smol::unblock(move || {
        (host_for_resolution.as_str(), port)
            .to_socket_addrs()
            .map(|addresses| addresses.map(|address| address.ip()).collect::<Vec<_>>())
    })
    .await
    .map_err(|error| SearchError::Transport {
        message: format!("DNS resolution failed: {error}"),
    })?;
    if addresses.is_empty() {
        return Err(SearchError::Transport {
            message: "DNS resolution returned no addresses".to_owned(),
        });
    }
    let mut resolve_map = ResolveMap::new();
    for address in addresses {
        url.validate_resolved_ip(address)?;
        resolve_map = resolve_map.add(host, port, address);
    }
    Ok(Some(resolve_map))
}

fn transport_error(error: isahc::Error) -> SearchError {
    if error.is_timeout() {
        SearchError::Timeout
    } else {
        SearchError::Transport {
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchLimits {
    pub max_response_bytes: usize,
    pub max_redirects: usize,
}

impl FetchLimits {
    /// Checks that byte and redirect limits stay within secure bounds.
    ///
    /// # Errors
    /// Returns a validation error for a zero byte limit or excessive redirects.
    pub fn validate(&self) -> Result<(), SearchError> {
        if self.max_response_bytes == 0 {
            return Err(SearchError::validation(
                "max_response_bytes",
                "must be greater than zero",
            ));
        }
        if self.max_redirects > DEFAULT_MAX_REDIRECTS {
            return Err(SearchError::validation(
                "max_redirects",
                "exceeds the secure redirect limit",
            ));
        }
        Ok(())
    }
}

pub struct Fetcher<T> {
    transport: T,
    policy: UrlPolicy,
    limits: FetchLimits,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchResponse {
    pub final_url: String,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

impl<T: Transport> Fetcher<T> {
    /// Builds a fetcher with an injected transport and URL policy.
    ///
    /// # Errors
    /// Returns a validation error when the limits are invalid.
    pub fn new(transport: T, policy: UrlPolicy, limits: FetchLimits) -> Result<Self, SearchError> {
        limits.validate()?;
        Ok(Self {
            transport,
            policy,
            limits,
        })
    }

    /// Fetches one URL with manual redirect validation and a streaming byte bound.
    ///
    /// # Errors
    /// Returns policy, transport, HTTP, parsing, or response-size errors.
    pub async fn fetch(&self, input: &str) -> Result<FetchResponse, SearchError> {
        self.fetch_bounded(input, self.limits.max_response_bytes)
            .await
    }

    /// Fetches one URL under a caller-selected bound no larger than the configured maximum.
    ///
    /// # Errors
    /// Returns validation, policy, transport, HTTP, parsing, or response-size errors.
    pub async fn fetch_bounded(
        &self,
        input: &str,
        max_response_bytes: usize,
    ) -> Result<FetchResponse, SearchError> {
        if max_response_bytes == 0 || max_response_bytes > self.limits.max_response_bytes {
            return Err(SearchError::validation(
                "max_response_bytes",
                "is outside the fetcher's configured range",
            ));
        }
        let mut url = self.policy.validate(input)?;
        for redirect_count in 0..=self.limits.max_redirects {
            let request = TransportRequest {
                url: url.clone(),
                max_response_bytes,
            };
            let response = self.transport.send(request).await?;
            if is_redirect(response.status) {
                if redirect_count == self.limits.max_redirects {
                    return Err(SearchError::PolicyDenied {
                        reason: "redirect limit exceeded",
                    });
                }
                let location =
                    header(&response.headers, "location").ok_or_else(|| SearchError::Parse {
                        message: "redirect response is missing Location".into(),
                    })?;
                let next = url
                    .as_url()
                    .join(location)
                    .map_err(|error| SearchError::Parse {
                        message: error.to_string(),
                    })?;
                url = self.policy.validate(next.as_str())?;
                continue;
            }
            if !(200..300).contains(&response.status) {
                return Err(SearchError::Http {
                    status: response.status,
                });
            }
            if let Some(length) = header(&response.headers, "content-length") {
                let length = length
                    .parse::<usize>()
                    .map_err(|error| SearchError::Parse {
                        message: format!("invalid Content-Length: {error}"),
                    })?;
                if length > max_response_bytes {
                    return Err(SearchError::ResponseTooLarge {
                        limit: max_response_bytes,
                    });
                }
            }
            let content_type = header(&response.headers, "content-type").map(ToOwned::to_owned);
            let mut bounded = response
                .body
                .take((max_response_bytes as u64).saturating_add(1));
            let mut body = Vec::new();
            bounded
                .read_to_end(&mut body)
                .await
                .map_err(|error| SearchError::Transport {
                    message: error.to_string(),
                })?;
            if body.len() > max_response_bytes {
                return Err(SearchError::ResponseTooLarge {
                    limit: max_response_bytes,
                });
            }
            return Ok(FetchResponse {
                final_url: url.as_url().to_string(),
                content_type,
                body,
            });
        }
        Err(SearchError::PolicyDenied {
            reason: "redirect limit exceeded",
        })
    }
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use futures_lite::io::Cursor;

    use super::{
        FetchLimits, Fetcher, Transport, TransportFuture, TransportRequest, TransportResponse,
    };
    use crate::{error::SearchError, url_policy::UrlPolicy};

    struct FakeTransport {
        responses: Mutex<VecDeque<TransportResponse>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<TransportResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
            }
        }
    }

    impl Transport for FakeTransport {
        fn send(&self, _request: TransportRequest) -> TransportFuture<'_> {
            Box::pin(async move {
                self.responses
                    .lock()
                    .map_err(|error| SearchError::Transport {
                        message: error.to_string(),
                    })?
                    .pop_front()
                    .ok_or_else(|| SearchError::Transport {
                        message: "fake response queue is empty".into(),
                    })
            })
        }
    }

    fn response(status: u16, headers: &[(&str, &str)], body: &[u8]) -> TransportResponse {
        TransportResponse {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).into(), (*value).into()))
                .collect(),
            body: Box::pin(Cursor::new(body.to_vec())),
        }
    }

    #[test]
    fn streaming_body_over_limit_is_rejected() {
        smol::block_on(async {
            let transport = FakeTransport::new(vec![response(200, &[], b"too large")]);
            let fetcher = Fetcher::new(
                transport,
                UrlPolicy::untrusted_page(),
                FetchLimits {
                    max_response_bytes: 3,
                    max_redirects: 1,
                },
            )
            .unwrap();
            assert!(matches!(
                fetcher.fetch("https://example.com").await,
                Err(SearchError::ResponseTooLarge { limit: 3 })
            ));
        });
    }

    #[test]
    fn redirect_target_is_revalidated() {
        smol::block_on(async {
            let transport = FakeTransport::new(vec![response(
                302,
                &[("Location", "http://127.0.0.1/admin")],
                &[],
            )]);
            let fetcher = Fetcher::new(
                transport,
                UrlPolicy::untrusted_page(),
                FetchLimits {
                    max_response_bytes: 64,
                    max_redirects: 1,
                },
            )
            .unwrap();
            assert!(matches!(
                fetcher.fetch("https://example.com").await,
                Err(SearchError::PolicyDenied { .. })
            ));
        });
    }
}
