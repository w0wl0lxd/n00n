#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("tool error in {tool}: {message}")]
    Tool { tool: String, message: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("http: {0}")]
    Http(#[from] isahc::Error),
    #[error("http request: {0}")]
    HttpRequest(#[from] isahc::http::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("channel send failed")]
    Channel,
    #[error("cancelled")]
    Cancelled,
}

impl AgentError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Api { status, .. } => *status == 429 || *status >= 500,
            Self::Io(_) => true,
            Self::Http(_) => true,
            Self::Tool { .. }
            | Self::Channel
            | Self::Json(_)
            | Self::Cancelled
            | Self::HttpRequest(_) => false,
        }
    }

    pub async fn from_response(mut response: isahc::Response<isahc::AsyncBody>) -> Self {
        use isahc::AsyncReadResponseExt;
        let status = response.status().as_u16();
        let message = response
            .text()
            .await
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

impl<T> From<flume::SendError<T>> for AgentError {
    fn from(_: flume::SendError<T>) -> Self {
        Self::Channel
    }
}

impl From<maki_storage::StorageError> for AgentError {
    fn from(e: maki_storage::StorageError) -> Self {
        match e {
            maki_storage::StorageError::Io(io) => Self::Io(io),
            maki_storage::StorageError::Json(j) => Self::Json(j),
            other => Self::Api {
                status: 0,
                message: other.to_string(),
            },
        }
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
}
