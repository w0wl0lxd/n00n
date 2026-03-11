use std::time::Duration;

use isahc::config::Configurable;
use serde::Deserialize;

use crate::AgentError;

pub(crate) mod anthropic;
pub(crate) mod zai;

pub use anthropic::auth;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RECV_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Deserialize)]
pub(crate) struct SseErrorPayload {
    pub error: SseErrorDetail,
}

#[derive(Deserialize)]
pub(crate) struct SseErrorDetail {
    #[serde(default)]
    pub r#type: String,
    pub message: String,
}

impl SseErrorPayload {
    pub fn into_agent_error(self) -> AgentError {
        let status = match self.error.r#type.as_str() {
            "overloaded_error" => 529,
            _ => 400,
        };
        AgentError::Api {
            status,
            message: self.error.message,
        }
    }
}

pub(crate) fn http_client() -> isahc::HttpClient {
    isahc::HttpClient::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(RECV_TIMEOUT)
        .build()
        .expect("failed to build HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("overloaded_error", "Overloaded", 529, "Overloaded" ; "overloaded_maps_to_529")]
    #[test_case("invalid_request_error", "Bad request", 400, "Bad request" ; "non_overloaded_maps_to_400")]
    fn sse_error_into_agent_error(
        error_type: &str,
        message: &str,
        expected_status: u16,
        expected_message: &str,
    ) {
        let payload = SseErrorPayload {
            error: SseErrorDetail {
                r#type: error_type.into(),
                message: message.into(),
            },
        };
        match payload.into_agent_error() {
            AgentError::Api { status, message } => {
                assert_eq!(status, expected_status);
                assert_eq!(message, expected_message);
            }
            other => panic!("expected Api error, got: {other:?}"),
        }
    }
}
