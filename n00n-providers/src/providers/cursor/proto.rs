//! Hand-rolled protobuf helpers for Cursor `agent.v1` Connect messages.
//!
//! Field layout reverse-engineered from `cursor-agent` 2026.07.26 and validated
//! against the MIT-licensed shunt `agent.rs` frame builder.
//!
//! This module is not yet wired into the Cursor provider; it's prepared for
//! future native integration to replace the cursor-agent subprocess approach.

#![allow(dead_code)]

use crate::providers::cursor::connect::encode_frame;

/// `AgentMode`: `AGENT` = 1, `ASK` = 2, `PLAN` = 3.
pub(crate) const AGENT_MODE_AGENT: u64 = 1;

// Protobuf field numbers for AgentService messages
pub(crate) const FIELD_USER_MESSAGE: u64 = 1;
pub(crate) const FIELD_ACTION: u64 = 2;
pub(crate) const FIELD_EMPTY_STRING: u64 = 3;
pub(crate) const FIELD_MODE: u64 = 4;
pub(crate) const FIELD_CONVERSATION_ID: u64 = 5;
pub(crate) const FIELD_CLIENT_HEARTBEAT: u64 = 7;
pub(crate) const FIELD_MODEL_META: u64 = 9;
pub(crate) const FIELD_ENV_CONTEXT: u64 = 10;
pub(crate) const FIELD_CWD: u64 = 11;
pub(crate) const FIELD_REQUEST_ID: u64 = 12;
pub(crate) const FIELD_DEFAULT_MODEL: u64 = 14;
pub(crate) const FIELD_ENV_FLAGS: u64 = 16;
pub(crate) const FIELD_INTERACTION_UPDATE: u64 = 1;
pub(crate) const FIELD_TEXT_DELTA: u64 = 1;
pub(crate) const FIELD_THINKING_DELTA: u64 = 4;
pub(crate) const FIELD_EXEC_SERVER_MESSAGE: u64 = 2;
pub(crate) const FIELD_KV_SERVER_MESSAGE: u64 = 4;
pub(crate) const FIELD_MCP_TOOLS: u64 = 4;

pub(crate) fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
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
pub(crate) fn field_ld(field: u64, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 10);
    encode_varint((field << 3) | 2, &mut out);
    encode_varint(data.len() as u64, &mut out);
    out.extend_from_slice(data);
    out
}

#[must_use]
pub(crate) fn field_str(field: u64, value: &str) -> Vec<u8> {
    field_ld(field, value.as_bytes())
}

#[must_use]
pub(crate) fn field_varint(field: u64, value: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    encode_varint(field << 3, &mut out);
    encode_varint(value, &mut out);
    out
}

#[must_use]
pub(crate) fn field_bytes(field: u64, value: &[u8]) -> Vec<u8> {
    field_ld(field, value)
}

#[must_use]
pub(crate) fn encode_model_meta(model_id: &str) -> Vec<u8> {
    field_str(FIELD_USER_MESSAGE, model_id)
}

/// Empty `mcp_tools` encodes as an empty length-delimited field (same as `field_str(FIELD_MCP_TOOLS, "")`).
#[must_use]
pub(crate) fn encode_empty_mcp_tools_field() -> Vec<u8> {
    field_str(FIELD_MCP_TOOLS, "")
}

#[derive(Debug, Clone)]
pub(crate) struct RunFrameParams<'a> {
    pub prompt: &'a str,
    pub model_id: &'a str,
    pub cwd: &'a str,
    pub conversation_id: &'a str,
    pub message_id: &'a str,
    pub mode: u64,
}

