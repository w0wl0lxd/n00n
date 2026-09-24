use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
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
    #[error("aggregate extraction exceeds the {limit}-byte limit")]
    TotalTooLarge { limit: usize },
    #[error("unsupported content type: {content_type}")]
    UnsupportedContentType { content_type: String },
    #[error("search index error: {message}")]
    Index { message: String },
    #[error("search configuration error: {message}")]
    Config { message: String },
    #[error("search I/O error: {source}")]
    Io { source: std::io::Error },
    #[error("search index is locked by another process")]
    Locked,
    #[error("semantic search is not configured")]
    NotSupported,
    #[error("tantivy error: {source}")]
    Tantivy { source: tantivy::TantivyError },
}

impl Error {
    pub(crate) fn validation(field: &'static str, message: impl Into<String>) -> Self {
        Self::Validation {
            field,
            message: message.into(),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Io { source }
    }
}

impl From<tantivy::TantivyError> for Error {
    fn from(source: tantivy::TantivyError) -> Self {
        Self::Tantivy { source }
    }
}
