//! Message and content types for provider communication.
//! `Message.display_text`: `Some("")` marks a message as synthetic (sent to the API but hidden
//! from the UI). `user_text()` returns `None` for these, so system-injected messages
//! (cancel markers, compaction prompts) stay invisible without a separate type.

use std::borrow::Cow;
use std::sync::Arc;

pub use n00n_storage::sessions::Effort;
pub use n00n_storage::sessions::{BodyOverride, EffortDialectId, ThinkingFieldConfig, ToggleEntry};
use n00n_storage::sessions::{
    MIN_THINKING_BUDGET, StoredReasoningContext, StoredReasoningMode, StoredThinking, TitleSource,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use strum::{Display, IntoStaticStr};
use tracing::warn;

use crate::TokenUsage;
use crate::model::Model;

const LEGACY_IMAGE_SOURCE_TYPE: &str = "base64";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMediaType {
    Png,
    Jpeg,
    Gif,
    Webp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, Deserialize, Serialize)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ImageDetail {
    Auto,
    Low,
    High,
    Original,
}

impl ImageMediaType {
    pub const ALL: [Self; 4] = [Self::Png, Self::Jpeg, Self::Gif, Self::Webp];

    /// Single source of truth for media-type strings: serde, data URLs,
    /// wire formats, and the Lua bridge all go through here.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }

    #[must_use]
    pub fn from_mime(mime: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.mime() == mime)
    }
}

impl Serialize for ImageMediaType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.mime())
    }
}

impl<'de> Deserialize<'de> for ImageMediaType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::from_mime(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown image media type '{s}'")))
    }
}

#[derive(Debug, Clone)]
pub struct ImageSource {
    pub media_type: ImageMediaType,
    pub data: Arc<str>,
    pub detail: Option<ImageDetail>,
    pub file_id: Option<String>,
    pub url: Option<String>,
}

impl<'de> Deserialize<'de> for ImageSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let value = Value::deserialize(deserializer)?;
        let obj = value
            .as_object()
            .ok_or_else(|| Error::custom("ImageSource must be an object"))?;

        let image_type = match obj.get("type") {
            None => LEGACY_IMAGE_SOURCE_TYPE,
            Some(Value::String(image_type)) => image_type,
            Some(_) => return Err(Error::custom("ImageSource type must be a string")),
        };

        let detail = match obj.get("detail") {
            None => None,
            Some(Value::String(detail)) => Some(match detail.as_str() {
                "auto" => ImageDetail::Auto,
                "low" => ImageDetail::Low,
                "high" => ImageDetail::High,
                "original" => ImageDetail::Original,
                unknown => {
                    return Err(Error::custom(format!("unknown ImageDetail '{unknown}'")));
                }
            }),
            Some(_) => return Err(Error::custom("ImageSource detail must be a string")),
        };

        match image_type {
            "file_id" => {
                let file_id = obj
                    .get("file_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::custom("ImageSource file_id variant missing file_id"))?;
                Ok(Self {
                    media_type: ImageMediaType::Png,
                    data: Arc::from(""),
                    detail,
                    file_id: Some(file_id.to_string()),
                    url: None,
                })
            }
            "url" => {
                let url = obj
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::custom("ImageSource url variant missing url"))?;
                Ok(Self {
                    media_type: ImageMediaType::Png,
                    data: Arc::from(""),
                    detail,
                    file_id: None,
                    url: Some(url.to_string()),
                })
            }
            "base64" => {
                let media_type_str =
                    obj.get("media_type")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            Error::custom("ImageSource base64 variant missing media_type")
                        })?;
                let media_type = ImageMediaType::from_mime(media_type_str).ok_or_else(|| {
                    Error::custom(format!("unknown image media type '{media_type_str}'"))
                })?;
                let data = obj
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::custom("ImageSource base64 variant missing data"))?;
                Ok(Self {
                    media_type,
                    data: Arc::from(data),
                    detail,
                    file_id: None,
                    url: None,
                })
            }
            other => Err(Error::custom(format!("unknown ImageSource type '{other}'"))),
        }
    }
}

impl Serialize for ImageSource {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        if let Some(ref file_id) = self.file_id {
            let mut state = serializer.serialize_struct("ImageSource", 2)?;
            state.serialize_field("type", "file_id")?;
            state.serialize_field("file_id", file_id)?;
            if let Some(detail) = self.detail {
                state.serialize_field("detail", &detail)?;
            }
            return state.end();
        }
        if let Some(ref url) = self.url {
            let mut state = serializer.serialize_struct("ImageSource", 2)?;
            state.serialize_field("type", "url")?;
            state.serialize_field("url", url)?;
            if let Some(detail) = self.detail {
                state.serialize_field("detail", &detail)?;
            }
            return state.end();
        }
        let mut state = serializer.serialize_struct("ImageSource", 3)?;
        state.serialize_field("type", "base64")?;
        state.serialize_field("media_type", &self.media_type)?;
        state.serialize_field("data", &self.data)?;
        if let Some(detail) = self.detail {
            state.serialize_field("detail", &detail)?;
        }
        state.end()
    }
}

impl ImageSource {
    #[must_use]
    pub fn new(media_type: ImageMediaType, data: Arc<str>) -> Self {
        Self {
            media_type,
            data,
            detail: None,
            file_id: None,
            url: None,
        }
    }

    #[must_use]
    pub fn file_id(file_id: impl Into<String>, detail: Option<ImageDetail>) -> Self {
        Self {
            media_type: ImageMediaType::Png,
            data: Arc::from(""),
            detail,
            file_id: Some(file_id.into()),
            url: None,
        }
    }

    #[must_use]
    pub fn url(url: impl Into<String>, detail: Option<ImageDetail>) -> Self {
        Self {
            media_type: ImageMediaType::Png,
            data: Arc::from(""),
            detail,
            file_id: None,
            url: Some(url.into()),
        }
    }

    #[must_use]
    pub fn to_data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type.mime(), self.data)
    }

    #[must_use]
    pub fn to_input_image_payload(&self) -> Value {
        let mut obj = json!({
            "type": "input_image",
        });
        if let Some(ref file_id) = self.file_id {
            obj["file_id"] = json!(file_id);
        } else if let Some(ref url) = self.url {
            obj["image_url"] = json!(url);
        } else {
            obj["image_url"] = json!(self.to_data_url());
        }
        if let Some(detail) = self.detail {
            obj["detail"] = json!(detail.to_string());
        }
        obj
    }
}

pub const IMAGE_OMITTED_NOTE: &str =
    "[image omitted: the current model does not support image input]";

pub const FILE_OMITTED_NOTE: &str = "[file omitted: the current model does not support file input]";

/// Prefix prepended to errored tool-result content so downstream providers can
/// distinguish a failed tool execution from a successful one.
pub(crate) const TOOL_RESULT_ERROR_PREFIX: &str = "[ERROR] ";

/// For models without vision, image blocks become a text note instead of a
/// wire block the API would reject. History keeps the pixels, so switching
/// back to a vision-capable model restores them.
#[must_use]
pub fn adapt_images_for_model<'a>(model: &Model, messages: &'a [Message]) -> Cow<'a, [Message]> {
    let has_image = |m: &Message| {
        m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::Image { .. }))
    };
    if model.supports_vision() || !messages.iter().any(has_image) {
        return Cow::Borrowed(messages);
    }
    let adapted = messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            for block in &mut m.content {
                if matches!(block, ContentBlock::Image { .. }) {
                    *block = ContentBlock::Text {
                        text: IMAGE_OMITTED_NOTE.into(),
                    };
                }
            }
            m
        })
        .collect();
    Cow::Owned(adapted)
}