/// Build the paced Connect frames for one `AgentService/Run` turn (no heartbeats).
pub(crate) fn build_run_frames(params: &RunFrameParams<'_>) -> Result<Vec<Vec<u8>>, String> {
    let mut user = field_str(FIELD_USER_MESSAGE, params.prompt);
    user.extend(field_str(2, params.message_id));
    user.extend(field_str(FIELD_EMPTY_STRING, ""));
    user.extend(field_varint(FIELD_MODE, params.mode));
    let action = field_ld(
        FIELD_ACTION,
        &field_ld(FIELD_USER_MESSAGE, &field_ld(FIELD_USER_MESSAGE, &user)),
    );

    let mut req = field_str(FIELD_USER_MESSAGE, "");
    req.extend(action);
    req.extend(encode_empty_mcp_tools_field());
    req.extend(field_str(FIELD_CONVERSATION_ID, params.conversation_id));
    req.extend(field_ld(
        FIELD_MODEL_META,
        &encode_model_meta(params.model_id),
    ));
    req.extend(field_varint(FIELD_REQUEST_ID, 0));
    req.extend(field_ld(
        FIELD_DEFAULT_MODEL,
        &field_str(FIELD_USER_MESSAGE, "default"),
    ));
    req.extend(field_ld(
        FIELD_DEFAULT_MODEL,
        &encode_model_meta(params.model_id),
    ));
    req.extend(field_str(FIELD_ENV_FLAGS, params.conversation_id));
    let run_frame = encode_frame(0, &field_ld(FIELD_USER_MESSAGE, &req))?;

    let mut env = field_str(FIELD_USER_MESSAGE, "linux");
    env.extend(field_str(2, params.cwd));
    env.extend(field_str(FIELD_EMPTY_STRING, "bash"));
    env.extend(field_str(FIELD_ENV_CONTEXT, "UTC"));
    env.extend(field_str(FIELD_CWD, params.cwd));
    env.extend(field_varint(FIELD_DEFAULT_MODEL, 1));
    env.extend(field_varint(FIELD_ENV_FLAGS, 1));
    env.extend(field_varint(19, 0));
    env.extend(field_varint(20, 0));
    env.extend(field_str(21, params.cwd));
    env.extend(field_varint(22, 0));
    let ctx = field_ld(
        FIELD_ACTION,
        &field_ld(
            FIELD_ENV_CONTEXT,
            &field_ld(
                FIELD_USER_MESSAGE,
                &field_ld(FIELD_USER_MESSAGE, &field_ld(FIELD_MCP_TOOLS, &env)),
            ),
        ),
    );
    let env_frame = encode_frame(0, &ctx)?;

    let mut out = vec![run_frame, env_frame];
    out.push(encode_frame(
        0,
        &field_ld(FIELD_CONVERSATION_ID, &field_str(FIELD_USER_MESSAGE, "")),
    )?);
    out.push(encode_frame(
        0,
        &field_ld(FIELD_EMPTY_STRING, &field_str(FIELD_EMPTY_STRING, "")),
    )?);
    for n in 1..=8u64 {
        let mut marker = field_varint(FIELD_USER_MESSAGE, n);
        marker.extend(field_str(FIELD_EMPTY_STRING, ""));
        out.push(encode_frame(0, &field_ld(FIELD_EMPTY_STRING, &marker))?);
    }
    Ok(out)
}

/// `AgentClientMessage.client_heartbeat` (field 7) empty message.
pub(crate) fn heartbeat_frame() -> Result<Vec<u8>, String> {
    encode_frame(0, &field_ld(FIELD_CLIENT_HEARTBEAT, &[]))
}

/// Decode fields from a protobuf message body.
///
/// For length-delimited fields (wire type 2) the returned slice is the field
/// payload. For varint fields (wire type 0) the returned slice is the varint
/// value bytes; callers can decode it with `decode_varint`.
pub(crate) fn iter_fields(
    mut buf: &[u8],
) -> impl Iterator<Item = Result<(u64, u8, &[u8]), String>> + '_ {
    std::iter::from_fn(move || {
        if buf.is_empty() {
            return None;
        }
        let (tag, rest) = match decode_varint(buf) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        buf = rest;
        let field = tag >> 3;
        let wire = (tag & 7) as u8;
        match wire {
            0 => {
                let value_start = buf;
                let (_, rest) = match decode_varint(buf) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let value_bytes = &value_start[..value_start.len() - rest.len()];
                buf = rest;
                Some(Ok((field, wire, value_bytes)))
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
                Some(Ok((field, wire, data)))
            }
            other => Some(Err(format!("unsupported protobuf wire type {other}"))),
        }
    })
}

