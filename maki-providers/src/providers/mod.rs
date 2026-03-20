use std::time::Duration;

use isahc::config::Configurable;
use serde::Deserialize;

use crate::AgentError;

pub(crate) mod anthropic;
pub mod dynamic;
pub(crate) mod openai;
pub mod openai_auth;
pub(crate) mod openai_compat;
pub(crate) mod zai;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RECV_TIMEOUT: Duration = Duration::from_secs(300);

pub struct ResolvedAuth {
    pub base_url: Option<String>,
    pub headers: Vec<(String, String)>,
}

pub(crate) fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("a b", "a%20b" ; "space")]
    #[test_case("a:b", "a%3Ab" ; "colon")]
    #[test_case("abc", "abc"   ; "passthrough")]
    fn urlenc_encodes(input: &str, expected: &str) {
        assert_eq!(urlenc(input), expected);
    }
}