/// For models without file support, file blocks become a text note instead of a
/// wire block the API would reject. History keeps the file metadata, so switching
/// back to a file-capable model restores them.
#[must_use]
pub fn adapt_files_for_model<'a>(model: &Model, messages: &'a [Message]) -> Cow<'a, [Message]> {
    let has_file = |m: &Message| {
        m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::File { .. }))
    };
    if model.supports_files() || !messages.iter().any(has_file) {
        return Cow::Borrowed(messages);
    }
    let adapted = messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            for block in &mut m.content {
                if matches!(block, ContentBlock::File { .. }) {
                    *block = ContentBlock::Text {
                        text: FILE_OMITTED_NOTE.into(),
                    };
                }
            }
            m
        })
        .collect();
    Cow::Owned(adapted)
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

/// Whether a system block ends a reusable prompt prefix that should be cached
/// by the provider.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CacheControl {
    /// Static content that is not itself a cache boundary. It is included in the
    /// cached prefix when a later `Ephemeral` block marks the boundary.
    #[default]
    None,
    /// Mark the end of the cacheable prefix at this block.
    Ephemeral,
    /// Dynamic content that may change during the session and should never be
    /// treated as a cache boundary.
    Dynamic,
}

/// A single section of a system prompt with an optional cache boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemBlock {
    pub text: String,
    pub cache: CacheControl,
}

impl SystemBlock {
    #[must_use]
    pub fn new(text: impl Into<String>, cache: CacheControl) -> Self {
        Self {
            text: text.into(),
            cache,
        }
    }
}

/// A provider-agnostic system prompt split into cacheable blocks.
///
/// Static session context (identity, conventions, AGENTS.md/CLAUDE.md) is placed
/// first and marked as the cached prefix; dynamic content (environment, model,
/// plan) follows after the cache boundary so a change to it does not invalidate
/// the cached static prefix.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct System {
    blocks: Vec<SystemBlock>,
}

impl System {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    #[must_use]
    pub fn blocks(&self) -> &[SystemBlock] {
        &self.blocks
    }

    pub fn push(&mut self, block: SystemBlock) {
        self.blocks.push(block);
    }

    pub fn push_static(&mut self, text: impl Into<String>) {
        self.push(SystemBlock::new(text, CacheControl::None));
    }

    pub fn push_dynamic(&mut self, text: impl Into<String>) {
        if !self
            .blocks
            .iter()
            .any(|block| matches!(block.cache, CacheControl::Ephemeral | CacheControl::Dynamic))
        {
            self.seal_static_boundary();
        }
        self.push(SystemBlock::new(text, CacheControl::Dynamic));
    }

    /// Replace the text of the most recently pushed dynamic block, if any.
    ///
    /// Returns `true` when a dynamic block was updated.
    pub fn replace_last_dynamic(&mut self, text: impl Into<String>) -> bool {
        if let Some(block) = self
            .blocks
            .iter_mut()
            .rev()
            .find(|block| block.cache == CacheControl::Dynamic)
        {
            block.text = text.into();
            true
        } else {
            false
        }
    }

    /// Mark the static prefix as cacheable when no dynamic content has been
    /// added. This is a no-op after a dynamic block because caching a later
    /// static block would also cache the dynamic prefix.
    pub fn seal(&mut self) {
        if !self
            .blocks
            .iter()
            .any(|block| matches!(block.cache, CacheControl::Ephemeral | CacheControl::Dynamic))
        {
            self.seal_static_boundary();
        }
    }

    fn seal_static_boundary(&mut self) {
        if let Some(last_static) = self
            .blocks
            .iter_mut()
            .rev()
            .find(|b| b.cache == CacheControl::None)
        {
            last_static.cache = CacheControl::Ephemeral;
        }
    }
}

impl std::fmt::Display for System {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for block in &self.blocks {
            write!(f, "{}", block.text)?;
        }
        Ok(())
    }
}

impl From<&str> for System {
    fn from(text: &str) -> Self {
        let mut system = Self::new();
        if !text.is_empty() {
            system.push_static(text);
            system.seal();
        }
        system
    }
}

impl From<String> for System {
    fn from(text: String) -> Self {
        Self::from(text.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
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
    File {
        source: FileSource,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileSource {
    #[serde(default)]
    pub file_id: Option<String>,
    #[serde(default)]
    pub file_url: Option<String>,
    #[serde(default)]
    pub file_data: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub detail: Option<ImageDetail>,
}

impl FileSource {
    #[must_use]
    pub fn new() -> Self {
        Self {
            file_id: None,
            file_url: None,
            file_data: None,
            filename: None,
            detail: None,
        }
    }

    #[must_use]
    pub fn file_id(file_id: impl Into<String>, detail: Option<ImageDetail>) -> Self {
        Self {
            file_id: Some(file_id.into()),
            detail,
            ..Self::new()
        }
    }

    #[must_use]
    pub fn file_url(file_url: impl Into<String>, detail: Option<ImageDetail>) -> Self {
        Self {
            file_url: Some(file_url.into()),
            detail,
            ..Self::new()
        }
    }

    #[must_use]
    pub fn identifier(&self) -> Option<&str> {
        self.file_id
            .as_deref()
            .or_else(|| self.filename.as_deref())
            .or_else(|| self.file_url.as_deref())
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub control: bool,
}

impl Message {
    #[must_use]
    pub fn user(text: String) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text }],
            ..Default::default()
        }
    }

    #[must_use]
    pub fn user_display(ai_text: String, display: String) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: ai_text }],
            display_text: Some(display),
            control: false,
        }
    }

    #[must_use]
    pub fn control_display(ai_text: String, display: String) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: ai_text }],
            display_text: Some(display),
            control: true,
        }
    }

    #[must_use]
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

    #[must_use]
    pub fn synthetic(text: String) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text }],
            display_text: Some(String::new()),
            control: false,
        }
    }

    #[must_use]
    pub fn assistant(text: String) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text }],
            display_text: Some(String::new()),
            control: false,
        }
    }

    #[must_use]
    pub fn user_text(&self) -> Option<&str> {
        match &self.display_text {
            Some(t) if t.is_empty() => None,
            Some(t) => Some(t),
            None => self.first_text_content(),
        }
    }

    #[must_use]
    pub fn first_text_content(&self) -> Option<&str> {
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

    #[must_use]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheKind {
    ResponseChain,
    Prompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheHealth {
    pub kind: CacheKind,
    pub valid_until: u64,
    pub ttl_seconds: u64,
    pub hit: bool,
}

#[derive(Debug, Clone, Serialize)]
pub enum ProviderEvent {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolUseStart {
        id: String,
        name: String,
    },
    PromptProgress {
        processed: u32,
        total: u32,
        cache: u32,
    },
    CacheHealth {
        cache: CacheHealth,
    },
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
    #[must_use]
    pub fn from_anthropic(s: &str) -> Self {
        match s {
            "tool_use" => Self::ToolUse,
            "max_tokens" => Self::MaxTokens,
            _ => Self::EndTurn,
        }
    }

    #[must_use]
    pub fn from_openai(s: &str) -> Self {
        match s {
            "tool_calls" => Self::ToolUse,
            "length" => Self::MaxTokens,
            _ => Self::EndTurn,
        }
    }

    pub fn from_google(s: &str) -> Self {
        match s {
            "MAX_TOKENS" => Self::MaxTokens,
            "SAFETY" | "RECITATION" => {
                warn!("Gemini stop reason: {s}, treating as end_turn");
                Self::EndTurn
            }
            _ => Self::EndTurn,
        }
    }
}

const THINKING_USAGE: &str =
    "Usage: /thinking [off|adaptive|minimal|low|medium|high|xhigh|max|<budget>]";

/// Effort levels are percentages, so they need a ceiling even when the model
/// never told us its output window. 32k matches common frontier thinking
/// caps. Explicit user budgets never go through this.
const FALLBACK_MAX_THINKING_BUDGET: u32 = 32_768;

/// How a provider's effort knob speaks: which levels its API accepts, what
/// `adaptive` means there, and whether "off" needs an explicit string.
/// New providers add a const in [`dialect`]; providers with dynamic model
/// listings build one from the model's declared levels (see `OpenRouter`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortDialect<'a> {
    /// Accepted levels, non-empty and ascending (checked by test).
    pub supported: &'a [Effort],
    /// What `Adaptive` maps to. `None` means the API has its own adaptive or
    /// default behavior: send nothing and let it decide.
    pub adaptive: Option<Effort>,
    /// Explicit opt-out string, e.g. GLM `"none"`.
    pub off: Option<&'static str>,
}

