// Hand-rolled protobuf helpers for Devin Connect messages.
//
// Based on the proto definitions from:
// - `exa/api_server_pb/api_server.proto`
// - `exa/auth_pb/auth.proto`
// - `exa/chat_pb/chat.proto`
// - `exa/codeium_common_pb/codeium_common.proto`

// Helper to encode a repeated string field
fn encode_repeated_string(field: u64, values: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for value in values {
        out.extend(field_str(field, value));
    }
    out
}

// Helper to encode a repeated message field
fn encode_repeated_message(field: u64, messages: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for msg in messages {
        out.extend(field_ld(field, msg));
    }
    out
}

// ChatMessageSource enum values (will be used when encoding chat_message_prompts)
#[allow(dead_code)]
pub(crate) const CHAT_MESSAGE_SOURCE_UNSPECIFIED: u64 = 0;
pub(crate) const CHAT_MESSAGE_SOURCE_USER: u64 = 1;
pub(crate) const CHAT_MESSAGE_SOURCE_SYSTEM: u64 = 2;
#[allow(dead_code)]
pub(crate) const CHAT_MESSAGE_SOURCE_UNKNOWN: u64 = 3;
pub(crate) const CHAT_MESSAGE_SOURCE_TOOL: u64 = 4;
#[allow(dead_code)]
pub(crate) const CHAT_MESSAGE_SOURCE_SYSTEM_PROMPT: u64 = 5;

// ConversationalPlannerMode enum values
const CONVERSATIONAL_PLANNER_MODE_DEFAULT: u64 = 1;

// ProviderSource enum values
const PROVIDER_SOURCE_CASCADE: u64 = 12;

// Language enum values
const LANGUAGE_UNSPECIFIED: u64 = 0;

// CacheControlType enum values
const CACHE_CONTROL_TYPE_EPHEMERAL: u64 = 1;

// ChatMessageRequestType enum values
const CHAT_MESSAGE_REQUEST_TYPE_CASCADE: u64 = 5;

// StopReason enum values (mapped in devin.rs, not used directly in encoding)
// const STOP_REASON_UNSPECIFIED: u64 = 0;
// const STOP_REASON_MAX_TOKENS: u64 = 1;
// const STOP_REASON_STOP_SEQUENCE: u64 = 2;
// const STOP_REASON_TOOL_USE: u64 = 3;

pub fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

#[must_use]
pub fn field_ld(field: u64, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 10);
    encode_varint((field << 3) | 2, &mut out);
    let len = u64::try_from(data.len()).unwrap_or_else(|_| {
        // If data.len() doesn't fit in u64, truncate to max u64
        // This is extremely unlikely in practice (data would need to be > 18 EB)
        u64::MAX
    });
    encode_varint(len, &mut out);
    out.extend_from_slice(data);
    out
}

#[must_use]
pub fn field_str(field: u64, value: &str) -> Vec<u8> {
    field_ld(field, value.as_bytes())
}

#[must_use]
pub fn field_varint(field: u64, value: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    encode_varint(field << 3, &mut out);
    encode_varint(value, &mut out);
    out
}

// Helper to encode a double value (wire type 1, fixed 64-bit little-endian)
fn field_double(field: u64, value: f64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    encode_varint(field << 3 | 1, &mut out);
    out.extend(value.to_le_bytes());
    out
}

pub fn decode_varint(buf: &[u8]) -> Result<(u64, &[u8]), String> {
    let mut value = 0u64;
    let mut shift = 0u32;
    for (i, byte) in buf.iter().copied().enumerate() {
        if shift >= 64 {
            return Err("varint too long".into());
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, &buf[i + 1..]));
        }
        shift += 7;
    }
    Err("truncated varint".into())
}

// Decode length-delimited fields from a protobuf message body.
pub fn iter_fields(mut buf: &[u8]) -> impl Iterator<Item = Result<(u64, u8, &[u8]), String>> + '_ {
    std::iter::from_fn(move || {
        if !buf.is_empty() {
            let (tag, rest) = match decode_varint(buf) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            buf = rest;
            let field = tag >> 3;
            let wire = (tag & 7) as u8;
            match wire {
                0 => {
                    let (_, rest) = match decode_varint(buf) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let data = &buf[..buf.len() - rest.len()];
                    buf = rest;
                    return Some(Ok((field, wire, data)));
                }
                1 => {
                    // 64-bit fixed (e.g. double)
                    if buf.len() < 8 {
                        return Some(Err("truncated 64-bit field".into()));
                    }
                    let (data, rest) = buf.split_at(8);
                    buf = rest;
                    return Some(Ok((field, wire, data)));
                }
                2 => {
                    let (len, rest) = match decode_varint(buf) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let Ok(len) = usize::try_from(len) else {
                        return Some(Err("protobuf length overflow".into()));
                    };
                    if rest.len() < len {
                        return Some(Err("truncated length-delimited field".into()));
                    }
                    let (data, rest) = rest.split_at(len);
                    buf = rest;
                    return Some(Ok((field, wire, data)));
                }
                5 => {
                    // 32-bit fixed
                    if buf.len() < 4 {
                        return Some(Err("truncated 32-bit field".into()));
                    }
                    let (data, rest) = buf.split_at(4);
                    buf = rest;
                    return Some(Ok((field, wire, data)));
                }
                other => return Some(Err(format!("unsupported protobuf wire type {other}"))),
            }
        }
        None
    })
}

