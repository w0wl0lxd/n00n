//! Provider error types with retry semantics.
//! Retryable: 429, 5xx, IO, HTTP transport. Non-retryable: other 4xx, JSON parse, config,
//! channel closed, user cancel. `user_message()` returns human-readable text for each variant.

use isahc::AsyncReadResponseExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryReplayReason {
    ContinuationUnavailable,
    ContinuationNotFound,
}

impl std::fmt::Display for HistoryReplayReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ContinuationUnavailable => {
                formatter.write_str("saved continuation is unavailable")
            }
            Self::ContinuationNotFound => formatter.write_str("saved continuation was not found"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestDeliveryPhase {
    NotSent,
    SentAwaitingAcceptance,
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestDeliveryMetadata {
    pub phase: RequestDeliveryPhase,
    pub response_id: Option<String>,
    /// Client-generated idempotency key sent with the request. When present,
    /// retrying the same request is safe because the provider can deduplicate.
    pub idempotency_key: Option<String>,
    pub close_code: Option<u16>,
    pub close_reason: Option<String>,
    pub emitted_event: bool,
}

impl RequestDeliveryMetadata {
    pub(crate) fn new(phase: RequestDeliveryPhase) -> Self {
        Self {
            phase,
            response_id: None,
            idempotency_key: None,
            close_code: None,
            close_reason: None,
            emitted_event: false,
        }
    }

    /// Whether the provider accepted the request, emitted output, or assigned a
    /// response id, all of which mean the request left the client and should not
    /// be retried on later failure.
    #[must_use]
    pub(crate) fn emitted_or_accepted(&self) -> bool {
        self.emitted_event
            || self.phase == RequestDeliveryPhase::Accepted
            || self.response_id.is_some()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("{message}")]
    Config { message: String },
    #[error("{message}")]
    SetupRequired { message: String },
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
    #[error("local storage operation failed")]
    Storage,
    #[error("channel send failed")]
    Channel,
    #[error("cancelled")]
    Cancelled,
    #[error("stream timed out after {secs}s of inactivity")]
    Timeout { secs: u64 },
    #[error("credential lock acquisition timed out after {millis}ms")]
    CredentialLockTimeout { millis: u64 },
    #[error("OpenAI Coding Plan request admission timed out after {millis}ms")]
    CodingPlanAdmissionTimeout { millis: u64 },
    #[error("OpenAI response-chain lock acquisition timed out after {millis}ms")]
    ResponseChainBusy { millis: u64 },
    #[error("OpenAI Coding Plan account scope changed before request send")]
    CodingPlanAdmissionScopeChanged,
    #[error(
        "OpenAI Coding Plan rejected the connection before the request was sent; the account may be at its concurrent request limit"
    )]
    CodingPlanAdmission {
        retry_after: Option<std::time::Duration>,
    },
    #[error("full-history replay requires explicit approval because {reason}")]
    HistoryReplayRequired { reason: HistoryReplayReason },
    #[error("request may have been accepted before the connection failed: {message}")]
    RequestSent {
        message: String,
        metadata: Option<RequestDeliveryMetadata>,
    },
}

impl AgentError {
    /// Returns true for a provider's transient `server_is_overloaded` response and
    /// any similarly worded provider-overload error, so the agent loop retries
    /// instead of giving up or consuming tool budgets on a temporary capacity fault.
    #[must_use]
    fn is_overload_message(message: &str) -> bool {
        let m = message.to_lowercase();
        m.contains("server_is_overloaded") || m.contains("our servers are currently overloaded")
    }

    #[must_use]
    pub fn is_server_overloaded(&self) -> bool {
        match self {
            Self::Api { message, .. } | Self::RequestSent { message, .. } => {
                Self::is_overload_message(message)
            }
            _ => false,
        }
    }

    #[must_use]
    pub fn is_retryable(&self) -> bool {
        if self.is_context_overflow() {
            return false;
        }
        if self.is_server_overloaded() {
            return true;
        }
        match self {
            Self::Api { status, .. } => *status == 429 || *status >= 500,
            Self::Io(_) | Self::Http(_) | Self::Timeout { .. } => true,
            Self::Config { .. }
            | Self::SetupRequired { .. }
            | Self::Tool { .. }
            | Self::Storage
            | Self::Channel
            | Self::Json(_)
            | Self::Cancelled
            | Self::HttpRequest(_)
            | Self::CredentialLockTimeout { .. }
            | Self::CodingPlanAdmissionTimeout { .. }
            | Self::ResponseChainBusy { .. }
            | Self::CodingPlanAdmissionScopeChanged
            | Self::CodingPlanAdmission { .. }
            | Self::HistoryReplayRequired { .. } => false,
            Self::RequestSent { metadata, .. } => metadata.as_ref().is_some_and(|m| {
                m.idempotency_key.is_some()
                    && m.phase == RequestDeliveryPhase::SentAwaitingAcceptance
                    && !m.emitted_event
            }),
        }
    }