pub mod dialect {
    use super::EffortDialect;
    use n00n_storage::sessions::Effort::{High, Low, Max, Medium, Minimal, XHigh};

    /// Wire string that disables reasoning, for APIs that need an explicit
    /// opt-out.
    pub const OFF: &str = "none";

    /// `OpenAI` platform, synthetic.
    pub const STANDARD: EffortDialect = EffortDialect {
        supported: &[Minimal, Low, Medium, High],
        adaptive: Some(Medium),
        off: None,
    };
    /// `OpenAI` Responses API with extended effort levels (xhigh, max).
    pub const OPENAI_EXTENDED: EffortDialect = EffortDialect {
        supported: &[Minimal, Low, Medium, High, XHigh, Max],
        adaptive: Some(Medium),
        off: None,
    };
    /// opencode chat-completions, openrouter (static fallback).
    pub const PREFER_HIGH: EffortDialect = EffortDialect {
        supported: &[Low, Medium, High],
        adaptive: Some(High),
        off: None,
    };
    /// Mistral.
    pub const HIGH_ONLY: EffortDialect = EffortDialect {
        supported: &[High],
        adaptive: Some(High),
        off: None,
    };
    /// Z.AI. GLM reasons by default, so Off sends "none" explicitly.
    /// Only use behind `Model::supports_thinking`.
    pub const GLM: EffortDialect = EffortDialect {
        supported: &[High, XHigh],
        adaptive: Some(High),
        off: Some(OFF),
    };
    /// `DeepSeek` accepts only "max"; Adaptive keeps the model's own default
    /// reasoning depth by sending no effort at all.
    pub const DEEPSEEK: EffortDialect = EffortDialect {
        supported: &[Max],
        adaptive: None,
        off: None,
    };
    /// `output_config.effort` on Anthropic adaptive-thinking models. The API
    /// has native adaptive mode, so Adaptive sends no effort.
    pub const ANTHROPIC_ADAPTIVE: EffortDialect = EffortDialect {
        supported: &[Low, Medium, High],
        adaptive: None,
        off: None,
    };
    /// `TensorX` routes models that may reason by default, so Off sends "none"
    /// explicitly and Adaptive asks for full depth.
    pub const TENSORX: EffortDialect = EffortDialect {
        supported: &[Low, Medium, High],
        adaptive: Some(High),
        off: Some(OFF),
    };
}

/// Resolve a config-level dialect id to the dialect it names. Lives here
/// because the dialect consts do, while the id is a storage type shared with
/// `n00n-config`.
#[must_use]
pub fn effort_dialect_for(id: EffortDialectId) -> &'static EffortDialect<'static> {
    match id {
        EffortDialectId::Standard => &dialect::STANDARD,
        EffortDialectId::OpenaiExtended => &dialect::OPENAI_EXTENDED,
        EffortDialectId::PreferHigh => &dialect::PREFER_HIGH,
        EffortDialectId::HighOnly => &dialect::HIGH_ONLY,
        EffortDialectId::Glm => &dialect::GLM,
        EffortDialectId::DeepSeek => &dialect::DEEPSEEK,
        EffortDialectId::AnthropicAdaptive => &dialect::ANTHROPIC_ADAPTIVE,
        EffortDialectId::TensorX => &dialect::TENSORX,
    }
}

/// Navigate a dot-separated path, replacing any non-object segment on the way
/// with a fresh object so indexing can never panic. `"reasoning.effort"`
/// returns the slot for `effort` inside `body["reasoning"]`.
fn entry_at<'a>(body: &'a mut Value, path: &str) -> &'a mut Value {
    let mut current = body;
    for segment in path.split('.') {
        if !current.is_object() {
            *current = json!({});
        }
        current = &mut current[segment];
    }
    current
}

/// Write `value` at `path`, overwriting whatever is there.
fn set_by_path(body: &mut Value, path: &str, value: Value) {
    *entry_at(body, path) = value;
}

