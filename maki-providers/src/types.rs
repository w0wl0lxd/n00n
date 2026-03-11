use std::sync::Arc;

use maki_storage::sessions::TitleSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum::{Display, IntoStaticStr};

use crate::TokenUsage;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ImageMediaType {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/gif")]
    Gif,
    #[serde(rename = "image/webp")]
    Webp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    pub media_type: ImageMediaType,
    pub data: Arc<str>,
}

impl ImageSource {
    pub fn new(media_type: ImageMediaType, data: Arc<str>) -> Self {
        Self { media_type, data }
    }

    pub fn to_data_url(&self) -> String {
        let mime = match self.media_type {
            ImageMediaType::Png => "image/png",
            ImageMediaType::Jpeg => "image/jpeg",
            ImageMediaType::Gif => "image/gif",
            ImageMediaType::Webp => "image/webp",
        };
        format!("data:{mime};base64,{}", self.data)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    User,
    Assistant,
}

impl Role {
    fn is_user(&self) -> bool {
        matches!(self, Self::User)
    }
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
    Image {
        source: ImageSource,
    },
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
}

impl Message {
    pub fn user(text: String) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text }],
            ..Default::default()
        }
    }

    pub fn user_display(ai_text: String, display: String) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: ai_text }],
            display_text: Some(display),
        }
    }

    pub fn user_with_images(text: String, images: Vec<ImageSource>) -> Self {
        let mut content: Vec<ContentBlock> = images
            .into_iter()
            .map(|source| ContentBlock::Image { source })
            .collect();
        if !text.is_empty() {
            content.push(ContentBlock::Text { text });
        }
        Self {
            role: Role::User,
            content,
            ..Default::default()
        }
    }

    pub fn synthetic(text: String) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text }],
            display_text: Some(String::new()),
        }
    }

    pub fn user_text(&self) -> Option<&str> {
        match &self.display_text {
            Some(t) if t.is_empty() => None,
            Some(t) => Some(t),
            None => self.first_text_content(),
        }
    }

    fn first_text_content(&self) -> Option<&str> {
        self.content.iter().find_map(|b| match b {
            ContentBlock::Text { text } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
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

impl TitleSource for Message {
    fn first_user_text(&self) -> Option<&str> {
        if !self.role.is_user() {
            return None;
        }
        self.user_text()
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum ProviderEvent {
    TextDelta { text: String },
    ThinkingDelta { text: String },
    ToolUseStart { id: String, name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Display, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
}

impl StopReason {
    pub fn from_anthropic(s: &str) -> Self {
        match s {
            "end_turn" => Self::EndTurn,
            "tool_use" => Self::ToolUse,
            "max_tokens" => Self::MaxTokens,
            _ => Self::EndTurn,
        }
    }

    pub fn from_openai(s: &str) -> Self {
        match s {
            "stop" => Self::EndTurn,
            "tool_calls" => Self::ToolUse,
            "length" => Self::MaxTokens,
            _ => Self::EndTurn,
        }
    }
}

#[derive(Debug)]
pub struct StreamResponse {
    pub message: Message,
    pub usage: TokenUsage,
    pub stop_reason: Option<StopReason>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use test_case::test_case;

    #[test_case("end_turn", StopReason::EndTurn   ; "end_turn")]
    #[test_case("tool_use", StopReason::ToolUse   ; "tool_use")]
    #[test_case("max_tokens", StopReason::MaxTokens ; "max_tokens")]
    #[test_case("unknown", StopReason::EndTurn    ; "unknown_defaults_to_end_turn")]
    fn stop_reason_from_anthropic(input: &str, expected: StopReason) {
        assert_eq!(StopReason::from_anthropic(input), expected);
    }

    #[test_case("stop", StopReason::EndTurn       ; "stop_maps_to_end_turn")]
    #[test_case("tool_calls", StopReason::ToolUse ; "tool_calls_maps_to_tool_use")]
    #[test_case("length", StopReason::MaxTokens   ; "length_maps_to_max_tokens")]
    #[test_case("unknown", StopReason::EndTurn    ; "unknown_defaults_to_end_turn")]
    fn stop_reason_from_openai(input: &str, expected: StopReason) {
        assert_eq!(StopReason::from_openai(input), expected);
    }

    #[test]
    fn user_with_images_text_and_images() {
        let source = ImageSource::new(ImageMediaType::Png, Arc::from("abc123"));
        let msg = Message::user_with_images("hello".into(), vec![source]);
        assert_eq!(msg.content.len(), 2);
        assert!(matches!(&msg.content[0], ContentBlock::Image { .. }));
        assert!(matches!(&msg.content[1], ContentBlock::Text { text } if text == "hello"));
    }

    #[test]
    fn user_with_images_empty_text_only_images() {
        let source = ImageSource::new(ImageMediaType::Png, Arc::from("abc123"));
        let msg = Message::user_with_images(String::new(), vec![source]);
        assert_eq!(msg.content.len(), 1);
        assert!(matches!(&msg.content[0], ContentBlock::Image { .. }));
    }

    #[test_case(ImageMediaType::Png,  "image/png"  ; "png")]
    #[test_case(ImageMediaType::Jpeg, "image/jpeg" ; "jpeg")]
    #[test_case(ImageMediaType::Gif,  "image/gif"  ; "gif")]
    #[test_case(ImageMediaType::Webp, "image/webp" ; "webp")]
    fn image_source_data_url(media: ImageMediaType, mime: &str) {
        let source = ImageSource::new(media, Arc::from("dGVzdA=="));
        assert_eq!(source.to_data_url(), format!("data:{mime};base64,dGVzdA=="));
    }
}
