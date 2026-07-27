use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

use crate::error::SearchError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrustClass {
    UntrustedPage,
    ConfiguredService,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ServiceScope {
    ManagedLoopback,
    ExplicitRemote,
}

#[derive(Clone, Debug)]
pub struct UrlPolicy {
    trust_class: TrustClass,
    service_origin: Option<String>,
    service_scope: Option<ServiceScope>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedUrl {
    url: Url,
    trust_class: TrustClass,
    service_scope: Option<ServiceScope>,
}

impl ValidatedUrl {
    #[must_use]
    pub fn as_url(&self) -> &Url {
        &self.url
    }

    #[must_use]
    pub fn trust_class(&self) -> &TrustClass {
        &self.trust_class
    }

    /// Applies the URL trust class to a pinned DNS result.
    ///
    /// # Errors
    /// Returns a policy denial when the address is outside the trust class.
    pub fn validate_resolved_ip(&self, address: IpAddr) -> Result<(), SearchError> {
        match self.service_scope {
            Some(ServiceScope::ManagedLoopback) if address.is_loopback() => Ok(()),
            Some(ServiceScope::ManagedLoopback) => Err(SearchError::PolicyDenied {
                reason: "managed service resolved outside loopback",
            }),
            Some(ServiceScope::ExplicitRemote) | None if !is_special_use(address) => Ok(()),
            Some(ServiceScope::ExplicitRemote) | None => Err(SearchError::PolicyDenied {
                reason: "destination resolved to a special-use address",
            }),
        }
    }
}

impl UrlPolicy {
    #[must_use]
    const fn new(trust_class: TrustClass) -> Self {
        Self {
            trust_class,
            service_origin: None,
            service_scope: None,
        }
    }

    #[must_use]
    pub const fn untrusted_page() -> Self {
        Self::new(TrustClass::UntrustedPage)
    }

    /// Allows only an exact loopback service origin.
    ///
    /// # Errors
    /// Returns a validation or policy error for a non-loopback endpoint.
    pub fn managed_service(endpoint: &str) -> Result<Self, SearchError> {
        Self::service(endpoint, ServiceScope::ManagedLoopback)
    }

    /// Allows only an exact, explicitly configured remote service origin.
    ///
    /// # Errors
    /// Returns a validation or policy error for an unsafe endpoint.
    pub fn remote_service(endpoint: &str) -> Result<Self, SearchError> {
        Self::service(endpoint, ServiceScope::ExplicitRemote)
    }

    fn service(endpoint: &str, scope: ServiceScope) -> Result<Self, SearchError> {
        let url = parse_http_url(endpoint)?;
        let address = literal_address(&url);
        match (&scope, address) {
            (ServiceScope::ManagedLoopback, Some(ip)) if ip.is_loopback() => {}
            (ServiceScope::ManagedLoopback, _) => {
                return Err(SearchError::PolicyDenied {
                    reason: "managed service endpoint must be a loopback IP literal",
                });
            }
            (ServiceScope::ExplicitRemote, Some(ip)) if is_special_use(ip) => {
                return Err(SearchError::PolicyDenied {
                    reason: "remote service endpoint uses a special-use address",
                });
            }
            (ServiceScope::ExplicitRemote, _) => {}
        }
        Ok(Self {
            trust_class: TrustClass::ConfiguredService,
            service_origin: Some(url.origin().ascii_serialization()),
            service_scope: Some(scope),
        })
    }

    /// Parses a URL and applies scheme, origin, credentials, and literal-IP policy.
    ///
    /// # Errors
    /// Returns a validation error for malformed input or a policy denial for an unsafe target.
    pub fn validate(&self, input: &str) -> Result<ValidatedUrl, SearchError> {
        let mut url = parse_http_url(input)?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(SearchError::PolicyDenied {
                reason: "URL userinfo is not allowed",
            });
        }
        url.set_fragment(None);
        if self.trust_class == TrustClass::ConfiguredService {
            let expected = self.service_origin.as_ref().ok_or_else(|| {
                SearchError::validation("service_endpoint", "configured service origin is missing")
            })?;
            if url.origin().ascii_serialization() != *expected {
                return Err(SearchError::PolicyDenied {
                    reason: "URL is outside the configured service origin",
                });
            }
        }
        if let Some(address) = literal_address(&url) {
            let validated = ValidatedUrl {
                url,
                trust_class: self.trust_class.clone(),
                service_scope: self.service_scope.clone(),
            };
            validated.validate_resolved_ip(address)?;
            return Ok(validated);
        }
        if url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
        {
            return Err(SearchError::PolicyDenied {
                reason: "localhost is not allowed",
            });
        }
        Ok(ValidatedUrl {
            url,
            trust_class: self.trust_class.clone(),
            service_scope: self.service_scope.clone(),
        })
    }
}

fn parse_http_url(input: &str) -> Result<Url, SearchError> {
    let url =
        Url::parse(input).map_err(|error| SearchError::validation("url", error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(SearchError::PolicyDenied {
            reason: "only HTTP and HTTPS URLs are allowed",
        });
    }
    if url.host().is_none() {
        return Err(SearchError::validation("url", "host is required"));
    }
    Ok(url)
}

fn literal_address(url: &Url) -> Option<IpAddr> {
    match url.host() {
        Some(Host::Ipv4(address)) => Some(IpAddr::V4(address)),
        Some(Host::Ipv6(address)) => Some(IpAddr::V6(address)),
        Some(Host::Domain(_)) | None => None,
    }
}

fn is_special_use(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_special_v4(address),
        IpAddr::V6(address) => is_special_v6(address),
    }
}

fn is_special_v4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_multicast()
        || address.is_unspecified()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 198 && (18..=19).contains(&octets[1]))
        || octets[0] >= 240
}

fn is_special_v6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x0100 && segments[1] == 0)
        || address.to_ipv4_mapped().is_some_and(is_special_v4)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use test_case::test_case;

    use super::UrlPolicy;

    #[test_case("http://127.0.0.1/private")]
    #[test_case("http://[::1]/private")]
    #[test_case("https://192.0.2.1/")]
    #[test_case("file:///etc/passwd")]
    fn untrusted_pages_reject_unsafe_urls(input: &str) {
        let policy = UrlPolicy::untrusted_page();
        assert!(policy.validate(input).is_err());
    }

    #[test]
    fn untrusted_page_rejects_special_dns_results() {
        let url = UrlPolicy::untrusted_page()
            .validate("https://example.com/")
            .unwrap();
        assert!(
            url.validate_resolved_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)))
                .is_err()
        );
    }

    #[test]
    fn untrusted_page_rejects_discard_only_ipv6_range() {
        let url = UrlPolicy::untrusted_page()
            .validate("https://example.com/")
            .unwrap();
        assert!(
            url.validate_resolved_ip(IpAddr::V6(Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 1)))
                .is_err()
        );
    }

    #[test]
    fn managed_service_accepts_only_its_exact_origin() {
        let policy = UrlPolicy::managed_service("http://127.0.0.1:3002").unwrap();
        assert!(policy.validate("http://127.0.0.1:3002/v2/scrape").is_ok());
        assert!(policy.validate("http://127.0.0.1:3003/v2/scrape").is_err());
    }
}