// GetUserJwtRequest encoder (exa.auth_pb.GetUserJwtRequest)

// message GetUserJwtRequest {
//   .exa.codeium_common_pb.Metadata metadata = 1;
// }
pub fn encode_get_user_jwt_request(api_key: &str) -> Vec<u8> {
    let metadata = encode_metadata(api_key);
    field_ld(1, &metadata)
}

// Metadata encoder (exa.codeium_common_pb.Metadata)

// message Metadata (exa.codeium_common_pb.Metadata)
//   string ide_name = 1;
//   string extension_version = 2;
//   string api_key = 3;
//   string locale = 4;
//   string os = 5;
//   bool disable_telemetry = 6;
//   string ide_version = 7;
//   ...
//   string extension_name = 12;
//   ...
//   string user_jwt = 21;
fn encode_metadata(api_key: &str) -> Vec<u8> {
    let mut out = field_str(1, "devin");
    out.extend(field_str(2, "3000.3.22"));
    out.extend(field_str(3, api_key));
    out.extend(field_str(4, "en"));
    out.extend(field_str(7, "3000.3.22"));
    out.extend(field_str(12, "devin"));
    out
}

fn encode_metadata_user_jwt(api_key: &str, user_jwt: &str) -> Vec<u8> {
    let mut out = encode_metadata(api_key);
    out.extend(field_str(21, user_jwt));
    out
}

// GetUserJwtResponse decoder (exa.auth_pb.GetUserJwtResponse)

// message GetUserJwtResponse {
//   string user_jwt = 1;
//   string custom_api_server_url = 2;
// }
#[derive(Debug, Default)]
pub struct GetUserJwtResponse {
    pub user_jwt: String,
    pub custom_api_server_url: String,
}

pub fn decode_get_user_jwt_response(buf: &[u8]) -> Result<GetUserJwtResponse, String> {
    let mut response = GetUserJwtResponse::default();
    for field in iter_fields(buf) {
        let (num, _wire, data) = field?;
        match num {
            1 => {
                response.user_jwt = String::from_utf8_lossy(data).into_owned();
            }
            2 => {
                response.custom_api_server_url = String::from_utf8_lossy(data).into_owned();
            }
            _ => {}
        }
    }
    Ok(response)
}

/// Input for `encode_chat_message_prompt`.
pub struct ChatMessagePromptInput<'a> {
    pub message_id: &'a str,
    pub source: u64,
    pub prompt: &'a str,
    pub tool_calls: &'a [ChatToolCall],
    pub tool_call_id: &'a str,
    pub tool_result_is_error: bool,
    pub images: &'a [ImageData<'a>],
    pub thinking: &'a str,
    pub signature: &'a str,
}

impl Default for ChatMessagePromptInput<'_> {
    fn default() -> Self {
        Self {
            message_id: "",
            source: CHAT_MESSAGE_SOURCE_UNSPECIFIED,
            prompt: "",
            tool_calls: &[],
            tool_call_id: "",
            tool_result_is_error: false,
            images: &[],
            thinking: "",
            signature: "",
        }
    }
}

/// Reference to an inline image attached to a `ChatMessagePrompt`.
#[derive(Debug, Clone, Copy)]
pub struct ImageData<'a> {
    pub base64_data: &'a str,
    pub mime_type: &'a str,
    pub caption: &'a str,
}

