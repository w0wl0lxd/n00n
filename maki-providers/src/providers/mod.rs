use std::time::Duration;

use isahc::config::Configurable;
use serde::Deserialize;

use crate::AgentError;

pub(crate) mod anthropic;
pub(crate) mod openai;
pub(crate) mod openai_compat;
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
            "api_error" | "server_error" => 500,
            "rate_limit_error" | "rate_limit_exceeded" | "tokens" => 429,
            "request_too_large" => 413,
            "not_found_error" => 404,
            "permission_error" => 403,
            "billing_error" | "insufficient_quota" => 402,
            "authentication_error" | "invalid_api_key" => 401,
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
