use std::sync::mpsc;

use crate::AgentEvent;

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
    pub fn from_response(response: ureq::http::Response<ureq::Body>) -> Self {
        let status = response.status().as_u16();
        let message = response
            .into_body()
            .read_to_string()
            .unwrap_or_else(|_| "unable to read error body".into());
        Self::Api { status, message }
    }
}

impl From<mpsc::SendError<AgentEvent>> for AgentError {
    fn from(_: mpsc::SendError<AgentEvent>) -> Self {
        Self::Channel
    }
}