    /// Converts failures that may have occurred after the provider accepted the
    /// request or emitted output into [`AgentError::RequestSent`], which is not
    /// retryable. Transport-level failures are treated as request-sent once the
    /// request has left the client. API/server errors are only suppressed when
    /// output has already been emitted or the request was accepted, preserving
    /// retryability when no output has been accepted.
    ///
    /// `server_is_overloaded` is never suppressed: it is a transient capacity
    /// signal and must be retried by the agent loop even if the request left the
    /// client, because the provider explicitly asks us to try again later.
    #[must_use]
    pub fn suppress_retry_after_send(self, metadata: Option<RequestDeliveryMetadata>) -> Self {
        if self.is_server_overloaded() {
            return self;
        }
        let emitted_or_accepted = metadata
            .as_ref()
            .is_some_and(RequestDeliveryMetadata::emitted_or_accepted);
        let sent = metadata
            .as_ref()
            .is_some_and(|m| m.phase != RequestDeliveryPhase::NotSent);

        match self {
            Self::Io(_) | Self::Http(_) | Self::Timeout { .. } if sent => Self::RequestSent {
                message: self.to_string(),
                metadata,
            },
            Self::Api { message, .. } if emitted_or_accepted => {
                Self::RequestSent { message, metadata }
            }
            _ => self,
        }
    }

    /// Returns true if the error indicates a context window overflow.
    ///
    /// Provider error formats:
    /// - Anthropic:  413 "prompt is too long"  <https://docs.anthropic.com/en/docs/errors>
    /// - `OpenAI`:     400 "maximum context length is X tokens"  <https://platform.openai.com/docs/guides/error-codes>
    /// - Gemini:     400 "input token count exceeds" / "too many tokens"  <https://ai.google.dev/gemini-api/docs/troubleshooting>
    /// - Ollama:     400 "context length exceeded"  <https://docs.ollama.com/api/errors>
    /// - llama.cpp:  400 "exceeds the available context size"  <https://github.com/ggml-org/llama.cpp/blob/master/tools/server/server-context.cpp>
    /// - Bedrock:    400 `ValidationException` "Input is too long for requested model"  <https://repost.aws/knowledge-center/bedrock-validation-exception-errors>
    /// - `DeepSeek`:   400 "maximum context length is X tokens"  <https://api-docs.deepseek.com/quick_start/pricing>
    /// - Mistral:    400 "too large for model with X maximum context length"  <https://docs.mistral.ai/resources/known-limitations>
    /// - `OpenRouter`: 400 "endpoint's maximum context length is X tokens"  <https://openrouter.ai/docs/api/reference/errors-and-debugging.mdx>
    /// - Synthetic:  400 pass-through from upstream models (OpenAI-compatible)  <https://synthetic.new>
    #[must_use]
    pub fn is_context_overflow(&self) -> bool {
        match self {
            Self::Api { status: 413, .. } => true,
            Self::Api {
                status: 400,
                message,
                ..
            } => {
                let m = message.to_lowercase();
                let is_scope = m.contains("context")
                    || m.contains("token")
                    || m.contains("prompt")
                    || m.contains("input");
                let is_overflow = m.contains("exceeds")
                    || m.contains("exceeded")
                    || m.contains("too long")
                    || m.contains("too many")
                    || m.contains("maximum");
                is_scope && is_overflow
            }
            Self::Api { .. }
            | Self::Config { .. }
            | Self::Tool { .. }
            | Self::Io(_)
            | Self::Http(_)
            | Self::HttpRequest(_)
            | Self::Json(_)
            | Self::Storage
            | Self::Channel
            | Self::Cancelled
            | Self::Timeout { .. }
            | Self::CredentialLockTimeout { .. }
            | Self::CodingPlanAdmissionTimeout { .. }
            | Self::ResponseChainBusy { .. }
            | Self::CodingPlanAdmissionScopeChanged
            | Self::CodingPlanAdmission { .. }
            | Self::HistoryReplayRequired { .. }
            | Self::RequestSent { .. }
            | Self::SetupRequired { .. } => false,
        }
    }

