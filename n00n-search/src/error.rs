use std::time::Duration;

use thiserror::Error;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SearchError {
    #[error("invalid {field}: {message}")]
    Validation {
        field: &'static str,
        message: String,
    },
    #[error("URL denied by policy: {reason}")]
    PolicyDenied { reason: &'static str },
    #[error("authentication failed for {provider}")]
    Auth { provider: String },
    #[error("transport failed: {message}")]
    Transport { message: String },
    #[error("operation timed out")]
    Timeout,
    #[error("rate limited")]
    RateLimit { retry_after: Option<Duration> },
    #[error("HTTP request failed with status {status}")]
    Http { status: u16 },
    #[error("remote service returned a challenge")]
    Challenge,
    #[error("response could not be parsed: {message}")]
    Parse { message: String },
    #[error("capability is unsupported: {capability}")]
    UnsupportedCapability { capability: String },
    #[error("provider quota is exhausted")]
    Quota,
    #[error("response exceeds the {limit}-byte limit")]
    ResponseTooLarge { limit: usize },
    #[error("unsupported content type: {content_type}")]
    UnsupportedContentType { content_type: String },
}

impl SearchError {
    pub(crate) fn validation(field: &'static str, message: impl Into<String>) -> Self {
        Self::Validation {
            field,
            message: message.into(),
        }
    }
}
