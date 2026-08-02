//! Hand-written protobuf schema for Cursor `agent.v1` Connect messages.
//!
//! We derive `prost::Message` instead of using `prost-build` because upstream
//! `.proto` files are not redistributable. Field numbers are reverse-engineered
//! from `cursor-agent` 2026.07.26 and validated against the MIT-licensed shunt
//! `agent.rs` frame builder.

#![allow(dead_code)]

use prost::Message;

use crate::providers::cursor::connect::encode_frame;

/// `AgentMode`: `AGENT` = 1, `ASK` = 2, `PLAN` = 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
#[repr(i32)]
pub(crate) enum AgentMode {
    Agent = 1,
    Ask = 2,
    Plan = 3,
}

pub(crate) const AGENT_MODE_AGENT: u64 = AgentMode::Agent as u64;
pub(crate) const AGENT_MODE_ASK: u64 = AgentMode::Ask as u64;

/// Model metadata: `{f1: model_id, f3: {f1:"fast", f2:"false"}}`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ModelMeta {
    #[prost(string, required, tag = "1")]
    pub model_id: String,
    #[prost(message, optional, tag = "3")]
    pub options: Option<ModelOptions>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ModelOptions {
    #[prost(string, required, tag = "1")]
    pub key: String,
    #[prost(string, required, tag = "2")]
    pub value: String,
}

impl ModelMeta {
    pub(crate) fn new(model_id: &str) -> Self {
        Self {
            model_id: model_id.to_string(),
            options: Some(ModelOptions {
                key: "fast".to_string(),
                value: "false".to_string(),
            }),
        }
    }
}

/// User message in the conversation.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct UserMessage {
    #[prost(string, required, tag = "1")]
    pub prompt: String,
    #[prost(string, required, tag = "2")]
    pub message_id: String,
    #[prost(string, required, tag = "3")]
    pub parent_id: String,
    #[prost(uint64, required, tag = "4")]
    pub mode: u64,
}

/// Action wrapper for user message.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Action {
    #[prost(message, optional, tag = "1")]
    pub user_message: Option<UserMessage>,
}

/// Environment context for the agent.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Environment {
    #[prost(string, required, tag = "1")]
    pub os: String,
    #[prost(string, required, tag = "2")]
    pub cwd: String,
    #[prost(string, required, tag = "3")]
    pub shell: String,
    #[prost(string, required, tag = "10")]
    pub timezone: String,
    #[prost(string, required, tag = "11")]
    pub home: String,
    #[prost(uint64, required, tag = "14")]
    pub field_14: u64,
    #[prost(uint64, required, tag = "16")]
    pub field_16: u64,
    #[prost(uint64, required, tag = "19")]
    pub field_19: u64,
    #[prost(uint64, required, tag = "20")]
    pub field_20: u64,
    #[prost(string, required, tag = "21")]
    pub project_path: String,
    #[prost(uint64, required, tag = "22")]
    pub field_22: u64,
}

impl Environment {
    pub(crate) fn new(cwd: &str) -> Self {
        Self {
            os: "linux".to_string(),
            cwd: cwd.to_string(),
            shell: "bash".to_string(),
            timezone: "UTC".to_string(),
            home: cwd.to_string(),
            field_14: 1,
            field_16: 1,
            field_19: 0,
            field_20: 0,
            project_path: cwd.to_string(),
            field_22: 0,
        }
    }
}

/// Context wrapper for environment.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Context {
    #[prost(message, optional, tag = "1")]
    pub environment: Option<Environment>,
}

/// Session metadata wrapper.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct SessionMeta {
    #[prost(message, optional, tag = "1")]
    pub context: Option<Context>,
}

/// Agent client message sent to the server.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AgentClientMessage {
    #[prost(string, required, tag = "1")]
    pub field_1: String,
    #[prost(message, optional, tag = "2")]
    pub action: Option<Action>,
    #[prost(string, required, tag = "4")]
    pub mcp_tools: String,
    #[prost(string, required, tag = "5")]
    pub conversation_id: String,
    #[prost(message, optional, tag = "9")]
    pub model_meta: Option<ModelMeta>,
    #[prost(uint64, required, tag = "12")]
    pub field_12: u64,
    #[prost(message, repeated, tag = "14")]
    pub environments: Vec<ModelMeta>,
    #[prost(string, required, tag = "16")]
    pub conversation_id_2: String,
}