    #[must_use]
    pub fn is_auth_error(&self) -> bool {
        matches!(self, Self::Api { status: 401, .. })
    }

    #[must_use]
    pub fn is_setup_required(&self) -> bool {
        matches!(self, Self::SetupRequired { .. })
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    #[must_use]
    pub fn should_rotate_key(&self) -> bool {
        matches!(self, Self::Api { status, .. } if *status == 429 || *status == 401 || *status == 403)
    }

    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::Config { message } | Self::SetupRequired { message } => message.clone(),
            Self::Api { status: 429, .. } => "rate limited, try again in a moment".into(),
            Self::Api { status: 529, .. } => "provider is overloaded, try again later".into(),
            Self::Api { message, .. } if Self::is_overload_message(message) => {
                "provider is overloaded, try again later".into()
            }
            Self::Api { status, .. } if *status >= 500 => format!("server error ({status})"),
            Self::Api { status: 401, .. } => {
                "authentication failed, run `n00n auth login` or check your API key".into()
            }
            Self::Api { status, message } => format!("API error ({status}): {message}"),
            Self::Tool { tool, message } => format!("{tool}: {message}"),
            Self::Io(e) => format!("I/O error: {e}"),
            Self::Http(_) => "connection error, check your network".into(),
            Self::Timeout { .. } => "stream timed out, retrying".into(),
            Self::CredentialLockTimeout { .. } => {
                "credential store is busy, try again in a moment".into()
            }
            Self::CodingPlanAdmissionTimeout { .. } => {
                "OpenAI Coding Plan is busy for this account, try again shortly".into()
            }
            Self::ResponseChainBusy { .. } => {
                "this session is busy in another n00n process, try again shortly".into()
            }
            Self::CodingPlanAdmissionScopeChanged => {
                "OpenAI account changed before request send, retrying with the current account".into()
            }
            Self::CodingPlanAdmission { retry_after } => match retry_after {
                Some(delay) => format!(
                    "OpenAI Coding Plan is busy for this account, retrying after {}s",
                    delay.as_secs()
                ),
                None => "OpenAI Coding Plan rejected the connection before the request was sent; the account may be at its concurrent request limit".into(),
            },
            Self::HistoryReplayRequired { reason } => {
                format!("full-history replay requires explicit approval because {reason}")
            }
            Self::HttpRequest(e) => format!("request error: {e}"),
            Self::Json(_) => "received an invalid response from the API".into(),
            Self::Storage => "local storage error, try again".into(),
            Self::Channel => "internal error, try again".into(),
            Self::Cancelled => "cancelled".into(),
            Self::RequestSent { metadata, .. } => match metadata.as_ref() {
                Some(m) if m.idempotency_key.is_some() => {
                    "connection failed after the request was sent; retrying with idempotency key".into()
                }
                _ => "connection failed after the request was sent; not retrying to avoid duplicate output or charges".into(),
            },
        }
    }

    pub async fn from_response(mut response: isahc::Response<isahc::AsyncBody>) -> Self {
        let status = response.status().as_u16();
        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "unable to read error body".into());
        Self::Api { status, message }
    }

    #[must_use]
    pub fn retry_message(&self) -> String {
        match self {
            Self::Api { status: 429, .. } => "Rate limited".into(),
            Self::Api { status: 529, .. } => "Provider is overloaded".into(),
            Self::Api { status, .. } if *status >= 500 => format!("Server error ({status})"),
            _ if self.is_server_overloaded() => "Provider is overloaded".into(),
            Self::Io(_) | Self::Http(_) => "Connection error".into(),
            Self::Timeout { .. } => "Stream timed out".into(),
            Self::CredentialLockTimeout { .. } => "Credential store is busy".into(),
            Self::CodingPlanAdmissionTimeout { .. } => "OpenAI Coding Plan is busy".into(),
            Self::ResponseChainBusy { .. } => "OpenAI session is busy".into(),
            Self::CodingPlanAdmissionScopeChanged => "OpenAI account changed".into(),
            Self::CodingPlanAdmission { .. } => "OpenAI Coding Plan admission rejected".into(),
            _ => self.to_string(),
        }
    }
}

impl<T> From<flume::SendError<T>> for AgentError {
    fn from(_: flume::SendError<T>) -> Self {
        Self::Channel
    }
}