// ChatMessagePrompt encoder (exa.chat_pb.ChatMessagePrompt)
pub fn encode_chat_message_prompt(input: &ChatMessagePromptInput<'_>) -> Vec<u8> {
    let mut out = Vec::new();

    // message_id (field 1)
    out.extend(field_str(1, input.message_id));

    // source (field 2)
    out.extend(field_varint(2, input.source));

    // prompt (field 3)
    if !input.prompt.is_empty() {
        out.extend(field_str(3, input.prompt));
    }

    // tool_calls (field 6)
    if !input.tool_calls.is_empty() {
        let encoded_calls: Vec<Vec<u8>> =
            input.tool_calls.iter().map(encode_chat_tool_call).collect();
        out.extend(encode_repeated_message(6, &encoded_calls));
    }

    // tool_call_id (field 7)
    if !input.tool_call_id.is_empty() {
        out.extend(field_str(7, input.tool_call_id));
    }

    // tool_result_is_error (field 9)
    if input.tool_result_is_error {
        out.extend(field_varint(9, 1));
    }

    // images (field 10)
    if !input.images.is_empty() {
        let encoded_images: Vec<Vec<u8>> = input.images.iter().map(encode_image_data).collect();
        out.extend(encode_repeated_message(10, &encoded_images));
    }

    // thinking (field 11)
    if !input.thinking.is_empty() {
        out.extend(field_str(11, input.thinking));
    }

    // signature (field 12)
    if !input.signature.is_empty() {
        out.extend(field_str(12, input.signature));
    }

    out
}

// ImageData encoder (exa.codeium_common_pb.ImageData)
#[must_use]
fn encode_image_data(image: &ImageData<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(field_str(1, image.base64_data));
    out.extend(field_str(2, image.mime_type));
    if !image.caption.is_empty() {
        out.extend(field_str(3, image.caption));
    }
    out
}

// ChatToolCall encoder (exa.codeium_common_pb.ChatToolCall)
#[allow(dead_code)]
fn encode_chat_tool_call(tc: &ChatToolCall) -> Vec<u8> {
    let mut out = Vec::new();

    // id (field 1)
    out.extend(field_str(1, &tc.id));

    // name (field 2)
    out.extend(field_str(2, &tc.name));

    // arguments_json (field 3)
    out.extend(field_str(3, &tc.arguments_json));

    out
}

/// Definition of a tool that can be requested by the model.
#[derive(Debug, Clone, Copy)]
pub struct ChatToolDefinition<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub json_schema_string: &'a str,
    pub strict: bool,
}

// ChatToolDefinition encoder (exa.chat_pb.ChatToolDefinition)
#[must_use]
pub fn encode_chat_tool_definition(tool: &ChatToolDefinition<'_>) -> Vec<u8> {
    let mut out = Vec::new();

    // name (field 1)
    out.extend(field_str(1, tool.name));

    // description (field 2)
    out.extend(field_str(2, tool.description));

    // json_schema_string (field 3)
    out.extend(field_str(3, tool.json_schema_string));

    // strict (field 12)
    out.extend(field_varint(12, u64::from(tool.strict)));

    out
}

// GetChatMessageRequest encoder (exa.api_server_pb.GetChatMessageRequest)
//
// Full encoder matching the TypeScript buildDevinChatRequest function
#[allow(clippy::too_many_arguments)]
pub fn encode_get_chat_message_request(
    api_key: &str,
    user_jwt: &str,
    prompt: &str,
    model_uid: &str,
    cascade_id: &str,
    execution_id: &str,
    chat_message_prompts: &[Vec<u8>],
    tools: &[Vec<u8>],
    max_tokens: u64,
    temperature: f64,
    top_p: f64,
) -> Vec<u8> {
    let mut out = Vec::new();

    // metadata (field 1)
    let metadata = encode_metadata_user_jwt(api_key, user_jwt);
    out.extend(field_ld(1, &metadata));

    // prompt (field 2)
    out.extend(field_str(2, prompt));

    // chat_message_prompts (field 3)
    out.extend(encode_repeated_message(3, chat_message_prompts));

    // chat_model_uid (field 21)
    out.extend(field_str(21, model_uid));

    // request_type = CASCADE (field 7, value 5)
    out.extend(field_varint(7, CHAT_MESSAGE_REQUEST_TYPE_CASCADE));

    // configuration (field 8)
    let config = encode_completion_configuration(max_tokens, temperature, top_p);
    out.extend(field_ld(8, &config));

    // tools (field 10)
    out.extend(encode_repeated_message(10, tools));

    // disable_parallel_tool_calls (field 11)
    out.extend(field_varint(11, 1));

    // tool_choice (field 12) - auto
    let tool_choice = encode_chat_tool_choice_auto();
    out.extend(field_ld(12, &tool_choice));

    // system_prompt_cache_options (field 13) - EPHEMERAL
    let cache_options = encode_prompt_cache_options_ephemeral();
    out.extend(field_ld(13, &cache_options));

    // cascade_id (field 16)
    out.extend(field_str(16, cascade_id));

    // provider_source = CASCADE (field 18, value 12)
    out.extend(field_varint(18, PROVIDER_SOURCE_CASCADE));

    // language = UNSPECIFIED (field 19, value 0)
    out.extend(field_varint(19, LANGUAGE_UNSPECIFIED));

    // planner_mode = DEFAULT (field 20, value 1)
    out.extend(field_varint(20, CONVERSATIONAL_PLANNER_MODE_DEFAULT));

    // execution_id (field 22)
    out.extend(field_str(22, execution_id));

    out
}