/// Marker message for pacing.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct MarkerMessage {
    #[prost(uint64, required, tag = "1")]
    pub index: u64,
    #[prost(string, required, tag = "3")]
    pub field_3: String,
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
    let user_message = UserMessage {
        prompt: params.prompt.to_string(),
        message_id: params.message_id.to_string(),
        parent_id: String::new(),
        mode: params.mode,
    };
    let action = Action {
        user_message: Some(user_message),
    };

    let model_meta = ModelMeta::new(params.model_id);
    let environments = vec![
        ModelMeta {
            model_id: "default".to_string(),
            options: None,
        },
        model_meta.clone(),
    ];

    let client_msg = AgentClientMessage {
        field_1: String::new(),
        action: Some(action),
        mcp_tools: String::new(),
        conversation_id: params.conversation_id.to_string(),
        model_meta: Some(model_meta),
        field_12: 0,
        environments,
        conversation_id_2: params.conversation_id.to_string(),
    };

    let run_frame = encode_frame(0, &client_msg.encode_to_vec())?;

    let environment = Environment::new(params.cwd);
    let context = Context {
        environment: Some(environment),
    };
    let session_meta = SessionMeta {
        context: Some(context),
    };
    let env_frame = encode_frame(0, &session_meta.encode_to_vec())?;

    let mut out = vec![run_frame, env_frame];
    out.push(encode_frame(
        0,
        &AgentClientMessage::default().encode_to_vec(),
    )?);
    out.push(encode_frame(
        0,
        &AgentClientMessage::default().encode_to_vec(),
    )?);
    for n in 1..=8u64 {
        let marker = MarkerMessage {
            index: n,
            field_3: String::new(),
        };
        out.push(encode_frame(0, &marker.encode_to_vec())?);
    }
    Ok(out)
}

/// `AgentClientMessage.client_heartbeat` (field 7) empty message.
pub(crate) fn heartbeat_frame() -> Result<Vec<u8>, String> {
    // Field 7, wire type 2 (length-delimited), length 0
    let tag = (7 << 3) | 2;
    let mut buf = Vec::new();
    prost::encoding::encode_varint(tag, &mut buf);
    prost::encoding::encode_varint(0u64, &mut buf);
    encode_frame(0, &buf)
}

/// Text delta from the server.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct TextDelta {
    #[prost(string, tag = "1")]
    pub text: String,
}

/// Thinking delta from the server.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ThinkingDelta {
    #[prost(string, tag = "1")]
    pub text: String,
}

/// Interaction update from the server.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct InteractionUpdate {
    #[prost(message, optional, tag = "1")]
    pub text_delta: Option<TextDelta>,
    #[prost(message, optional, tag = "4")]
    pub thinking_delta: Option<ThinkingDelta>,
}

/// Agent server message received from the server.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AgentServerMessage {
    #[prost(message, optional, tag = "1")]
    pub interaction_update: Option<InteractionUpdate>,
    #[prost(bytes, tag = "2")]
    pub exec_server_message: Vec<u8>,
    #[prost(bytes, tag = "3")]
    pub field_3: Vec<u8>,
    #[prost(bytes, tag = "4")]
    pub kv_server_message: Vec<u8>,
}

/// Get blob arguments from server.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetBlobArgs {
    #[prost(bytes, tag = "1")]
    pub blob_id: Vec<u8>,
}

/// Set blob arguments from server.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct SetBlobArgs {
    #[prost(bytes, tag = "1")]
    pub blob_id: Vec<u8>,
    #[prost(bytes, tag = "2")]
    pub blob_data: Vec<u8>,
}

/// KV server message wrapper.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct KvServerMessage {
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(message, optional, tag = "2")]
    pub get_blob: Option<GetBlobArgs>,
    #[prost(message, optional, tag = "3")]
    pub set_blob: Option<SetBlobArgs>,
}

/// Get blob result from client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetBlobResult {
    #[prost(bytes, tag = "1")]
    pub blob_data: Vec<u8>,
}