impl From<n00n_storage::StorageError> for AgentError {
    fn from(e: n00n_storage::StorageError) -> Self {
        match e {
            n00n_storage::StorageError::Io(io) => Self::Io(io),
            n00n_storage::StorageError::Json(j) => Self::Json(j),
            n00n_storage::StorageError::HomeNotSet
            | n00n_storage::StorageError::NotFound(_)
            | n00n_storage::StorageError::SlugCollision
            | n00n_storage::StorageError::Toon(_)
            | n00n_storage::StorageError::GetRandom(_) => Self::Storage,
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

    fn api_msg(status: u16, message: &str) -> AgentError {
        AgentError::Api {
            status,
            message: message.into(),
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

    #[test_case(401, true  ; "unauthorized")]
    #[test_case(403, false ; "forbidden")]
    fn api_auth_error(status: u16, expected: bool) {
        assert_eq!(api(status).is_auth_error(), expected);
    }

    #[test_case(429, "Rate limited"        ; "rate_limited")]
    #[test_case(529, "Provider is overloaded" ; "overloaded")]
    #[test_case(500, "Server error (500)"  ; "server_error")]
    fn retry_message_api(status: u16, expected: &str) {
        assert_eq!(api(status).retry_message(), expected);
    }

    #[test_case(RequestDeliveryPhase::NotSent, false  ; "not_sent")]
    #[test_case(RequestDeliveryPhase::SentAwaitingAcceptance, true  ; "sent_awaiting")]
    #[test_case(RequestDeliveryPhase::Accepted, false ; "accepted")]
    fn request_sent_with_idempotency_key_is_retryable(phase: RequestDeliveryPhase, expected: bool) {
        let mut metadata = RequestDeliveryMetadata::new(phase);
        metadata.idempotency_key = Some("n00n-test".into());
        let error = AgentError::RequestSent {
            message: String::new(),
            metadata: Some(metadata),
        };
        assert_eq!(error.is_retryable(), expected);
    }

    #[test]
    fn request_sent_without_idempotency_key_is_not_retryable() {
        let mut metadata =
            RequestDeliveryMetadata::new(RequestDeliveryPhase::SentAwaitingAcceptance);
        metadata.idempotency_key = None;
        let error = AgentError::RequestSent {
            message: String::new(),
            metadata: Some(metadata),
        };
        assert!(!error.is_retryable());
    }

    #[test]
    fn request_sent_after_emitted_event_is_not_retryable() {
        let mut metadata =
            RequestDeliveryMetadata::new(RequestDeliveryPhase::SentAwaitingAcceptance);
        metadata.idempotency_key = Some("n00n-test".into());
        metadata.emitted_event = true;
        let error = AgentError::RequestSent {
            message: String::new(),
            metadata: Some(metadata),
        };
        assert!(!error.is_retryable());
    }

    #[test_case(429, "rate limited, try again in a moment"                              ; "user_msg_429")]
    #[test_case(529, "provider is overloaded, try again later"                           ; "user_msg_529")]
    #[test_case(500, "server error (500)"                                                 ; "user_msg_500")]
    #[test_case(401, "authentication failed, run `n00n auth login` or check your API key" ; "user_msg_401")]
    #[test_case(400, "API error (400): bad input"                                         ; "user_msg_400")]
    fn user_message_api(status: u16, expected: &str) {
        let err = AgentError::Api {
            status,
            message: "bad input".into(),
        };
        assert_eq!(err.user_message(), expected);
    }

    #[test]
    fn timeout_is_retryable() {
        assert!(AgentError::Timeout { secs: 30 }.is_retryable());
    }

    #[test]
    fn missing_local_storage_item_is_not_an_api_error_or_path_leak() {
        let private_ref = "/home/user/.local/share/n00n/sessions/Cd-private";
        let error = AgentError::from(n00n_storage::StorageError::NotFound(private_ref.into()));

        assert!(!matches!(error, AgentError::Api { .. }));
        assert!(!error.to_string().contains(private_ref));
        assert!(!error.user_message().contains(private_ref));
    }

    // llama.cpp: https://github.com/ggml-org/llama.cpp/blob/master/tools/server/server-context.cpp
    #[test_case(400, "request (268914 tokens) exceeds the available context size (262144 tokens)", true   ; "llama_cpp_overshoot")]
    // OpenAI: https://platform.openai.com/docs/guides/error-codes
    #[test_case(400, "Input exceeds context limit", true                                                 ; "openai_style")]
    // OpenAI: https://platform.openai.com/docs/guides/error-codes
    #[test_case(400, "This model's maximum context length is 8192 tokens. However, you requested 9850 tokens", true ; "openai_max_context")]
    // Gemini: https://ai.google.dev/gemini-api/docs/troubleshooting
    #[test_case(400, "The input token count exceeds the maximum number of tokens allowed", true           ; "gemini_exceeds")]
    // Gemini: https://ai.google.dev/gemini-api/docs/troubleshooting
    #[test_case(400, "Request contains too many tokens. Please reduce the input size.", true              ; "gemini_too_many")]
    // Gemini: https://ai.google.dev/gemini-api/docs/troubleshooting
    #[test_case(400, "Your input context is too long.", true                                              ; "gemini_500_input")]
    // Ollama: https://docs.ollama.com/api/errors
    #[test_case(400, "context length exceeded", true                                                      ; "ollama")]
    // Anthropic: https://docs.anthropic.com/en/docs/errors
    #[test_case(413, "prompt is too long", true                                                           ; "anthropic_413")]
    // HTTP 413: https://www.rfc-editor.org/rfc/rfc9110.html#name-413-content-too-large
    #[test_case(413, "Payload too large", true                                                            ; "generic_413")]
    // DeepSeek: https://api-docs.deepseek.com/quick_start/pricing
    #[test_case(400, "This model's maximum context length is 131072 tokens. However, you requested 168754 tokens", true ; "deepseek")]
    // Mistral: https://docs.mistral.ai/resources/known-limitations
    #[test_case(400, "Prompt contains 321774 tokens and 0 draft tokens, too large for model with 262144 maximum context length", true ; "mistral")]
    // OpenRouter: https://openrouter.ai/docs/api/reference/errors-and-debugging.mdx
    #[test_case(400, "This endpoint's maximum context length is 200000 tokens. However, you requested about 5028244 tokens", true ; "openrouter")]
    // Bedrock: https://repost.aws/knowledge-center/bedrock-validation-exception-errors
    #[test_case(400, "Input is too long for requested model.", true                                                          ; "bedrock")]
    #[test_case(400, "Input is too long for the model", true                                              ; "too_long_input")]
    #[test_case(400, "Rate limit exceeded", false                                                         ; "not_context")]
    #[test_case(400, "Invalid API key", false                                                             ; "auth_error")]
    #[test_case(500, "Internal server error", false                                                       ; "server_error")]
    #[test_case(400, "The output is too long", false                                                      ; "output_not_context")]
    fn is_context_overflow(status: u16, message: &str, expected: bool) {
        assert_eq!(api_msg(status, message).is_context_overflow(), expected);
    }

    #[test]
    fn context_overflow_is_not_retryable() {
        let err = api_msg(400, "request exceeds the available context size");
        assert!(err.is_context_overflow());
        assert!(!err.is_retryable());
    }

    #[test]
    fn server_overloaded_400_is_retryable() {
        let err = api_msg(
            400,
            "server_is_overloaded: Our servers are currently overloaded. Please try again later.",
        );
        assert!(err.is_server_overloaded());
        assert!(err.is_retryable());
        assert_eq!(
            err.user_message(),
            "provider is overloaded, try again later"
        );
        assert_eq!(err.retry_message(), "Provider is overloaded");
    }

    #[test]
    fn request_sent_with_server_overload_is_retryable() {
        let err = AgentError::RequestSent {
            message: "request may have been accepted before the connection failed: API error (400): server_is_overloaded".into(),
            metadata: None,
        };
        assert!(err.is_server_overloaded());
        assert!(err.is_retryable());
        assert_eq!(err.retry_message(), "Provider is overloaded");
    }

    #[test]
    fn server_overloaded_is_not_suppressed_to_request_sent() {
        let err = api_msg(
            400,
            "server_is_overloaded: Our servers are currently overloaded. Please try again later.",
        );
        let metadata = Some(RequestDeliveryMetadata::new(
            RequestDeliveryPhase::SentAwaitingAcceptance,
        ));
        assert!(matches!(
            err.suppress_retry_after_send(metadata),
            AgentError::Api { status: 400, .. }
        ));
    }

    #[test]
    fn setup_required_error_is_recognized() {
        let err = AgentError::SetupRequired {
            message: "missing API key".into(),
        };
        assert!(err.is_setup_required());
        assert!(!err.is_cancelled());
        assert!(!err.is_auth_error());
    }

    #[test]
    fn unexpected_config_error_is_not_setup_required() {
        let err = AgentError::Config {
            message: "invalid provider URL".into(),
        };
        assert!(!err.is_setup_required());
    }

    #[test]
    fn cancelled_error_is_recognized() {
        let err = AgentError::Cancelled;
        assert!(err.is_cancelled());
        assert!(!err.is_setup_required());
        assert!(!err.is_auth_error());
    }
}