// CompletionConfiguration encoder (exa.codeium_common_pb.CompletionConfiguration)
fn encode_completion_configuration(max_tokens: u64, temperature: f64, top_p: f64) -> Vec<u8> {
    let mut out = Vec::new();

    // num_completions = 1 (field 1)
    out.extend(field_varint(1, 1));

    // max_tokens (field 2)
    out.extend(field_varint(2, max_tokens));

    // max_newlines = 200 (field 3)
    out.extend(field_varint(3, 200));

    // temperature (field 5)
    out.extend(field_double(5, temperature));

    // first_temperature = temperature (field 6)
    out.extend(field_double(6, temperature));

    // top_k = 50 (field 7)
    out.extend(field_varint(7, 50));

    // top_p (field 8)
    out.extend(field_double(8, top_p));

    // stop_patterns (field 9)
    let stop_patterns = vec![
        "<|user|>".to_string(),
        "<|bot|>".to_string(),
        "<|context_request|>".to_string(),
        "<|endoftext|>".to_string(),
        "<|end_of_turn|>".to_string(),
    ];
    out.extend(encode_repeated_string(9, &stop_patterns));

    // fim_eot_prob_threshold = 1 (field 11)
    out.extend(field_double(11, 1.0));

    out
}

// ChatToolChoice encoder (exa.chat_pb.ChatToolChoice) - auto
fn encode_chat_tool_choice_auto() -> Vec<u8> {
    // oneof choice: option_name = "auto" (field 1)
    field_str(1, "auto")
}

// PromptCacheOptions encoder (exa.chat_pb.PromptCacheOptions) - EPHEMERAL
fn encode_prompt_cache_options_ephemeral() -> Vec<u8> {
    // type = EPHEMERAL (field 1, value 1)
    field_varint(1, CACHE_CONTROL_TYPE_EPHEMERAL)
}

// GetChatMessageResponse decoder (exa.api_server_pb.GetChatMessageResponse)

// message GetChatMessageResponse {
//   string message_id = 1;
//   string delta_text = 3;
//   .exa.codeium_common_pb.StopReason stop_reason = 5;
//   repeated .exa.codeium_common_pb.ChatToolCall delta_tool_calls = 6;
//   .exa.codeium_common_pb.ModelUsageStats usage = 7;
//   string delta_thinking = 9;
//   string delta_signature = 10;
// }
#[derive(Debug, Default)]
pub struct GetChatMessageResponse {
    pub message_id: String,
    pub delta_text: String,
    pub delta_thinking: String,
    pub delta_signature: String,
    pub stop_reason: u32,
    pub usage: Option<ModelUsageStats>,
    pub delta_tool_calls: Vec<ChatToolCall>,
}

#[derive(Debug, Default)]
#[allow(clippy::struct_field_names)]
pub struct ModelUsageStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Default, Clone)]
pub struct ChatToolCall {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
}

pub fn decode_get_chat_message_response(buf: &[u8]) -> Result<GetChatMessageResponse, String> {
    let mut response = GetChatMessageResponse::default();
    for field in iter_fields(buf) {
        let (num, _wire, data) = field?;
        match num {
            1 => {
                response.message_id = String::from_utf8_lossy(data).into_owned();
            }
            3 => {
                response.delta_text = String::from_utf8_lossy(data).into_owned();
            }
            5 => {
                // stop_reason is an enum, stored as varint
                if let Ok((value, _)) = decode_varint(data) {
                    response.stop_reason = u32::try_from(value).map_or(0, std::convert::identity);
                }
            }
            6 => {
                // delta_tool_calls is repeated
                if let Ok(tc) = decode_chat_tool_call(data) {
                    response.delta_tool_calls.push(tc);
                }
            }
            7 => {
                response.usage = Some(decode_model_usage_stats(data)?);
            }
            9 => {
                response.delta_thinking = String::from_utf8_lossy(data).into_owned();
            }
            10 => {
                response.delta_signature = String::from_utf8_lossy(data).into_owned();
            }
            _ => {}
        }
    }
    Ok(response)
}