/// Set blob result from client.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct SetBlobResult {
    #[prost(string, tag = "1")]
    pub error: String,
}

/// KV client message wrapper.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct KvClientMessage {
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(message, optional, tag = "2")]
    pub get_blob_result: Option<GetBlobResult>,
    #[prost(message, optional, tag = "3")]
    pub set_blob_result: Option<SetBlobResult>,
}

/// Extract assistant text deltas from an `AgentServerMessage` payload.
pub(crate) fn extract_text_deltas(payload: &[u8]) -> Result<Vec<String>, String> {
    let msg = AgentServerMessage::decode(payload).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(update) = msg.interaction_update
        && let Some(delta) = update.text_delta
        && !delta.text.is_empty()
    {
        out.push(delta.text);
    }
    Ok(out)
}

/// Extract thinking deltas from an `AgentServerMessage` payload.
pub(crate) fn extract_thinking_deltas(payload: &[u8]) -> Result<Vec<String>, String> {
    let msg = AgentServerMessage::decode(payload).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(update) = msg.interaction_update
        && let Some(delta) = update.thinking_delta
        && !delta.text.is_empty()
    {
        out.push(delta.text);
    }
    Ok(out)
}

/// True when the server asked for an MCP tool via `exec_server_message` (field 2).
pub(crate) fn has_exec_server_message(payload: &[u8]) -> Result<bool, String> {
    let msg = AgentServerMessage::decode(payload).map_err(|e| e.to_string())?;
    Ok(!msg.exec_server_message.is_empty())
}