/// Like [`set_by_path`] but shallow-merges objects at the leaf, so a toggle's
/// static fields survive a later budget or effort write to the same object.
fn merge_by_path(body: &mut Value, path: &str, value: Value) {
    let target = entry_at(body, path);
    match (target.as_object_mut(), value.as_object()) {
        (Some(existing), Some(incoming)) => {
            for (key, val) in incoming {
                existing.insert(key.clone(), val.clone());
            }
        }
        _ => *target = value,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningMode {
    Standard,
    Pro,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningContext {
    Auto,
    CurrentTurn,
    AllTurns,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThinkingExtras {
    pub reasoning_mode: Option<ReasoningMode>,
    pub reasoning_context: Option<ReasoningContext>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThinkingConfig {
    #[default]
    Off,
    Adaptive,
    Effort(Effort),
    Budget(u32),
    WithExtras(Effort, ThinkingExtras),
}

/// Resolved thinking value for token-budget APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Budgeted {
    Off,
    Adaptive,
    Tokens(u32),
}

impl ThinkingConfig {
    #[must_use]
    pub fn is_enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    #[must_use]
    pub fn effort(self) -> Option<Effort> {
        match self {
            Self::Effort(e) | Self::WithExtras(e, _) => Some(e),
            Self::Off | Self::Adaptive | Self::Budget(_) => None,
        }
    }

    #[must_use]
    pub fn extras(self) -> ThinkingExtras {
        match self {
            Self::WithExtras(_, extras) => extras,
            _ => ThinkingExtras::default(),
        }
    }

    /// The effort string to send, snapped to the dialect's supported levels
    /// here and nowhere else (never chain snaps). `None` means send nothing:
    /// `Off` without an explicit off string, or `Adaptive` on APIs with their
    /// own default behavior.
    #[must_use]
    pub fn effort_str(self, dialect: &EffortDialect, model: &Model) -> Option<&'static str> {
        let level = match self {
            Self::Off => return dialect.off,
            Self::Adaptive => dialect.adaptive?,
            Self::Effort(e) | Self::WithExtras(e, _) => e,
            Self::Budget(n) => Effort::from_budget(
                n,
                model
                    .max_thinking_budget()
                    .unwrap_or_else(|| FALLBACK_MAX_THINKING_BUDGET),
            ),
        };
        Some(level.snap(dialect.supported).as_str())
    }

    /// The token budget to send, clamped to `[MIN_THINKING_BUDGET, max]` here
    /// and nowhere else. An unknown `max` never caps: the user's number goes
    /// through as asked, and effort levels scale the fallback ceiling.
    fn budget(self, max: Option<u32>) -> Budgeted {
        match self {
            Self::Off => Budgeted::Off,
            Self::Adaptive => Budgeted::Adaptive,
            Self::Effort(e) | Self::WithExtras(e, _) => {
                Budgeted::Tokens(e.budget(max.unwrap_or_else(|| FALLBACK_MAX_THINKING_BUDGET)))
            }
            Self::Budget(n) => Budgeted::Tokens(match max {
                Some(max) => n.clamp(MIN_THINKING_BUDGET, max.max(MIN_THINKING_BUDGET)),
                None => n.max(MIN_THINKING_BUDGET),
            }),
        }
    }

    /// Anthropic messages API body. Adaptive-thinking models get the native
    /// adaptive knob plus `output_config.effort`; legacy models get a plain
    /// token budget. A model carrying `thinking_fields` (declared by a dynamic
    /// or custom provider) takes full control of the wire format instead, so
    /// the id-based version check is skipped.
    pub fn apply_to_body(self, body: &mut Value, model: &Model) {
        if let Some(fields) = model.thinking_fields.clone() {
            self.apply_thinking(body, model, &dialect::ANTHROPIC_ADAPTIVE, &fields);
            return;
        }
        if Self::requires_adaptive(&model.id) {
            match self {
                Self::Off => {}
                Self::Adaptive => body["thinking"] = json!({"type": "adaptive"}),
                Self::Effort(_) | Self::Budget(_) | Self::WithExtras(_, _) => {
                    body["thinking"] = json!({"type": "adaptive"});
                    if let Some(effort) = self.effort_str(&dialect::ANTHROPIC_ADAPTIVE, model) {
                        body["output_config"]["effort"] = json!(effort);
                    }
                }
            }
            return;
        }
        match self.budget(model.max_thinking_budget()) {
            Budgeted::Off => {}
            Budgeted::Adaptive => body["thinking"] = json!({"type": "adaptive"}),
            Budgeted::Tokens(n) => {
                body["thinking"] = json!({"type": "enabled", "budget_tokens": n});
            }
        }
    }

    /// Version check, not an allowlist, so future Opus releases work
    /// automatically. Splits on `-` and `.` since Copilot uses dotted ids
    /// (`claude-opus-4.7`).
    fn requires_adaptive(model_id: &str) -> bool {
        let Some(version) = model_id.strip_prefix("claude-opus-") else {
            return false;
        };
        let mut parts = version.split(['-', '.']);
        let (Some(Ok(major)), Some(Ok(minor))) = (
            parts.next().map(str::parse::<u32>),
            parts.next().map(str::parse::<u32>),
        ) else {
            return false;
        };
        (major, minor) >= (4, 7)
    }

    /// Serialize thinking into `body` from a declarative field layout.
    /// `model.thinking_fields` overrides `default_fields` field by field, so a
    /// script that declares only a toggle still inherits the base provider's
    /// effort and budget paths; `model.thinking_dialect` overrides
    /// `default_dialect`.
    ///
    /// Order: toggles (`off` set for Off, `adaptive`/`on` merged otherwise),
    /// then the budget (at `budget_path`, else nested under the first toggle
    /// declaring a `budget_key`), then the effort string at `effort_path`.
    pub fn apply_thinking(
        self,
        body: &mut Value,
        model: &Model,
        default_dialect: &EffortDialect,
        default_fields: &ThinkingFieldConfig,
    ) {
        let model_fields = model.thinking_fields.as_ref();
        let effort_path = model_fields
            .and_then(|f| f.effort_path.as_deref())
            .or(default_fields.effort_path.as_deref());
        let budget_path = model_fields
            .and_then(|f| f.budget_path.as_deref())
            .or(default_fields.budget_path.as_deref());
        let budget_max = model_fields
            .and_then(|f| f.budget_max)
            .or(default_fields.budget_max);
        let toggles = match model_fields.filter(|f| !f.toggles.is_empty()) {
            Some(fields) => &fields.toggles,
            None => &default_fields.toggles,
        };

        let max = match budget_max {
            Some(cap) => Some(match model.max_thinking_budget() {
                Some(model_max) => model_max.min(cap),
                None => cap,
            }),
            None => model.max_thinking_budget(),
        };

        for toggle in toggles {
            match self {
                Self::Off => {
                    if let Some(off) = &toggle.off {
                        set_by_path(body, &toggle.path, off.clone());
                    }
                }
                Self::Adaptive => {
                    if let Some(value) = toggle.adaptive.as_ref().or(toggle.on.as_ref()) {
                        merge_by_path(body, &toggle.path, value.clone());
                    }
                }
                Self::Effort(_) | Self::Budget(_) | Self::WithExtras(_, _) => {
                    if let Some(on) = &toggle.on {
                        merge_by_path(body, &toggle.path, on.clone());
                    }
                }
            }
        }

        if let Budgeted::Tokens(tokens) = self.budget(max) {
            match budget_path {
                Some(path) => set_by_path(body, path, json!(tokens)),
                None => {
                    if let Some((path, key)) = toggles
                        .iter()
                        .find_map(|t| t.budget_key.as_ref().map(|key| (&t.path, key)))
                    {
                        set_by_path(body, &format!("{path}.{key}"), json!(tokens));
                    }
                }
            }
        }

        if let Some(path) = effort_path
            && let Some(effort) = self.effort_str(model.effort_dialect(default_dialect), model)
        {
            set_by_path(body, path, json!(effort));
        }
    }

    /// Parse a `/thinking` command argument.
    ///
    /// # Errors
    ///
    /// Returns `THINKING_USAGE` when `input` is not a valid thinking setting.
    pub fn parse(input: &str, current: Self) -> Result<Self, &'static str> {
        if input.is_empty() {
            return Ok(if current.is_enabled() {
                Self::Off
            } else {
                Self::Adaptive
            });
        }
        StoredThinking::parse_setting(input)
            .map(Into::into)
            .map_err(|_| THINKING_USAGE)
    }

    #[must_use]
    pub fn status_label(self) -> Option<Cow<'static, str>> {
        match self {
            Self::Off => None,
            Self::Adaptive => Some(Cow::Borrowed("thinking")),
            Self::Effort(e) | Self::WithExtras(e, _) => Some(Cow::Owned(format!("thinking: {e}"))),
            Self::Budget(n) => Some(Cow::Owned(format!("thinking: {n}"))),
        }
    }
}

impl std::fmt::Display for ThinkingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::Adaptive => f.write_str("adaptive"),
            Self::Effort(e) | Self::WithExtras(e, _) => f.write_str(e.as_str()),
            Self::Budget(n) => write!(f, "{n}"),
        }
    }
}