fn decode_chat_tool_call(buf: &[u8]) -> Result<ChatToolCall, String> {
    let mut tool_call = ChatToolCall::default();
    for field in iter_fields(buf) {
        let (num, _wire, data) = field?;
        match num {
            1 => {
                tool_call.id = String::from_utf8_lossy(data).into_owned();
            }
            2 => {
                tool_call.name = String::from_utf8_lossy(data).into_owned();
            }
            3 => {
                tool_call.arguments_json = String::from_utf8_lossy(data).into_owned();
            }
            _ => {}
        }
    }
    Ok(tool_call)
}

fn decode_model_usage_stats(buf: &[u8]) -> Result<ModelUsageStats, String> {
    let mut stats = ModelUsageStats::default();
    for field in iter_fields(buf) {
        let (num, _wire, data) = field?;
        match num {
            1 => {
                if let Ok((value, _)) = decode_varint(data) {
                    stats.input_tokens = value;
                }
            }
            2 => {
                if let Ok((value, _)) = decode_varint(data) {
                    stats.output_tokens = value;
                }
            }
            3 => {
                if let Ok((value, _)) = decode_varint(data) {
                    stats.cache_read_tokens = value;
                }
            }
            4 => {
                if let Ok((value, _)) = decode_varint(data) {
                    stats.cache_write_tokens = value;
                }
            }
            _ => {}
        }
    }
    Ok(stats)
}

// GetCliModelConfigsRequest encoder (exa.api_server_pb.GetCliModelConfigsRequest)
//
// message GetCliModelConfigsRequest {
//   .exa.codeium_common_pb.Metadata metadata = 1;
// }
#[must_use]
pub fn encode_get_cli_model_configs_request(api_key: &str) -> Vec<u8> {
    let metadata = encode_metadata(api_key);
    field_ld(1, &metadata)
}

// Parses a GetCliModelConfigsResponse into a map from display model id to wire model uid.
//
// ClientModelConfig:
//   string label = 1;
//   ModelOrAlias model_or_alias = 2;
//   string model_uid = 22;
pub fn decode_cli_model_configs(
    buf: &[u8],
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut map = std::collections::HashMap::new();
    for field in iter_fields(buf) {
        let (num, _wire, data) = field?;
        if num != 1 {
            continue;
        }
        // data is a ClientModelConfig message
        let mut label = String::new();
        let mut model_uid_22 = String::new();
        let mut wire_uid = String::new();
        for inner in iter_fields(data) {
            let (inner_num, _inner_wire, inner_data) = inner?;
            match inner_num {
                1 => label = String::from_utf8_lossy(inner_data).into_owned(),
                2 => {
                    // model_or_alias is a ModelOrAlias message
                    for alias_field in iter_fields(inner_data) {
                        let (alias_num, _alias_wire, alias_data) = alias_field?;
                        if alias_num == 3 {
                            wire_uid = String::from_utf8_lossy(alias_data).into_owned();
                        }
                    }
                }
                22 => model_uid_22 = String::from_utf8_lossy(inner_data).into_owned(),
                _ => {}
            }
        }
        let display = if model_uid_22.is_empty() {
            label.clone()
        } else {
            model_uid_22.clone()
        };
        let wire = if wire_uid.is_empty() {
            display.clone()
        } else {
            wire_uid
        };
        if !display.is_empty() {
            map.insert(display, wire.clone());
        }
        // also map by label if different
        if !label.is_empty() && !model_uid_22.is_empty() && label != model_uid_22 {
            map.insert(label, wire);
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_varint_single_byte() {
        let mut out = Vec::new();
        encode_varint(127, &mut out);
        assert_eq!(out, vec![127]);
    }

    #[test]
    fn encode_varint_multi_byte() {
        let mut out = Vec::new();
        encode_varint(300, &mut out);
        assert_eq!(out, vec![0b1010_1100, 0b0000_0010]);
    }

    #[test]
    fn field_str_roundtrip() {
        let encoded = field_str(1, "hello");
        for field in iter_fields(&encoded) {
            let (num, _wire, data) = field.unwrap();
            assert_eq!(num, 1);
            assert_eq!(String::from_utf8_lossy(data), "hello");
        }
    }

    #[test]
    fn decode_get_user_jwt_response_roundtrip() {
        let mut encoded = field_str(1, "test_jwt");
        encoded.extend_from_slice(&field_str(2, "https://custom.com"));
        let response = decode_get_user_jwt_response(&encoded).unwrap();
        assert_eq!(response.user_jwt, "test_jwt");
        assert_eq!(response.custom_api_server_url, "https://custom.com");
    }
}