/// True when the server sent a KV get/set request (`kv_server_message` field 4).
#[allow(dead_code)] // used when Run wires checkpoint replies (Phase 1)
pub(crate) fn has_kv_server_message(payload: &[u8]) -> Result<bool, String> {
    let msg = AgentServerMessage::decode(payload).map_err(|e| e.to_string())?;
    Ok(!msg.kv_server_message.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::cursor::connect::FrameBuffer;
    use crate::providers::proto_test_util::parse_wire_fields;

    #[test]
    fn heartbeat_is_single_connect_frame_with_field_7() {
        let frame = heartbeat_frame().unwrap();
        let mut buf = FrameBuffer::default();
        buf.push(&frame);
        let decoded = buf.next_frame().expect("frame").expect("ok");
        assert!(!decoded.end_stream);
        // Field 7 with empty length-delimited payload
        assert_eq!(decoded.payload[0] >> 3, 7);
    }

    #[test]
    fn encode_model_meta_includes_fast_flag() {
        let meta = ModelMeta::new("default");
        let encoded = meta.encode_to_vec();
        let hay = String::from_utf8_lossy(&encoded);
        assert!(hay.contains("default"));
        assert!(hay.contains("fast"));
        assert!(hay.contains("false"));
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
        .expect("frames");
        assert!(frames.len() >= 4);
        let joined: Vec<u8> = frames.iter().flat_map(|f| f.iter().copied()).collect();
        let hay = String::from_utf8_lossy(&joined);
        assert!(hay.contains("PROMPT_MARKER"));
        assert!(hay.contains("default"));
        assert!(hay.contains("conv-1"));
        assert!(hay.contains("/tmp"));
        assert!(hay.contains("fast"));
    }

    #[test]
    fn extract_text_deltas_from_nested_update() {
        let msg = AgentServerMessage {
            interaction_update: Some(InteractionUpdate {
                text_delta: Some(TextDelta {
                    text: "hi".to_string(),
                }),
                thinking_delta: None,
            }),
            exec_server_message: Vec::new(),
            field_3: Vec::new(),
            kv_server_message: Vec::new(),
        };
        let payload = msg.encode_to_vec();
        assert_eq!(
            extract_text_deltas(&payload).expect("ok"),
            vec!["hi".to_string()]
        );
    }

    #[test]
    fn has_exec_server_message_detects_non_empty_field_2() {
        let msg = AgentServerMessage {
            interaction_update: None,
            exec_server_message: b"exec_data".to_vec(),
            field_3: Vec::new(),
            kv_server_message: Vec::new(),
        };
        let payload = msg.encode_to_vec();
        assert!(has_exec_server_message(&payload).expect("ok"));
        // Empty field 2 must not false-positive
        let msg_empty = AgentServerMessage {
            interaction_update: None,
            exec_server_message: Vec::new(),
            field_3: Vec::new(),
            kv_server_message: Vec::new(),
        };
        assert!(!has_exec_server_message(&msg_empty.encode_to_vec()).expect("ok"));
    }

    #[test]
    fn build_run_frames_agent_client_message_wire_format() {
        let frames = build_run_frames(&RunFrameParams {
            prompt: "PROMPT_MARKER",
            model_id: "default",
            cwd: "/tmp",
            conversation_id: "conv-1",
            message_id: "msg-1",
            mode: AGENT_MODE_AGENT,
        })
        .expect("frames");

        let mut buf = FrameBuffer::default();
        buf.push(&frames[0]);
        let frame = buf.next_frame().expect("frame").expect("ok");
        let client_fields = parse_wire_fields(&frame.payload).expect("valid client message");
        assert_eq!(
            client_fields.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1, 2, 4, 5, 9, 12, 14, 14, 16]
        );

        let action = parse_wire_fields(client_fields[1].as_bytes().unwrap()).expect("action");
        assert_eq!(action[0].number, 1);
        let user_msg = parse_wire_fields(action[0].as_bytes().unwrap()).expect("user message");
        assert_eq!(
            user_msg.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );

        let model_meta =
            parse_wire_fields(client_fields[4].as_bytes().unwrap()).expect("model meta");
        assert_eq!(
            model_meta.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1, 3]
        );

        let first_env = parse_wire_fields(client_fields[6].as_bytes().unwrap()).expect("first env");
        assert_eq!(
            first_env.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1]
        );
        let second_env =
            parse_wire_fields(client_fields[7].as_bytes().unwrap()).expect("second env");
        assert_eq!(
            second_env.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn environment_includes_zero_fields() {
        let env = Environment::new("/tmp");
        let fields = parse_wire_fields(&env.encode_to_vec()).expect("valid environment");
        assert_eq!(
            fields.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1, 2, 3, 10, 11, 14, 16, 19, 20, 21, 22]
        );
        assert_eq!(fields[7].as_varint(), Some(0));
        assert_eq!(fields[8].as_varint(), Some(0));
        assert_eq!(fields[10].as_varint(), Some(0));
    }

    #[test]
    fn marker_message_includes_empty_field_3() {
        let marker = MarkerMessage {
            index: 1,
            field_3: String::new(),
        };
        let fields = parse_wire_fields(&marker.encode_to_vec()).expect("valid marker");
        assert_eq!(
            fields.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(fields[1].as_string().as_deref(), Some(""));
    }

    #[test]
    fn agent_client_message_default_encodes_required_fields() {
        let fields = parse_wire_fields(&AgentClientMessage::default().encode_to_vec())
            .expect("valid default client message");
        assert_eq!(
            fields.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1, 4, 5, 12, 16]
        );
    }

    #[test]
    fn decode_agent_server_message_with_unknown_field() {
        let mut buf = AgentServerMessage {
            interaction_update: None,
            exec_server_message: Vec::new(),
            field_3: Vec::new(),
            kv_server_message: Vec::new(),
        }
        .encode_to_vec();
        let mut unknown = Vec::new();
        prost::encoding::encode_varint((999 << 3) | 2, &mut unknown);
        prost::encoding::encode_varint(0u64, &mut unknown);
        buf.extend(unknown);
        let decoded = AgentServerMessage::decode(&buf[..]).expect("decode with unknown");
        assert!(decoded.interaction_update.is_none());
    }

    #[test]
    fn decode_rejects_truncated_message() {
        let mut buf = AgentServerMessage {
            interaction_update: Some(InteractionUpdate {
                text_delta: Some(TextDelta {
                    text: "hi".to_string(),
                }),
                thinking_delta: None,
            }),
            exec_server_message: Vec::new(),
            field_3: Vec::new(),
            kv_server_message: Vec::new(),
        }
        .encode_to_vec();
        buf.pop();
        assert!(AgentServerMessage::decode(&buf[..]).is_err());
    }
}
