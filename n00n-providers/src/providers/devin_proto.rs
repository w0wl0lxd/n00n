//! Hand-rolled protobuf helpers for Devin Connect messages.
//!
//! Based on the proto definitions from:
//! - exa/api_server_pb/api_server.proto
//! - exa/auth_pb/auth.proto
//! - exa/chat_pb/chat.proto
//! - exa/codeium_common_pb/codeium_common.proto

use std::io::Read;

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
    encode_varint(data.len() as u64, &mut out);
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

#[must_use]
pub fn field_bytes(field: u64, value: &[u8]) -> Vec<u8> {
    field_ld(field, value)
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

/// Decode length-delimited fields from a protobuf message body.
pub fn iter_fields(mut buf: &[u8]) -> impl Iterator<Item = Result<(u64, u8, &[u8]), String>> + '_ {
    std::iter::from_fn(move || {
        while !buf.is_empty() {
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
                    buf = rest;
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
                other => return Some(Err(format!("unsupported protobuf wire type {other}"))),
            }
        }
        None
    })
}

/// GetUserJwtRequest encoder (exa.auth_pb.GetUserJwtRequest)
///
/// message GetUserJwtRequest {
///   .exa.codeium_common_pb.Metadata metadata = 1;
/// }
pub fn encode_get_user_jwt_request(api_key: &str) -> Vec<u8> {
    let metadata = encode_metadata(api_key);
    field_ld(1, &metadata)
}

/// Metadata encoder (exa.codeium_common_pb.Metadata)
///
/// message Metadata {
///   string api_key = 1;
///   string ide_name = 2;
///   string ide_version = 3;
///   string extension_name = 4;
///   string extension_version = 5;
///   string locale = 6;
///   string user_jwt = 7;
/// }
fn encode_metadata(api_key: &str) -> Vec<u8> {
    let mut out = field_str(1, api_key);
    out.extend(field_str(2, "windsurf"));
    out.extend(field_str(3, "3.2.23"));
    out.extend(field_str(4, "windsurf"));
    out.extend(field_str(5, "1.48.2"));
    out.extend(field_str(6, "en"));
    out
}

/// GetUserJwtResponse decoder (exa.auth_pb.GetUserJwtResponse)
///
/// message GetUserJwtResponse {
///   string user_jwt = 1;
///   string custom_api_server_url = 2;
/// }
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

/// GetChatMessageRequest encoder (exa.api_server_pb.GetChatMessageRequest)
///
/// This is a simplified encoder for the basic fields needed for a chat request.
pub fn encode_get_chat_message_request(
    api_key: &str,
    user_jwt: &str,
    prompt: &str,
    model_uid: &str,
    cascade_id: &str,
) -> Vec<u8> {
    let mut out = Vec::new();

    // metadata (field 1)
    let mut metadata = encode_metadata(api_key);
    metadata.extend(field_str(7, user_jwt));
    out.extend(field_ld(1, &metadata));

    // prompt (field 2)
    out.extend(field_str(2, prompt));

    // chat_model_uid (field 21)
    out.extend(field_str(21, model_uid));

    // cascade_id (field 16)
    out.extend(field_str(16, cascade_id));

    // request_type = CASCADE (field 7, value 5)
    out.extend(field_varint(7, 5));

    out
}

/// GetChatMessageResponse decoder (exa.api_server_pb.GetChatMessageResponse)
///
/// message GetChatMessageResponse {
///   string message_id = 1;
///   string delta_text = 3;
///   .exa.codeium_common_pb.StopReason stop_reason = 5;
///   repeated .exa.codeium_common_pb.ChatToolCall delta_tool_calls = 6;
///   .exa.codeium_common_pb.ModelUsageStats usage = 7;
///   string delta_thinking = 9;
///   string delta_signature = 10;
/// }
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
                    response.stop_reason = value as u32;
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
        assert_eq!(out, vec![0b10101100, 0b00000010]);
    }

    #[test]
    fn field_str_roundtrip() {
        let encoded = field_str(1, "hello");
        let mut buf = Vec::new();
        for field in iter_fields(&encoded) {
            let (num, _wire, data) = field.unwrap();
            assert_eq!(num, 1);
            assert_eq!(String::from_utf8_lossy(data), "hello");
        }
    }

    #[test]
    fn decode_get_user_jwt_response() {
        let encoded = field_str(1, "test_jwt") + &field_str(2, "https://custom.com");
        let response = decode_get_user_jwt_response(&encoded).unwrap();
        assert_eq!(response.user_jwt, "test_jwt");
        assert_eq!(response.custom_api_server_url, "https://custom.com");
    }
}
