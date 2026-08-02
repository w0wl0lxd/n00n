//! Prost-derived protobuf helpers for Devin Connect messages.
//!
//! Field numbers and enum values are preserved from the hand-rolled version that
//! captured requests emitted by Devin CLI `3000.3.22`.

use prost::Message;
use std::collections::HashMap;

const CLI_SOURCE: &str = "devin";
const CLI_VERSION: &str = "3000.3.22";

pub(crate) const CHAT_MESSAGE_SOURCE_USER: u64 = 1;
pub(crate) const CHAT_MESSAGE_SOURCE_SYSTEM: u64 = 2;
pub(crate) const CHAT_MESSAGE_SOURCE_TOOL: u64 = 4;

const CONVERSATIONAL_PLANNER_MODE_DEFAULT: u64 = 1;
const PROVIDER_SOURCE_CASCADE: u64 = 12;
const LANGUAGE_UNSPECIFIED: u64 = 0;
const CACHE_CONTROL_TYPE_EPHEMERAL: u64 = 1;
const CHAT_MESSAGE_REQUEST_TYPE_CASCADE: u64 = 5;

pub(crate) const STOP_REASON_UNSPECIFIED: u32 = 0;
pub(crate) const STOP_REASON_MAX_TOKENS: u32 = 3;
pub(crate) const STOP_REASON_TOOL_USE: u32 = 10;

const DEFAULT_STOP_PATTERNS: &[&str] = &[
    "<|user|>",
    "<|bot|>",
    "<|context_request|>",
    "<|endoftext|>",
    "<|end_of_turn|>",
];

/// Input for `encode_chat_message_prompt`.
#[derive(Debug, Clone, Default)]
pub struct ChatMessagePromptInput<'a> {
    pub message_id: &'a str,
    pub source: u64,
    pub prompt: &'a str,
    pub tool_calls: &'a [ChatToolCall],
    pub tool_call_id: &'a str,
    pub tool_result_is_error: bool,
    pub images: &'a [ImageData],
    pub thinking: &'a str,
    pub signature: &'a str,
}