impl From<StoredThinking> for ThinkingConfig {
    fn from(s: StoredThinking) -> Self {
        match s {
            StoredThinking::Off => Self::Off,
            StoredThinking::Adaptive => Self::Adaptive,
            StoredThinking::Effort { level } => Self::Effort(level),
            StoredThinking::Budget { tokens } => Self::Budget(tokens),
            StoredThinking::WithExtras {
                level,
                reasoning_mode,
                reasoning_context,
            } => Self::WithExtras(
                level,
                ThinkingExtras {
                    reasoning_mode: reasoning_mode.map(|mode| match mode {
                        StoredReasoningMode::Standard => ReasoningMode::Standard,
                        StoredReasoningMode::Pro => ReasoningMode::Pro,
                    }),
                    reasoning_context: reasoning_context.map(|context| match context {
                        StoredReasoningContext::Auto => ReasoningContext::Auto,
                        StoredReasoningContext::CurrentTurn => ReasoningContext::CurrentTurn,
                        StoredReasoningContext::AllTurns => ReasoningContext::AllTurns,
                    }),
                },
            ),
        }
    }
}

impl From<ThinkingConfig> for StoredThinking {
    fn from(c: ThinkingConfig) -> Self {
        match c {
            ThinkingConfig::Off => Self::Off,
            ThinkingConfig::Adaptive => Self::Adaptive,
            ThinkingConfig::Effort(level) => Self::Effort { level },
            ThinkingConfig::Budget(tokens) => Self::Budget { tokens },
            ThinkingConfig::WithExtras(level, extras) => Self::WithExtras {
                level,
                reasoning_mode: extras.reasoning_mode.map(|mode| match mode {
                    ReasoningMode::Standard => StoredReasoningMode::Standard,
                    ReasoningMode::Pro => StoredReasoningMode::Pro,
                }),
                reasoning_context: extras.reasoning_context.map(|context| match context {
                    ReasoningContext::Auto => StoredReasoningContext::Auto,
                    ReasoningContext::CurrentTurn => StoredReasoningContext::CurrentTurn,
                    ReasoningContext::AllTurns => StoredReasoningContext::AllTurns,
                }),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestOptions {
    pub thinking: ThinkingConfig,
    /// Raw user preference, reconciled by [`RequestOptions::clamped`] before use.
    pub fast: bool,
    /// Number of recent messages whose last content block should be marked with
    /// `cache_control`. Default is 2. Higher values increase cache write cost but
    /// may improve cache hit rates in long conversations.
    pub message_cache_breakpoints: usize,
    pub protect_history_replay: bool,
    pub allow_history_replay: bool,
    /// Optional safety identifier for the request (max 64 chars for `OpenAI`).
    pub safety_identifier: Option<String>,
    /// Whether moderation is enabled for this request.
    pub moderation: bool,
    /// Client-generated idempotency key for the request. When present, the
    /// provider layer can safely retry transport failures that occur after
    /// the request has left the client because the server can deduplicate
    /// repeated requests with the same key.
    pub idempotency_key: Option<String>,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            thinking: Default::default(),
            fast: false,
            message_cache_breakpoints: 2,
            protect_history_replay: false,
            allow_history_replay: false,
            safety_identifier: None,
            moderation: false,
            idempotency_key: None,
        }
    }
}

impl RequestOptions {
    /// Generates a client-side idempotency key for this request if one is not
    /// already set. The same key is reused across retries so the provider can
    /// safely deduplicate repeated requests after transport failures.
    #[must_use]
    pub fn with_idempotency_key(mut self) -> Self {
        if self.idempotency_key.is_none() {
            self.idempotency_key = Some(n00n_storage::id::n00nId::generate().to_string());
        }
        self
    }

    /// Strips options the model does not support. Called once before every
    /// request so UI state, restored sessions, and subagent flags all go
    /// through the same gate.
    #[must_use]
    pub fn clamped(self, model: &crate::model::Model) -> Self {
        Self {
            thinking: if model.supports_thinking() {
                self.thinking
            } else {
                ThinkingConfig::Off
            },
            fast: self.fast && model.supports_fast(),
            message_cache_breakpoints: self.message_cache_breakpoints,
            protect_history_replay: self.protect_history_replay,
            allow_history_replay: self.allow_history_replay,
            safety_identifier: self.safety_identifier,
            moderation: self.moderation,
            idempotency_key: self.idempotency_key,
        }
    }
}

#[derive(Debug)]
pub struct StreamResponse {
    pub message: Message,
    pub usage: TokenUsage,
    pub stop_reason: Option<StopReason>,
}

/// Provider-reported usage quota, independent of local token accounting. Not every
/// provider exposes a programmatic quota endpoint; check `Provider::fetch_usage`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUsage {
    /// Subscription/plan level when the provider reports one (e.g. "lite").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub limits: Vec<UsageLimit>,
}

/// A single quota window (e.g. a 5-hour or weekly token quota).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageLimit {
    /// Human-readable label for the window, provided by the provider.
    pub label: String,
    /// Usage percentage within the window, 0-100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percentage: Option<u32>,
    /// When the window resets, as epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<u64>,
    /// Extra provider-supplied context, e.g. "$2.33 spent" for usage credits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
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
    fn message_control_flag_is_backward_compatible() {
        let legacy: Message = serde_json::from_value(serde_json::json!({
            "role": "user",
            "content": [{ "type": "text", "text": "hello" }]
        }))
        .unwrap();
        assert!(!legacy.control);
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("control")
                .is_none()
        );

        let control = Message::control_display("wrapped".into(), "display".into());
        assert_eq!(serde_json::to_value(control).unwrap()["control"], true);
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

    #[test_case("image/png",  Some(ImageMediaType::Png)  ; "png")]
    #[test_case("image/webp", Some(ImageMediaType::Webp) ; "webp")]
    #[test_case("image/bmp",  None                       ; "unsupported")]
    fn media_type_from_mime(mime: &str, expected: Option<ImageMediaType>) {
        assert_eq!(ImageMediaType::from_mime(mime), expected);
    }

