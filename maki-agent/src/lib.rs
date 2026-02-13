pub mod agent;
pub mod auth;
pub mod client;
pub mod tool;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: String) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text }],
        }
    }

    pub fn tool_results(results: Vec<(String, ToolOutput)>) -> Self {
        Self {
            role: Role::User,
            content: results
                .into_iter()
                .map(|(id, output)| ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: output.content,
                    is_error: output.is_error,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: String) -> Self {
        Self {
            content,
            is_error: false,
        }
    }

    pub fn err(content: String) -> Self {
        Self {
            content,
            is_error: true,
        }
    }
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ToolStart {
        name: String,
        input: String,
    },
    ToolDone {
        name: String,
        output: String,
    },
    Done {
        input_tokens: u32,
        output_tokens: u32,
    },
    Error(String),
}

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

impl From<mpsc::SendError<AgentEvent>> for AgentError {
    fn from(_: mpsc::SendError<AgentEvent>) -> Self {
        Self::Channel
    }
}

pub struct PendingToolCall {
    pub id: String,
    pub call: tool::ToolCall,
}

pub struct StreamResponse {
    pub message: Message,
    pub tool_calls: Vec<PendingToolCall>,
    pub input_tokens: u32,
    pub output_tokens: u32,
}