#[derive(Clone, PartialEq, prost::Oneof)]
pub(crate) enum ChatToolChoiceOneof {
    #[prost(string, tag = "1")]
    OptionName(String),
    #[prost(string, tag = "2")]
    ToolName(String),
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ChatToolChoice {
    #[prost(oneof = "ChatToolChoiceOneof", tags = "1, 2")]
    pub choice: Option<ChatToolChoiceOneof>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Metadata {
    #[prost(string, tag = "1")]
    pub ide_name: String,
    #[prost(string, tag = "2")]
    pub extension_version: String,
    #[prost(string, tag = "3")]
    pub api_key: String,
    #[prost(string, tag = "4")]
    pub locale: String,
    #[prost(string, tag = "5")]
    pub os: String,
    #[prost(bool, tag = "6")]
    pub disable_telemetry: bool,
    #[prost(string, tag = "7")]
    pub ide_version: String,
    #[prost(string, tag = "12")]
    pub extension_name: String,
    #[prost(string, tag = "21")]
    pub user_jwt: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetUserJwtRequest {
    #[prost(message, optional, tag = "1")]
    pub metadata: Option<Metadata>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetUserJwtResponse {
    #[prost(string, tag = "1")]
    pub user_jwt: String,
    #[prost(string, tag = "2")]
    pub custom_api_server_url: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ImageData {
    #[prost(string, tag = "1")]
    pub base64_data: String,
    #[prost(string, tag = "2")]
    pub mime_type: String,
    #[prost(string, tag = "3")]
    pub caption: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ChatToolCall {
    #[prost(string, tag = "1")]
    pub id: String,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(string, tag = "3")]
    pub arguments_json: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ChatMessagePrompt {
    #[prost(string, tag = "1")]
    pub message_id: String,
    #[prost(uint64, tag = "2")]
    pub source: u64,
    #[prost(string, tag = "3")]
    pub prompt: String,
    #[prost(message, repeated, tag = "6")]
    pub tool_calls: Vec<ChatToolCall>,
    #[prost(string, tag = "7")]
    pub tool_call_id: String,
    #[prost(bool, tag = "9")]
    pub tool_result_is_error: bool,
    #[prost(message, repeated, tag = "10")]
    pub images: Vec<ImageData>,
    #[prost(string, tag = "11")]
    pub thinking: String,
    #[prost(string, tag = "12")]
    pub signature: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ChatToolDefinition {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub description: String,
    #[prost(string, tag = "3")]
    pub json_schema_string: String,
    #[prost(bool, tag = "12")]
    pub strict: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct PromptCacheOptions {
    #[prost(uint64, tag = "1")]
    pub cache_control_type: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CompletionConfiguration {
    #[prost(uint64, tag = "1")]
    pub num_completions: u64,
    #[prost(uint64, tag = "2")]
    pub max_tokens: u64,
    #[prost(uint64, tag = "3")]
    pub max_newlines: u64,
    #[prost(double, tag = "5")]
    pub temperature: f64,
    #[prost(double, tag = "6")]
    pub first_temperature: f64,
    #[prost(uint64, tag = "7")]
    pub top_k: u64,
    #[prost(double, tag = "8")]
    pub top_p: f64,
    #[prost(string, repeated, tag = "9")]
    pub stop_patterns: Vec<String>,
    #[prost(double, tag = "11")]
    pub fim_eot_prob_threshold: f64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetChatMessageRequest {
    #[prost(message, optional, tag = "1")]
    pub metadata: Option<Metadata>,
    #[prost(string, tag = "2")]
    pub prompt: String,
    #[prost(bytes, repeated, tag = "3")]
    pub chat_message_prompts: Vec<Vec<u8>>,
    #[prost(string, tag = "21")]
    pub chat_model_uid: String,
    #[prost(uint64, tag = "7")]
    pub request_type: u64,
    #[prost(message, optional, tag = "8")]
    pub configuration: Option<CompletionConfiguration>,
    #[prost(bytes, repeated, tag = "10")]
    pub tools: Vec<Vec<u8>>,
    #[prost(bool, tag = "11")]
    pub disable_parallel_tool_calls: bool,
    #[prost(message, optional, tag = "12")]
    pub tool_choice: Option<ChatToolChoice>,
    #[prost(message, optional, tag = "13")]
    pub system_prompt_cache_options: Option<PromptCacheOptions>,
    #[prost(string, tag = "16")]
    pub cascade_id: String,
    #[prost(uint64, tag = "18")]
    pub provider_source: u64,
    #[prost(uint64, tag = "19")]
    pub language: u64,
    #[prost(uint64, tag = "20")]
    pub planner_mode: u64,
    #[prost(string, tag = "22")]
    pub execution_id: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetChatMessageResponse {
    #[prost(string, tag = "1")]
    pub message_id: String,
    #[prost(string, tag = "3")]
    pub delta_text: String,
    #[prost(uint32, tag = "5")]
    pub stop_reason: u32,
    #[prost(message, repeated, tag = "6")]
    pub delta_tool_calls: Vec<ChatToolCall>,
    #[prost(message, optional, tag = "7")]
    pub usage: Option<ModelUsageStats>,
    #[prost(string, tag = "9")]
    pub delta_thinking: String,
    #[prost(string, tag = "10")]
    pub delta_signature: String,
}

#[derive(Clone, PartialEq, prost::Message)]
#[allow(clippy::struct_field_names)]
pub(crate) struct ModelUsageStats {
    #[prost(uint64, tag = "1")]
    pub input_tokens: u64,
    #[prost(uint64, tag = "2")]
    pub output_tokens: u64,
    #[prost(uint64, tag = "3")]
    pub cache_read_tokens: u64,
    #[prost(uint64, tag = "4")]
    pub cache_write_tokens: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetCliModelConfigsRequest {
    #[prost(message, optional, tag = "1")]
    pub metadata: Option<Metadata>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetCliModelConfigsResponse {
    #[prost(message, repeated, tag = "1")]
    pub client_model_configs: Vec<ClientModelConfig>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ClientModelConfig {
    #[prost(string, tag = "1")]
    pub label: String,
    #[prost(message, optional, tag = "2")]
    pub model_or_alias: Option<ModelOrAlias>,
    #[prost(string, tag = "22")]
    pub model_uid: String,
    #[prost(message, optional, tag = "23")]
    pub model_info: Option<ModelInfo>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ModelOrAlias {
    #[prost(string, tag = "3")]
    pub model_uid: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ModelInfo {
    #[prost(string, tag = "12")]
    pub chat_model_name: String,
}
#[must_use]
pub fn encode_get_user_jwt_request(api_key: &str) -> Vec<u8> {
    GetUserJwtRequest {
        metadata: Some(Metadata {
            ide_name: CLI_SOURCE.to_string(),
            extension_version: CLI_VERSION.to_string(),
            api_key: api_key.to_string(),
            locale: "en".to_string(),
            os: String::new(),
            disable_telemetry: false,
            ide_version: CLI_VERSION.to_string(),
            extension_name: CLI_SOURCE.to_string(),
            user_jwt: String::new(),
        }),
    }
    .encode_to_vec()
}

pub fn decode_get_user_jwt_response(buf: &[u8]) -> Result<GetUserJwtResponse, String> {
    GetUserJwtResponse::decode(buf).map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn encode_get_chat_message_request(
    api_key: &str,
    user_jwt: &str,
    prompt: &str,
    chat_model_uid: &str,
    cascade_id: &str,
    execution_id: &str,
    chat_message_prompts: &[Vec<u8>],
    chat_tools: &[Vec<u8>],
    max_tokens: u64,
    temperature: f64,
    top_p: f64,
) -> Vec<u8> {
    GetChatMessageRequest {
        metadata: Some(Metadata {
            ide_name: CLI_SOURCE.to_string(),
            extension_version: CLI_VERSION.to_string(),
            api_key: api_key.to_string(),
            locale: "en".to_string(),
            os: String::new(),
            disable_telemetry: false,
            ide_version: CLI_VERSION.to_string(),
            extension_name: CLI_SOURCE.to_string(),
            user_jwt: user_jwt.to_string(),
        }),
        prompt: prompt.to_string(),
        chat_message_prompts: chat_message_prompts.to_vec(),
        chat_model_uid: chat_model_uid.to_string(),
        request_type: CHAT_MESSAGE_REQUEST_TYPE_CASCADE,
        configuration: Some(CompletionConfiguration {
            num_completions: 1,
            max_tokens,
            max_newlines: 200,
            temperature,
            first_temperature: temperature,
            top_k: 50,
            top_p,
            stop_patterns: DEFAULT_STOP_PATTERNS
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            fim_eot_prob_threshold: 1.0,
        }),
        tools: chat_tools.to_vec(),
        disable_parallel_tool_calls: true,
        tool_choice: Some(ChatToolChoice {
            choice: Some(ChatToolChoiceOneof::OptionName("auto".to_string())),
        }),
        system_prompt_cache_options: Some(PromptCacheOptions {
            cache_control_type: CACHE_CONTROL_TYPE_EPHEMERAL,
        }),
        cascade_id: cascade_id.to_string(),
        provider_source: PROVIDER_SOURCE_CASCADE,
        language: LANGUAGE_UNSPECIFIED,
        planner_mode: CONVERSATIONAL_PLANNER_MODE_DEFAULT,
        execution_id: execution_id.to_string(),
    }
    .encode_to_vec()
}

pub fn decode_get_chat_message_response(buf: &[u8]) -> Result<GetChatMessageResponse, String> {
    GetChatMessageResponse::decode(buf).map_err(|e| e.to_string())
}

#[must_use]
pub fn encode_chat_message_prompt(input: &ChatMessagePromptInput<'_>) -> Vec<u8> {
    ChatMessagePrompt {
        message_id: input.message_id.to_string(),
        source: input.source,
        prompt: input.prompt.to_string(),
        tool_calls: input.tool_calls.to_vec(),
        tool_call_id: input.tool_call_id.to_string(),
        tool_result_is_error: input.tool_result_is_error,
        images: input.images.to_vec(),
        thinking: input.thinking.to_string(),
        signature: input.signature.to_string(),
    }
    .encode_to_vec()
}

#[must_use]
pub fn encode_chat_tool_definition(tool: &ChatToolDefinition) -> Vec<u8> {
    tool.encode_to_vec()
}

#[must_use]
pub fn encode_get_cli_model_configs_request(api_key: &str) -> Vec<u8> {
    GetCliModelConfigsRequest {
        metadata: Some(Metadata {
            ide_name: CLI_SOURCE.to_string(),
            extension_version: CLI_VERSION.to_string(),
            api_key: api_key.to_string(),
            locale: "en".to_string(),
            os: String::new(),
            disable_telemetry: false,
            ide_version: CLI_VERSION.to_string(),
            extension_name: CLI_SOURCE.to_string(),
            user_jwt: String::new(),
        }),
    }
    .encode_to_vec()
}

pub fn decode_cli_model_configs(buf: &[u8]) -> Result<HashMap<String, String>, String> {
    let resp = GetCliModelConfigsResponse::decode(buf).map_err(|e| e.to_string())?;
    let mut map = HashMap::new();
    for config in resp.client_model_configs {
        let label = config.label;
        let model_uid_22 = config.model_uid;
        let wire_uid = if let Some(m) = config.model_or_alias.as_ref() {
            m.model_uid.clone()
        } else {
            String::new()
        };
        let chat_model_name = if let Some(m) = config.model_info.as_ref() {
            m.chat_model_name.clone()
        } else {
            String::new()
        };
        let display = if model_uid_22.is_empty() {
            label.clone()
        } else {
            model_uid_22
        };
        let wire = if !chat_model_name.is_empty() {
            chat_model_name
        } else if !wire_uid.is_empty() {
            wire_uid
        } else {
            display.clone()
        };
        if !display.is_empty() {
            map.insert(display.clone(), wire.clone());
        }
        if !label.is_empty() && label != display {
            map.insert(label, wire);
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_get_user_jwt_response_roundtrip() {
        let response = GetUserJwtResponse {
            user_jwt: "test_jwt".to_string(),
            custom_api_server_url: "https://custom.com".to_string(),
        };
        let buf = response.encode_to_vec();
        let decoded = decode_get_user_jwt_response(&buf).expect("decode");
        assert_eq!(decoded.user_jwt, "test_jwt");
        assert_eq!(decoded.custom_api_server_url, "https://custom.com");
    }

    #[test]
    fn decode_chat_response_roundtrip_preserves_all_supported_fields() {
        let tool_call = ChatToolCall {
            id: "call-1".to_string(),
            name: "read".to_string(),
            arguments_json: r#"{"path":"file.txt"}"#.to_string(),
        };
        let usage = ModelUsageStats {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_tokens: 5,
            cache_write_tokens: 3,
        };
        let response = GetChatMessageResponse {
            message_id: "message-1".to_string(),
            delta_text: "text".to_string(),
            stop_reason: STOP_REASON_TOOL_USE,
            delta_tool_calls: vec![tool_call.clone()],
            usage: Some(usage),
            delta_thinking: "thinking".to_string(),
            delta_signature: "signature".to_string(),
        };
        let buf = response.encode_to_vec();
        let decoded = decode_get_chat_message_response(&buf).expect("decode");
        assert_eq!(decoded.message_id, "message-1");
        assert_eq!(decoded.delta_text, "text");
        assert_eq!(decoded.delta_thinking, "thinking");
        assert_eq!(decoded.delta_signature, "signature");
        assert_eq!(decoded.stop_reason, STOP_REASON_TOOL_USE);
        assert_eq!(decoded.delta_tool_calls.len(), 1);
        assert_eq!(decoded.delta_tool_calls[0].id, tool_call.id);
        assert_eq!(decoded.delta_tool_calls[0].name, tool_call.name);
        assert_eq!(
            decoded.delta_tool_calls[0].arguments_json,
            tool_call.arguments_json
        );
        let stats = decoded.usage.expect("usage");
        assert_eq!(stats.input_tokens, 11);
        assert_eq!(stats.output_tokens, 7);
        assert_eq!(stats.cache_read_tokens, 5);
        assert_eq!(stats.cache_write_tokens, 3);
    }

    #[test]
    fn cli_model_config_prefers_chat_model_name_for_display_and_label() {
        let config = ClientModelConfig {
            label: "Model Label".to_string(),
            model_or_alias: Some(ModelOrAlias {
                model_uid: "alias-wire-id".to_string(),
            }),
            model_uid: "display-model-id".to_string(),
            model_info: Some(ModelInfo {
                chat_model_name: "chat-model-name".to_string(),
            }),
        };
        let response = GetCliModelConfigsResponse {
            client_model_configs: vec![config],
        };
        let models = decode_cli_model_configs(&response.encode_to_vec()).expect("decode configs");
        assert_eq!(
            models.get("display-model-id").map(String::as_str),
            Some("chat-model-name")
        );
        assert_eq!(
            models.get("Model Label").map(String::as_str),
            Some("chat-model-name")
        );
    }
}