    #[test]
    fn adapt_images_borrows_when_model_has_vision_or_no_images() {
        let model = clamp_test_model(crate::provider::ProviderKind::Anthropic);
        let with_image = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                source: ImageSource::new(ImageMediaType::Png, Arc::from("abc123")),
            }],
            ..Default::default()
        }];
        assert!(matches!(
            adapt_images_for_model(&model, &with_image),
            Cow::Borrowed(_)
        ));

        let mut text_only_model = model;
        text_only_model.supports_vision_override = Some(false);
        let no_images = vec![Message::user("hi".into())];
        assert!(matches!(
            adapt_images_for_model(&text_only_model, &no_images),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn adapt_images_replaces_blocks_for_text_only_model() {
        let mut model = clamp_test_model(crate::provider::ProviderKind::Anthropic);
        model.supports_vision_override = Some(false);
        let messages = vec![Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "[image: pic.png 1KB]".into(),
                    is_error: false,
                },
                ContentBlock::Image {
                    source: ImageSource::new(ImageMediaType::Png, Arc::from("abc123")),
                },
            ],
            ..Default::default()
        }];
        let adapted = adapt_images_for_model(&model, &messages);
        assert_eq!(adapted[0].content.len(), 2);
        assert!(matches!(
            &adapted[0].content[0],
            ContentBlock::ToolResult { .. }
        ));
        assert!(
            matches!(&adapted[0].content[1], ContentBlock::Text { text } if text == IMAGE_OMITTED_NOTE)
        );
    }

    #[test]
    fn image_source_serde_injects_type_base64() {
        let source = ImageSource::new(ImageMediaType::Png, Arc::from("abc123"));
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json["type"], "base64");
        assert_eq!(json["media_type"], "image/png");
        assert_eq!(json["data"], "abc123");
        let deserialized: ImageSource = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.media_type, ImageMediaType::Png);
        assert_eq!(&*deserialized.data, "abc123");
    }

    use Effort::{High, Low, Max, Minimal, XHigh};

    #[test]
    fn image_source_rejects_unknown_detail() {
        let error = serde_json::from_value::<ImageSource>(serde_json::json!({
            "type": "url",
            "url": "https://example.com/image.png",
            "detail": "ultra"
        }))
        .unwrap_err();

        assert!(error.to_string().contains("unknown ImageDetail 'ultra'"));
    }

    #[test]
    fn image_source_rejects_malformed_type() {
        let error = serde_json::from_value::<ImageSource>(serde_json::json!({
            "type": 42,
            "media_type": "image/png",
            "data": "abc123"
        }))
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("ImageSource type must be a string")
        );
    }

    #[test]
    fn image_source_missing_type_uses_legacy_base64_shape() {
        let source = serde_json::from_value::<ImageSource>(serde_json::json!({
            "media_type": "image/png",
            "data": "abc123"
        }))
        .unwrap();

        assert_eq!(source.media_type, ImageMediaType::Png);
        assert_eq!(source.data.as_ref(), "abc123");
        assert!(source.file_id.is_none());
        assert!(source.url.is_none());
    }

    /// `max_output_tokens: 8192`, so `max_thinking_budget()` is 4096.
    fn thinking_model(id: &str) -> crate::model::Model {
        crate::model::Model {
            id: id.into(),
            ..clamp_test_model(crate::provider::ProviderKind::Anthropic)
        }
    }

    #[test]
    fn dialects_have_non_empty_ascending_supported() {
        let all = [
            &dialect::STANDARD,
            &dialect::OPENAI_EXTENDED,
            &dialect::PREFER_HIGH,
            &dialect::HIGH_ONLY,
            &dialect::GLM,
            &dialect::DEEPSEEK,
            &dialect::ANTHROPIC_ADAPTIVE,
            &dialect::TENSORX,
        ];
        for d in all {
            assert!(!d.supported.is_empty());
            for pair in d.supported.windows(2) {
                assert!(pair[0] < pair[1], "supported must be strictly ascending");
            }
            if let Some(adaptive) = d.adaptive {
                assert!(d.supported.contains(&adaptive));
            }
        }
    }

    #[test_case(ThinkingConfig::Off, "claude-opus-4-5", &json!({}) ; "off")]
    #[test_case(ThinkingConfig::Adaptive, "claude-opus-4-5", &json!({"thinking": {"type": "adaptive"}}) ; "adaptive")]
    #[test_case(ThinkingConfig::Budget(2048), "claude-opus-4-5", &json!({"thinking": {"type": "enabled", "budget_tokens": 2048}}) ; "budget_legacy_in_range")]
    #[test_case(ThinkingConfig::Budget(10000), "claude-opus-4-5", &json!({"thinking": {"type": "enabled", "budget_tokens": 4096}}) ; "budget_legacy_clamped_to_max")]
    #[test_case(ThinkingConfig::Budget(10000), "claude-sonnet-4-6", &json!({"thinking": {"type": "enabled", "budget_tokens": 4096}}) ; "budget_legacy_sonnet")]
    #[test_case(ThinkingConfig::Budget(10000), "claude-opus-4-6", &json!({"thinking": {"type": "enabled", "budget_tokens": 4096}}) ; "budget_legacy_opus_4_6")]
    #[test_case(ThinkingConfig::Off, "claude-opus-4-7", &json!({}) ; "off_adaptive_model")]
    #[test_case(ThinkingConfig::Adaptive, "claude-opus-4-7", &json!({"thinking": {"type": "adaptive"}}) ; "adaptive_adaptive_model")]
    #[test_case(ThinkingConfig::Budget(10000), "claude-opus-4-7", &json!({"thinking": {"type": "adaptive"}, "output_config": {"effort": "high"}}) ; "budget_adaptive_opus_4_7")]
    #[test_case(ThinkingConfig::Effort(Low), "claude-opus-4-7", &json!({"thinking": {"type": "adaptive"}, "output_config": {"effort": "low"}}) ; "effort_low_passthrough")]
    #[test_case(ThinkingConfig::Budget(10000), "claude-opus-4-8-1m", &json!({"thinking": {"type": "adaptive"}, "output_config": {"effort": "high"}}) ; "budget_adaptive_opus_4_8_long_context")]
    #[test_case(ThinkingConfig::Budget(10000), "claude-opus-5-0", &json!({"thinking": {"type": "adaptive"}, "output_config": {"effort": "high"}}) ; "budget_adaptive_future_opus_5")]
    #[test_case(ThinkingConfig::Budget(10000), "claude-opus-4.7", &json!({"thinking": {"type": "adaptive"}, "output_config": {"effort": "high"}}) ; "budget_adaptive_copilot_dotted_id")]
    #[test_case(ThinkingConfig::Budget(10000), "claude-opus-4.6", &json!({"thinking": {"type": "enabled", "budget_tokens": 4096}}) ; "budget_legacy_copilot_dotted_4_6")]
    fn thinking_apply_to_body(config: ThinkingConfig, model_id: &str, expected: &Value) {
        let mut body = json!({});
        config.apply_to_body(&mut body, &thinking_model(model_id));
        assert_eq!(body, *expected);
    }

    #[test_case(&dialect::STANDARD, ThinkingConfig::Off,             None            ; "standard_off_noop")]
    #[test_case(&dialect::STANDARD, ThinkingConfig::Adaptive,        Some("medium")  ; "standard_adaptive")]
    #[test_case(&dialect::STANDARD, ThinkingConfig::Effort(Minimal), Some("minimal") ; "standard_minimal_passthrough")]
    #[test_case(&dialect::STANDARD, ThinkingConfig::Effort(Max),     Some("high")    ; "standard_max_snaps_down")]
    #[test_case(&dialect::STANDARD, ThinkingConfig::Budget(1024),    Some("medium")  ; "standard_quarter_budget")]
    #[test_case(&dialect::PREFER_HIGH, ThinkingConfig::Adaptive,        Some("high") ; "prefer_high_adaptive")]
    #[test_case(&dialect::HIGH_ONLY, ThinkingConfig::Adaptive,        Some("high") ; "high_only_adaptive")]
    #[test_case(&dialect::HIGH_ONLY, ThinkingConfig::Effort(Minimal), Some("high") ; "high_only_minimal")]
    #[test_case(&dialect::GLM, ThinkingConfig::Off,          Some("none")  ; "glm_off_explicit_none")]
    #[test_case(&dialect::GLM, ThinkingConfig::Adaptive,     Some("high")  ; "glm_adaptive")]
    #[test_case(&dialect::GLM, ThinkingConfig::Effort(Max),  Some("xhigh") ; "glm_max_snaps_to_xhigh")]
    #[test_case(&dialect::DEEPSEEK, ThinkingConfig::Adaptive,        None        ; "deepseek_adaptive_uses_api_default")]
    #[test_case(&dialect::DEEPSEEK, ThinkingConfig::Effort(Minimal), Some("max") ; "deepseek_minimal")]
    #[test_case(&dialect::ANTHROPIC_ADAPTIVE, ThinkingConfig::Adaptive,      None         ; "anthropic_adaptive_is_native")]
    #[test_case(&dialect::ANTHROPIC_ADAPTIVE, ThinkingConfig::Effort(XHigh), Some("high") ; "anthropic_xhigh_snaps_down")]
    #[test_case(&dialect::TENSORX, ThinkingConfig::Off,             Some("none") ; "tensorx_off_explicit_none")]
    fn thinking_effort_reaches_the_body(
        dialect: &EffortDialect,
        config: ThinkingConfig,
        expected: Option<&str>,
    ) {
        let fields = ThinkingFieldConfig {
            effort_path: Some("reasoning_effort".into()),
            ..Default::default()
        };
        let mut body = json!({"model": "test"});
        config.apply_thinking(&mut body, &thinking_model("test-model"), dialect, &fields);
        match expected {
            Some(e) => assert_eq!(body["reasoning_effort"], e),
            None => assert!(body.get("reasoning_effort").is_none()),
        }
    }

    #[test_case(ThinkingConfig::Off,             Some(4096), Budgeted::Off            ; "off")]
    #[test_case(ThinkingConfig::Adaptive,        Some(4096), Budgeted::Adaptive       ; "adaptive")]
    #[test_case(ThinkingConfig::Effort(Max),     Some(4096), Budgeted::Tokens(4096)   ; "effort_delegates_to_level_budget")]
    #[test_case(ThinkingConfig::Budget(2048),    Some(4096), Budgeted::Tokens(2048)   ; "budget_in_range")]
    #[test_case(ThinkingConfig::Budget(512),     Some(4096), Budgeted::Tokens(1024)   ; "budget_floored")]
    #[test_case(ThinkingConfig::Budget(10000),   Some(4096), Budgeted::Tokens(4096)   ; "budget_clamped_to_max")]
    #[test_case(ThinkingConfig::Budget(2048),    Some(512),  Budgeted::Tokens(1024)   ; "tiny_max_raised_to_floor")]
    #[test_case(ThinkingConfig::Budget(16384),   None,       Budgeted::Tokens(16384)  ; "unknown_max_passes_budget_through")]
    #[test_case(ThinkingConfig::Budget(512),     None,       Budgeted::Tokens(1024)   ; "unknown_max_still_floors")]
    #[test_case(ThinkingConfig::Effort(Max),     None,       Budgeted::Tokens(32_768) ; "unknown_max_effort_scales_fallback")]
    #[test_case(ThinkingConfig::Effort(Minimal), None,       Budgeted::Tokens(3_276)  ; "unknown_max_minimal_effort")]
    fn thinking_budget_resolver(config: ThinkingConfig, max: Option<u32>, expected: Budgeted) {
        assert_eq!(config.budget(max), expected);
    }

    #[test]
    fn apply_thinking_writes_effort_at_nested_path() {
        let fields = ThinkingFieldConfig {
            effort_path: Some("reasoning.effort".into()),
            ..Default::default()
        };
        let mut body = json!({"messages": []});
        ThinkingConfig::Effort(Effort::High).apply_thinking(
            &mut body,
            &thinking_model("test-model"),
            &dialect::PREFER_HIGH,
            &fields,
        );
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["messages"], json!([]));
    }

    #[test_case(ThinkingConfig::Off,      &json!({"type": "disabled"}) ; "off_uses_off_value")]
    #[test_case(ThinkingConfig::Adaptive, &json!({"type": "adaptive"}) ; "adaptive_uses_adaptive_value")]
    #[test_case(ThinkingConfig::Effort(Effort::High), &json!({"type": "enabled"}) ; "effort_uses_on_value")]
    fn apply_thinking_toggle_per_state(config: ThinkingConfig, expected: &Value) {
        let fields = ThinkingFieldConfig {
            toggles: vec![ToggleEntry {
                path: "thinking".into(),
                on: Some(json!({"type": "enabled"})),
                off: Some(json!({"type": "disabled"})),
                adaptive: Some(json!({"type": "adaptive"})),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut body = json!({});
        config.apply_thinking(
            &mut body,
            &thinking_model("test-model"),
            &dialect::STANDARD,
            &fields,
        );
        assert_eq!(&body["thinking"], expected);
    }

    /// `budget_key` nests the resolved budget inside the toggle object without
    /// clobbering the static fields the toggle already wrote.
    #[test]
    fn apply_thinking_budget_key_nests_under_toggle() {
        let fields = ThinkingFieldConfig {
            toggles: vec![ToggleEntry {
                path: "thinking".into(),
                on: Some(json!({"type": "enabled"})),
                budget_key: Some("budget_tokens".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut body = json!({});
        ThinkingConfig::Budget(2048).apply_thinking(
            &mut body,
            &thinking_model("test-model"),
            &dialect::STANDARD,
            &fields,
        );
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 2048})
        );
    }

    /// `budget_max` caps below the model's own half-window ceiling.
    #[test]
    fn apply_thinking_budget_max_caps_request() {
        let fields = ThinkingFieldConfig {
            budget_path: Some("generationConfig.thinkingConfig.thinkingBudget".into()),
            budget_max: Some(1500),
            ..Default::default()
        };
        let mut body = json!({});
        ThinkingConfig::Budget(9999).apply_thinking(
            &mut body,
            &thinking_model("test-model"),
            &dialect::STANDARD,
            &fields,
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            1500
        );
    }

    /// A model's declared layout overrides the provider default field by
    /// field: the model's effort path wins, the provider's toggle survives.
    #[test]
    fn apply_thinking_model_fields_partially_override_provider_defaults() {
        let provider_fields = ThinkingFieldConfig {
            effort_path: Some("reasoning_effort".into()),
            toggles: vec![ToggleEntry {
                path: "thinking".into(),
                on: Some(json!(true)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut model = thinking_model("test-model");
        model.thinking_fields = Some(ThinkingFieldConfig {
            effort_path: Some("reasoning.effort".into()),
            ..Default::default()
        });
        let mut body = json!({});
        ThinkingConfig::Effort(Effort::Low).apply_thinking(
            &mut body,
            &model,
            &dialect::STANDARD,
            &provider_fields,
        );
        assert_eq!(body["reasoning"]["effort"], "low");
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["thinking"], json!(true));
    }

    /// `thinking_dialect` re-snaps the effort level to the declared dialect.
    #[test]
    fn apply_thinking_model_dialect_overrides_provider_dialect() {
        let fields = ThinkingFieldConfig {
            effort_path: Some("reasoning_effort".into()),
            ..Default::default()
        };
        let mut model = thinking_model("test-model");
        model.thinking_dialect = Some(EffortDialectId::HighOnly);
        let mut body = json!({});
        ThinkingConfig::Effort(Effort::Low).apply_thinking(
            &mut body,
            &model,
            &dialect::STANDARD,
            &fields,
        );
        assert_eq!(body["reasoning_effort"], "high");
    }

    /// Anthropic's hardcoded layout yields to a model that declares its own.
    #[test]
    fn apply_to_body_defers_to_model_thinking_fields() {
        let mut model = thinking_model("claude-opus-4-8");
        model.thinking_fields = Some(ThinkingFieldConfig {
            toggles: vec![ToggleEntry {
                path: "chat_template_kwargs".into(),
                on: Some(json!({"enable_thinking": true})),
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut body = json!({});
        ThinkingConfig::Effort(Effort::High).apply_to_body(&mut body, &model);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
        assert!(body.get("thinking").is_none());
    }

    fn clamp_test_model(provider: crate::provider::ProviderKind) -> crate::model::Model {
        crate::model::Model {
            id: "test-model".into(),
            provider: std::sync::Arc::<str>::from(provider.to_string()),
            tier: crate::model::ModelTier::Medium,
            family: provider.family(),
            supports_tool_examples_override: None,
            supports_thinking_override: None,
            supports_vision_override: Some(provider.family().supports_vision()),
            supports_files_override: None,
            pricing: crate::model::ModelPricing::default(),
            max_output_tokens: Some(8192),
            context_window: 200_000,
            thinking_dialect: None,
            thinking_fields: None,
            body_override: None,
        }
    }

    #[test_case(None,        ThinkingConfig::Adaptive, ThinkingConfig::Adaptive ; "provider_default_keeps")]
    #[test_case(Some(false), ThinkingConfig::Adaptive, ThinkingConfig::Off      ; "override_false_clamps")]
    fn request_options_clamped_thinking(
        supports: Option<bool>,
        thinking: ThinkingConfig,
        expected: ThinkingConfig,
    ) {
        let mut model = clamp_test_model(crate::provider::ProviderKind::Anthropic);
        model.supports_thinking_override = supports;
        let opts = RequestOptions {
            thinking,
            fast: false,
            message_cache_breakpoints: 2,
            protect_history_replay: false,
            allow_history_replay: false,
            safety_identifier: None,
            moderation: false,
            idempotency_key: None,
        };
        assert_eq!(opts.clamped(&model).thinking, expected);
    }

    #[test]
    fn request_options_clamped_fast_requires_model_support() {
        let model = clamp_test_model(crate::provider::ProviderKind::Google);
        let opts = RequestOptions {
            thinking: ThinkingConfig::Off,
            fast: true,
            message_cache_breakpoints: 2,
            protect_history_replay: false,
            allow_history_replay: false,
            safety_identifier: None,
            moderation: false,
            idempotency_key: None,
        };
        assert!(!opts.clamped(&model).fast);
    }

    #[test_case("",         ThinkingConfig::Off,      Ok(ThinkingConfig::Adaptive)  ; "toggle_on")]
    #[test_case("",         ThinkingConfig::Adaptive, Ok(ThinkingConfig::Off)       ; "toggle_off")]
    #[test_case("off",      ThinkingConfig::Adaptive, Ok(ThinkingConfig::Off)       ; "explicit_off")]
    #[test_case("adaptive", ThinkingConfig::Off,      Ok(ThinkingConfig::Adaptive)  ; "explicit_adaptive")]
    #[test_case("high",     ThinkingConfig::Off,      Ok(ThinkingConfig::Effort(High)) ; "explicit_effort")]
    #[test_case("8192",     ThinkingConfig::Off,      Ok(ThinkingConfig::Budget(8192)) ; "explicit_budget")]
    #[test_case("512",      ThinkingConfig::Off,      Ok(ThinkingConfig::Budget(512)) ; "small_budget")]
    #[test_case("0",        ThinkingConfig::Off,      Err(())                       ; "budget_zero")]
    #[test_case("garbage",  ThinkingConfig::Off,      Err(())                       ; "invalid_input")]
    fn thinking_parse(input: &str, current: ThinkingConfig, expected: Result<ThinkingConfig, ()>) {
        let result = ThinkingConfig::parse(input, current).map_err(|_| ());
        assert_eq!(result, expected);
    }

    #[test_case(ThinkingConfig::Off, ThinkingConfig::Off ; "off")]
    #[test_case(ThinkingConfig::Adaptive, ThinkingConfig::Adaptive ; "adaptive")]
    #[test_case(ThinkingConfig::Effort(Max), ThinkingConfig::Effort(Max) ; "effort")]
    #[test_case(ThinkingConfig::Budget(8192), ThinkingConfig::Budget(8192) ; "budget")]
    #[test_case(
        ThinkingConfig::WithExtras(Max, ThinkingExtras {
            reasoning_mode: Some(ReasoningMode::Pro),
            reasoning_context: Some(ReasoningContext::AllTurns),
        }),
        ThinkingConfig::Effort(Max);
        "with_extras_display_narrows_to_effort"
    )]
    fn thinking_display_round_trip(config: ThinkingConfig, expected: ThinkingConfig) {
        let s = config.to_string();
        let parsed = ThinkingConfig::parse(&s, ThinkingConfig::Off).unwrap();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn thinking_serde_no_signature_omits_field() {
        let block = ContentBlock::Thinking {
            thinking: "x".into(),
            signature: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert!(json.get("signature").is_none());
    }

    #[test]
    fn thinking_config_with_extras_extracts_effort() {
        let config = ThinkingConfig::WithExtras(
            High,
            ThinkingExtras {
                reasoning_mode: Some(ReasoningMode::Pro),
                reasoning_context: Some(ReasoningContext::AllTurns),
            },
        );
        assert_eq!(config.effort(), Some(High));
    }

    #[test]
    fn thinking_config_with_extras_extracts_extras() {
        let extras = ThinkingExtras {
            reasoning_mode: Some(ReasoningMode::Pro),
            reasoning_context: Some(ReasoningContext::CurrentTurn),
        };
        let config = ThinkingConfig::WithExtras(High, extras);
        let extracted = config.extras();
        assert_eq!(extracted.reasoning_mode, Some(ReasoningMode::Pro));
        assert_eq!(
            extracted.reasoning_context,
            Some(ReasoningContext::CurrentTurn)
        );
    }

    #[test_case(
        ThinkingConfig::WithExtras(High, ThinkingExtras {
            reasoning_mode: Some(ReasoningMode::Pro),
            reasoning_context: Some(ReasoningContext::CurrentTurn),
        });
        "preserves_all_extras"
    )]
    fn thinking_config_stored_round_trip(config: ThinkingConfig) {
        let stored = StoredThinking::from(config);
        assert_eq!(ThinkingConfig::from(stored), config);
    }

    #[test]
    fn thinking_config_without_extras_returns_default_extras() {
        let config = ThinkingConfig::Effort(High);
        let extras = config.extras();
        assert_eq!(extras.reasoning_mode, None);
        assert_eq!(extras.reasoning_context, None);
    }

    #[test]
    fn thinking_config_effort_str_with_extras() {
        let config = ThinkingConfig::WithExtras(
            XHigh,
            ThinkingExtras {
                reasoning_mode: Some(ReasoningMode::Pro),
                reasoning_context: None,
            },
        );
        let dialect = &dialect::OPENAI_EXTENDED;
        let model = thinking_model("gpt-5.6-sol");
        let effort_str = config.effort_str(dialect, &model);
        assert_eq!(effort_str, Some("xhigh"));
    }

    #[test]
    fn request_options_default_has_no_safety_or_moderation() {
        let opts = RequestOptions::default();
        assert!(opts.safety_identifier.is_none());
        assert!(!opts.moderation);
    }

    #[test]
    fn request_options_clamped_preserves_safety_and_moderation() {
        let model = clamp_test_model(crate::provider::ProviderKind::OpenAi);
        let opts = RequestOptions {
            thinking: ThinkingConfig::Off,
            fast: false,
            message_cache_breakpoints: 2,
            protect_history_replay: false,
            allow_history_replay: false,
            safety_identifier: Some("test-id".to_string()),
            moderation: true,
            idempotency_key: None,
        };
        let clamped = opts.clamped(&model);
        assert_eq!(clamped.safety_identifier, Some("test-id".to_string()));
        assert!(clamped.moderation);
    }
}
