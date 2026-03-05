use std::sync::mpsc;

use crate::ProviderEvent;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("tool error in {tool}: {message}")]
    Tool { tool: String, message: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("http: {0}")]
    Http(#[from] ureq::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("channel send failed")]
    Channel,
}

impl AgentError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Api { status, .. } => *status == 429 || *status >= 500,
            Self::Io(_) => true,
            Self::Http(e) => is_transient_http(e),
            Self::Tool { .. } | Self::Channel | Self::Json(_) => false,
        }
    }

    pub fn from_response(response: ureq::http::Response<ureq::Body>) -> Self {
        let status = response.status().as_u16();
        let message = response
            .into_body()
            .read_to_string()
            .unwrap_or_else(|_| "unable to read error body".into());
        Self::Api { status, message }
    }

    pub fn retry_message(&self) -> String {
        match self {
            Self::Api { status: 429, .. } => "Rate limited".into(),
            Self::Api { status: 529, .. } => "Provider is overloaded".into(),
            Self::Api { status, .. } if *status >= 500 => format!("Server error ({status})"),
            Self::Io(_) | Self::Http(_) => "Connection error".into(),
            _ => self.to_string(),
        }
    }
}

fn is_transient_http(e: &ureq::Error) -> bool {
    !matches!(
        e,
        ureq::Error::BadUri(_)
            | ureq::Error::RequireHttpsOnly(_)
            | ureq::Error::BodyExceedsLimit(_)
            | ureq::Error::InvalidProxyUrl
    )
}

impl From<mpsc::SendError<ProviderEvent>> for AgentError {
    fn from(_: mpsc::SendError<ProviderEvent>) -> Self {
        Self::Channel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn api(status: u16) -> AgentError {
        AgentError::Api {
            status,
            message: String::new(),
        }
    }

    #[test_case(429, true  ; "rate_limit")]
    #[test_case(500, true  ; "server_error")]
    #[test_case(529, true  ; "overloaded")]
    #[test_case(400, false ; "bad_request")]
    #[test_case(401, false ; "unauthorized")]
    fn api_retryable(status: u16, expected: bool) {
        assert_eq!(api(status).is_retryable(), expected);
    }

    #[test]
    fn io_is_retryable() {
        assert!(AgentError::Io(std::io::ErrorKind::BrokenPipe.into()).is_retryable());
    }

    #[test]
    fn transient_http_is_retryable() {
        assert!(AgentError::Http(ureq::Error::ConnectionFailed).is_retryable());
    }

    #[test]
    fn permanent_http_not_retryable() {
        assert!(!AgentError::Http(ureq::Error::InvalidProxyUrl).is_retryable());
    }

    const CONNECTION: &str = "Connection error";

    #[test_case(429, "Rate limited"        ; "rate_limited")]
    #[test_case(529, "Provider is overloaded" ; "overloaded")]
    #[test_case(500, "Server error (500)"  ; "server_error")]
    fn retry_message_api(status: u16, expected: &str) {
        assert_eq!(api(status).retry_message(), expected);
    }

    #[test]
    fn retry_message_io() {
        assert_eq!(
            AgentError::Io(std::io::ErrorKind::BrokenPipe.into()).retry_message(),
            CONNECTION
        );
    }

    #[test]
    fn retry_message_http() {
        assert_eq!(
            AgentError::Http(ureq::Error::ConnectionFailed).retry_message(),
            CONNECTION
        );
    }
}
