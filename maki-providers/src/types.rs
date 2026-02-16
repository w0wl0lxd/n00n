use serde::Serialize;
use serde_json::Value;

use crate::TokenUsage;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
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

    pub fn tool_results(results: Vec<(String, ToolDoneEvent)>) -> Self {
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

    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }

    pub fn has_tool_calls(&self) -> bool {
        self.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    }
}

#[derive(Debug, Serialize)]
pub struct ToolStartEvent {
    pub tool: &'static str,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDoneEvent {
    pub tool: &'static str,
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TextDelta {
        text: String,
    },
    ToolStart(ToolStartEvent),
    ToolDone(ToolDoneEvent),
    TurnComplete {
        message: Message,
        usage: TokenUsage,
        model: String,
    },
    ToolResultsSubmitted {
        message: Message,
    },
    Done {
        usage: TokenUsage,
        num_turns: u32,
        stop_reason: Option<String>,
    },
    Error {
        message: String,
    },
}

pub struct StreamResponse {
    pub message: Message,
    pub usage: TokenUsage,
    pub stop_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_is_error_skipped_when_false() {
        let ok = ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "ok".into(),
            is_error: false,
        };
        let ok_json: Value = serde_json::to_value(&ok).unwrap();
        assert!(ok_json.get("is_error").is_none());

        let err = ContentBlock::ToolResult {
            tool_use_id: "t2".into(),
            content: "fail".into(),
            is_error: true,
        };
        let err_json: Value = serde_json::to_value(&err).unwrap();
        assert_eq!(err_json["is_error"], true);
    }
}
