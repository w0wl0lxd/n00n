use std::sync::mpsc;

use crate::Envelope;

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
        matches!(self, Self::Api { status, .. } if *status == 429 || *status >= 500)
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
            _ => self.to_string(),
        }
    }
}

impl From<mpsc::SendError<Envelope>> for AgentError {
    fn from(_: mpsc::SendError<Envelope>) -> Self {
        Self::Channel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(429, true ; "rate_limit_is_retryable")]
    #[test_case(500, true ; "server_error_is_retryable")]
    #[test_case(529, true ; "overloaded_is_retryable")]
    #[test_case(400, false ; "bad_request_not_retryable")]
    #[test_case(0, false ; "zero_status_not_retryable")]
    fn is_retryable(status: u16, expected: bool) {
        let err = AgentError::Api {
            status,
            message: "test".into(),
        };
        assert_eq!(err.is_retryable(), expected);
    }

    #[test_case(429, "Rate limited"           ; "rate_limited")]
    #[test_case(529, "Provider is overloaded" ; "overloaded")]
    #[test_case(500, "Server error (500)"     ; "server_error")]
    #[test_case(400, "API error (400): x"     ; "non_retryable_falls_through")]
    fn retry_message_text(status: u16, expected: &str) {
        let err = AgentError::Api {
            status,
            message: "x".into(),
        };
        assert_eq!(err.retry_message(), expected);
    }
}