pub(crate) fn decode_varint(buf: &[u8]) -> Result<(u64, &[u8]), String> {
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

/// Extract assistant text deltas from an `AgentServerMessage` payload.
/// Path: `interaction_update` (f1) → `text_delta` (f1) → text (f1).
pub(crate) fn extract_text_deltas(payload: &[u8]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for field in iter_fields(payload) {
        let (num, wire, data) = field?;
        if num == FIELD_INTERACTION_UPDATE && wire == 2 {
            for update in iter_fields(data) {
                let (unum, uwire, udata) = update?;
                if unum == FIELD_TEXT_DELTA && uwire == 2 {
                    for text_field in iter_fields(udata) {
                        let (tnum, twire, tdata) = text_field?;
                        if tnum == FIELD_TEXT_DELTA && twire == 2 {
                            out.push(String::from_utf8_lossy(tdata).into_owned());
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Extract thinking deltas: `interaction_update` (f1) → `thinking_delta` (f4) → text (f1).
pub(crate) fn extract_thinking_deltas(payload: &[u8]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for field in iter_fields(payload) {
        let (num, wire, data) = field?;
        if num == FIELD_INTERACTION_UPDATE && wire == 2 {
            for update in iter_fields(data) {
                let (unum, uwire, udata) = update?;
                if unum == FIELD_THINKING_DELTA && uwire == 2 {
                    for text_field in iter_fields(udata) {
                        let (tnum, twire, tdata) = text_field?;
                        if tnum == FIELD_TEXT_DELTA && twire == 2 {
                            out.push(String::from_utf8_lossy(tdata).into_owned());
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

/// True when the server asked the client to run a tool (`exec_server_message` f2).
pub(crate) fn has_exec_server_message(payload: &[u8]) -> Result<bool, String> {
    for field in iter_fields(payload) {
        let (num, wire, _) = field?;
        if num == FIELD_EXEC_SERVER_MESSAGE && wire == 2 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// True when the server sent a KV get/set request (`kv_server_message` f4).
pub(crate) fn has_kv_server_message(payload: &[u8]) -> Result<bool, String> {
    for field in iter_fields(payload) {
        let (num, wire, _) = field?;
        if num == FIELD_KV_SERVER_MESSAGE && wire == 2 {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::cursor::connect::FrameBuffer;

    #[test]
    fn empty_mcp_tools_matches_empty_string_field() {
        assert_eq!(
            encode_empty_mcp_tools_field(),
            field_str(FIELD_MCP_TOOLS, "")
        );
    }

    #[test]
    fn heartbeat_is_single_connect_frame_with_field_7() {
        let frame = heartbeat_frame().expect("encode");
        let mut buf = FrameBuffer::default();
        buf.push(&frame);
        let decoded = buf.next_frame().expect("frame").expect("ok");
        assert!(!decoded.end_stream);
        assert_eq!(decoded.payload, field_ld(FIELD_CLIENT_HEARTBEAT, &[]));
    }

    #[test]
    fn build_run_frames_embeds_prompt_and_default_model() {
        let frames = build_run_frames(&RunFrameParams {
            prompt: "PROMPT_MARKER",
            model_id: "default",
            cwd: "/tmp",
            conversation_id: "conv-1",
            message_id: "msg-1",
            mode: AGENT_MODE_AGENT,
        })
        .expect("encode");
        assert!(frames.len() >= 4);
        let joined: Vec<u8> = frames.iter().flat_map(|f| f.iter().copied()).collect();
        let hay = String::from_utf8_lossy(&joined);
        assert!(hay.contains("PROMPT_MARKER"));
        assert!(hay.contains("default"));
        assert!(hay.contains("conv-1"));
        assert!(hay.contains("/tmp"));
    }

    #[test]
    fn extract_text_deltas_from_nested_update() {
        // AgentServerMessage { interaction_update { text_delta { text: "hi" } } }
        let text_delta = field_str(FIELD_TEXT_DELTA, "hi");
        let interaction = field_ld(FIELD_INTERACTION_UPDATE, &text_delta);
        let msg = field_ld(FIELD_INTERACTION_UPDATE, &interaction);
        assert_eq!(
            extract_text_deltas(&msg).expect("ok"),
            vec!["hi".to_string()]
        );
    }

    #[test]
    fn has_exec_server_message_detects_field_2() {
        let payload = field_ld(FIELD_EXEC_SERVER_MESSAGE, b"tool");
        assert!(has_exec_server_message(&payload).expect("ok"));
        assert!(!has_exec_server_message(&field_ld(FIELD_INTERACTION_UPDATE, b"x")).expect("ok"));
    }
}
