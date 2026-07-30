//! Native Devin provider using Connect protocol over gRPC-Web.
//!
//! Protocol:
//! 1. Read `~/.local/share/devin/credentials.toml` or env for session token
//! 2. Call `POST /exa.auth_pb.AuthService/GetUserJwt` (application/proto) to get user JWT
//! 3. Call `POST /exa.api_server_pb.ApiServerService/GetChatMessage` (application/connect+proto)
//!    with gzip-framed request, stream of gzip-framed responses
//! 4. Parse Connect frames, gunzip, decode protobuf, emit `ProviderEvents`

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures_lite::io::{AsyncReadExt, BufReader};

use flate2::Compression;
use flate2::write::GzEncoder;
use flume::Sender;
use isahc::AsyncReadResponseExt;
use isahc::RequestExt;
use serde::Deserialize;
use tracing::debug;

use crate::model::ModelEntry;
use crate::provider::{BoxFuture, Provider};
use crate::types::{ContentBlock, Role, System};
use crate::{
    AgentError, Message, ProviderEvent, RequestOptions, StopReason, StreamResponse, TokenUsage,
};

use super::ResolvedAuth;
use super::devin_connect::{
    CONNECT_COMPRESSED_FLAG, FrameBuffer, decode_frame_payload, encode_frame,
};
use super::devin_proto::{
    CHAT_MESSAGE_SOURCE_SYSTEM, CHAT_MESSAGE_SOURCE_TOOL, CHAT_MESSAGE_SOURCE_USER,
    ChatMessagePromptInput, ChatToolCall, ChatToolDefinition, ImageData, decode_cli_model_configs,
    decode_get_chat_message_response, decode_get_user_jwt_response, encode_chat_message_prompt,
    encode_chat_tool_definition, encode_get_chat_message_request,
    encode_get_cli_model_configs_request, encode_get_user_jwt_request,
};

use n00n_storage::id::n00nId;

const DEVIN_API_URL: &str = "https://server.codeium.com";
const DEVIN_AUTH_PATH: &str = "/exa.auth_pb.AuthService/GetUserJwt";
const DEVIN_CHAT_PATH: &str = "/exa.api_server_pb.ApiServerService/GetChatMessage";
const DEVIN_CLI_MODEL_CONFIGS_PATH: &str = "/exa.api_server_pb.ApiServerService/GetCliModelConfigs";
const DEVIN_SESSION_TOKEN_PREFIX: &str = "devin-session-token$";
const DEFAULT_TEMPERATURE: f64 = 0.4;
const DEFAULT_TOP_P: f64 = 1.0;
const DEFAULT_MAX_TOKENS: u32 = 64_000;

inventory::submit!(n00n_config::providers::BuiltInProvider {
    slug: "devin",
    display_name: "Devin",
    protocol: n00n_config::providers::Protocol::Devin,
    default_base_url: DEVIN_API_URL,
    default_api_key_env: "DEVIN_API_KEY",
    default_model: "devin/swe-1-7",
    plans: None,
    login_url: None,
    needs_url: false,
});

pub(crate) const fn models() -> &'static [ModelEntry] {
    crate::providers::devin_models::models()
}

#[derive(Debug, Clone)]
struct DevinCredentials {
    session_token: String,
    api_server_url: String,
}

#[derive(Deserialize)]
struct TomlCredentials {
    windsurf_api_key: Option<String>,
    api_server_url: Option<String>,
}

impl DevinCredentials {
    fn from_env() -> Option<Self> {
        let session_token = std::env::var("WINDSURF_API_KEY")
            .ok()
            .or_else(|| std::env::var("DEVIN_API_KEY").ok())?;
        Some(Self {
            session_token: normalize_session_token(&session_token),
            api_server_url: DEVIN_API_URL.to_string(),
        })
    }

    fn from_file() -> Option<Self> {
        let home = std::env::var("HOME").ok()?;
        let creds_path = PathBuf::from(home).join(".local/share/devin/credentials.toml");

        let content = std::fs::read_to_string(&creds_path).ok()?;

        let creds: TomlCredentials = toml::from_str(&content).ok()?;
        let session_token = creds.windsurf_api_key?;
        let api_server_url = match creds.api_server_url {
            Some(url) => url,
            None => DEVIN_API_URL.to_string(),
        };

        Some(Self {
            session_token: normalize_session_token(&session_token),
            api_server_url,
        })
    }
}

fn normalize_session_token(token: &str) -> String {
    if token.starts_with(DEVIN_SESSION_TOKEN_PREFIX) {
        token.to_string()
    } else {
        format!("{DEVIN_SESSION_TOKEN_PREFIX}{token}")
    }
}

fn chat_message_id(_cascade_id: &str, _message_index: usize, suffix: &str) -> String {
    if suffix == "assistant" {
        format!("bot-{}", n00nId::generate())
    } else {
        n00nId::generate().to_string()
    }
}

fn encode_devin_tools(tools: &serde_json::Value) -> Result<Vec<Vec<u8>>, AgentError> {
    let arr = tools
        .as_array()
        .map_or_else(|| &[][..], std::vec::Vec::as_slice);
    let mut encoded = Vec::with_capacity(arr.len());
    for tool in arr {
        let function = match tool.get("function") {
            Some(v) => v,
            None => tool,
        };
        let name = function
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AgentError::Config {
                message: "tool missing name".to_string(),
            })?;
        let description = function
            .get("description")
            .and_then(serde_json::Value::as_str);
        let schema_string = match function.get("parameters") {
            Some(v) => serde_json::to_string(v).map_err(|e| AgentError::Config {
                message: format!("failed to serialize tool schema: {e}"),
            })?,
            None => "{}".to_string(),
        };
        let strict = tool
            .get("strict")
            .or_else(|| function.get("strict"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or_else(|| false);
        encoded.push(encode_chat_tool_definition(&ChatToolDefinition {
            name,
            description: description.unwrap_or_else(|| ""),
            json_schema_string: &schema_string,
            strict,
        }));
    }
    Ok(encoded)
}

fn encode_devin_chat_message_prompts(messages: &[Message], cascade_id: &str) -> Vec<Vec<u8>> {
    let mut prompts = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        match message.role {
            Role::User => {
                let mut prompt_text = String::new();
                let mut images = Vec::new();
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => prompt_text.push_str(text),
                        ContentBlock::Image { source } => images.push(ImageData {
                            base64_data: source.data.as_ref(),
                            mime_type: source.media_type.mime(),
                            caption: "",
                        }),
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            if !prompt_text.is_empty() || !images.is_empty() {
                                prompts.push(encode_chat_message_prompt(&ChatMessagePromptInput {
                                    message_id: &chat_message_id(cascade_id, index, "user"),
                                    source: CHAT_MESSAGE_SOURCE_USER,
                                    prompt: &prompt_text,
                                    images: &images,
                                    ..ChatMessagePromptInput::default()
                                }));
                                prompt_text.clear();
                                images.clear();
                            }
                            prompts.push(encode_chat_message_prompt(&ChatMessagePromptInput {
                                message_id: &chat_message_id(
                                    cascade_id,
                                    index,
                                    &format!("tool\0{tool_use_id}"),
                                ),
                                source: CHAT_MESSAGE_SOURCE_TOOL,
                                prompt: content,
                                tool_call_id: tool_use_id,
                                tool_result_is_error: *is_error,
                                ..ChatMessagePromptInput::default()
                            }));
                        }
                        _ => {}
                    }
                }
                if !prompt_text.is_empty() || !images.is_empty() {
                    prompts.push(encode_chat_message_prompt(&ChatMessagePromptInput {
                        message_id: &chat_message_id(cascade_id, index, "user"),
                        source: CHAT_MESSAGE_SOURCE_USER,
                        prompt: &prompt_text,
                        images: &images,
                        ..ChatMessagePromptInput::default()
                    }));
                }
            }
            Role::Assistant => {
                let mut prompt_text = String::new();
                let mut thinking = String::new();
                let mut signature = String::new();
                let mut tool_calls = Vec::new();
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => prompt_text.push_str(text),
                        ContentBlock::Thinking {
                            thinking: t,
                            signature: sig,
                        } => {
                            thinking.push_str(t);
                            if signature.is_empty()
                                && let Some(s) = sig
                            {
                                signature.clone_from(s);
                            }
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            let arguments_json =
                                serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string());
                            tool_calls.push(ChatToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                arguments_json,
                            });
                        }
                        _ => {}
                    }
                }
                if !prompt_text.is_empty()
                    || !thinking.is_empty()
                    || !signature.is_empty()
                    || !tool_calls.is_empty()
                {
                    prompts.push(encode_chat_message_prompt(&ChatMessagePromptInput {
                        message_id: &chat_message_id(cascade_id, index, "assistant"),
                        source: CHAT_MESSAGE_SOURCE_SYSTEM,
                        prompt: &prompt_text,
                        thinking: &thinking,
                        signature: &signature,
                        tool_calls: &tool_calls,
                        ..ChatMessagePromptInput::default()
                    }));
                }
            }
        }
    }
    prompts
}

pub struct Devin {
    credentials: Option<DevinCredentials>,
    client_model_configs: Mutex<Option<std::collections::HashMap<String, String>>>,
}

impl Devin {
    pub fn new(timeouts: super::Timeouts) -> Self {
        let _ = timeouts; // Not used in native implementation

        Self {
            credentials: DevinCredentials::from_env().or_else(DevinCredentials::from_file),
            client_model_configs: Mutex::new(None),
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    pub fn with_auth(
        auth: &Arc<Mutex<ResolvedAuth>>,
        _timeouts: super::Timeouts,
    ) -> Result<Self, AgentError> {
        let resolved = match auth.lock() {
            Ok(guard) => guard,
            Err(e) => e.into_inner(),
        };

        let api_server_url = match resolved.base_url.clone() {
            Some(url) => url,
            None => DEVIN_API_URL.to_string(),
        };

        let session_token = resolved
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .and_then(|(_, v)| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(normalize_session_token);

        let credentials = session_token.map(|token| DevinCredentials {
            session_token: token,
            api_server_url,
        });

        Ok(Self {
            credentials,
            client_model_configs: Mutex::new(None),
        })
    }

    async fn get_user_jwt(&self) -> Result<(String, String), AgentError> {
        let creds = self
            .credentials
            .as_ref()
            .ok_or_else(|| AgentError::Config {
                message: "no Devin credentials found".to_string(),
            })?;

        let request_bytes = encode_get_user_jwt_request(&creds.session_token);

        let url = format!("{}{}", creds.api_server_url, DEVIN_AUTH_PATH);

        let mut response = isahc::Request::post(&url)
            .header("content-type", "application/proto")
            .header("connect-protocol-version", "1")
            .body(request_bytes)
            .map_err(|e| AgentError::Config {
                message: format!("failed to build auth request: {e}"),
            })?
            .send_async()
            .await
            .map_err(|e| AgentError::Api {
                status: 0,
                message: format!("auth request failed: {e}"),
            })?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = match response.text().await {
                Ok(b) => b,
                Err(_) => "unable to read error body".to_string(),
            };
            return Err(AgentError::Api {
                status,
                message: format!("auth failed: {body}"),
            });
        }

        let response_bytes = response.bytes().await.map_err(|e| AgentError::Api {
            status: 0,
            message: format!("failed to read auth response: {e}"),
        })?;

        let auth_response =
            decode_get_user_jwt_response(&response_bytes).map_err(|e| AgentError::Api {
                status: 0,
                message: format!("failed to decode auth response: {e}"),
            })?;

        if auth_response.user_jwt.is_empty() {
            return Err(AgentError::Api {
                status: 0,
                message: "auth response missing user_jwt".to_string(),
            });
        }

        let base_url = if auth_response.custom_api_server_url.is_empty() {
            creds.api_server_url.clone()
        } else {
            auth_response
                .custom_api_server_url
                .trim_end_matches('/')
                .to_string()
        };

        Ok((auth_response.user_jwt, base_url))
    }

    async fn get_cli_model_configs(
        &self,
        base_url: &str,
    ) -> Result<std::collections::HashMap<String, String>, AgentError> {
        if let Ok(guard) = self.client_model_configs.lock()
            && let Some(cache) = guard.as_ref()
        {
            return Ok(cache.clone());
        }

        let creds = self
            .credentials
            .as_ref()
            .ok_or_else(|| AgentError::Config {
                message: "no Devin credentials found".to_string(),
            })?;

        let request_bytes = encode_get_cli_model_configs_request(&creds.session_token);

        let url = format!("{base_url}{DEVIN_CLI_MODEL_CONFIGS_PATH}");
        let mut response = isahc::Request::post(&url)
            .header("content-type", "application/proto")
            .header("connect-protocol-version", "1")
            .body(request_bytes)
            .map_err(|e| AgentError::Config {
                message: format!("failed to build model configs request: {e}"),
            })?
            .send_async()
            .await
            .map_err(|e| AgentError::Api {
                status: 0,
                message: format!("model configs request failed: {e}"),
            })?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = match response.text().await {
                Ok(b) => b,
                Err(_) => "unable to read error body".to_string(),
            };
            return Err(AgentError::Api {
                status,
                message: format!("model configs failed: {body}"),
            });
        }

        let response_bytes = response.bytes().await.map_err(|e| AgentError::Api {
            status: 0,
            message: format!("failed to read model configs response: {e}"),
        })?;

        let configs = decode_cli_model_configs(&response_bytes).map_err(|e| AgentError::Api {
            status: 0,
            message: format!("failed to decode model configs response: {e}"),
        })?;

        if let Ok(mut guard) = self.client_model_configs.lock() {
            *guard = Some(configs.clone());
        }

        Ok(configs)
    }

    async fn stream_chat_message<'a>(
        &'a self,
        model: &'a crate::model::Model,
        messages: &'a [Message],
        system: &'a System,
        tools: &'a serde_json::Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
    ) -> Result<StreamResponse, AgentError> {
        let _ = opts;
        let (user_jwt, base_url) = self.get_user_jwt().await?;
        let creds = self
            .credentials
            .as_ref()
            .ok_or_else(|| AgentError::Config {
                message: "no Devin credentials found".to_string(),
            })?;

        let model_router_uid = model
            .id
            .split('/')
            .next_back()
            .unwrap_or_else(|| model.id.as_str());
        // Resolve aliases (e.g. "opus") to the canonical model uid before
        // looking up the server-side wire id.
        let canonical_id =
            crate::model::lookup_entry(crate::providers::devin::models(), model_router_uid)
                .map_or(model_router_uid, |entry| entry.prefixes[0]);
        let cli_configs = self.get_cli_model_configs(&base_url).await?;
        let chat_model_uid = cli_configs
            .get(canonical_id)
            .map_or(canonical_id, |wire| wire.as_str());

        let cascade_id = n00nId::generate().to_string();
        let execution_id = n00nId::generate().to_string();

        let prompt = system
            .blocks()
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n");

        let chat_message_prompts = encode_devin_chat_message_prompts(messages, &cascade_id);
        let chat_tools = encode_devin_tools(tools)?;

        let max_tokens = u64::from(
            model
                .max_output_tokens
                .map_or(DEFAULT_MAX_TOKENS, |m| m.min(DEFAULT_MAX_TOKENS)),
        );
        let request_bytes = encode_get_chat_message_request(
            &creds.session_token,
            &user_jwt,
            &prompt,
            chat_model_uid,
            &cascade_id,
            &execution_id,
            &chat_message_prompts,
            &chat_tools,
            max_tokens,
            DEFAULT_TEMPERATURE,
            DEFAULT_TOP_P,
        );

        let gzipped = {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder
                .write_all(&request_bytes)
                .map_err(|e| AgentError::Config {
                    message: format!("gzip compression failed: {e}"),
                })?;
            encoder.finish().map_err(|e| AgentError::Config {
                message: format!("gzip finish failed: {e}"),
            })?
        };

        let frame =
            encode_frame(CONNECT_COMPRESSED_FLAG, &gzipped).map_err(|e| AgentError::Config {
                message: format!("failed to encode connect frame: {e}"),
            })?;

        let url = format!("{base_url}{DEVIN_CHAT_PATH}");

        let mut response = isahc::Request::post(&url)
            .header("content-type", "application/connect+proto")
            .header("connect-protocol-version", "1")
            .header("connect-content-encoding", "gzip")
            .header("accept-encoding", "identity")
            .header("user-agent", "connect-go/1.18.1 (go1.26.3)")
            .header("connect-accept-encoding", "gzip")
            .body(frame)
            .map_err(|e| AgentError::Config {
                message: format!("failed to build chat request: {e}"),
            })?
            .send_async()
            .await
            .map_err(|e| AgentError::Api {
                status: 0,
                message: format!("chat request failed: {e}"),
            })?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = match response.text().await {
                Ok(b) => b,
                Err(_) => "unable to read error body".to_string(),
            };
            return Err(AgentError::Api {
                status,
                message: format!("chat failed: {body}"),
            });
        }

        let mut reader = BufReader::new(response.into_body());

        let mut frame_buffer = FrameBuffer::default();
        let mut text = String::new();
        let mut thinking = String::new();
        let mut usage = TokenUsage::default();
        let mut stop_reason = StopReason::EndTurn;
        let mut tool_calls: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();

        let mut buffer = vec![0u8; 8192];

        loop {
            let n = reader
                .read(&mut buffer)
                .await
                .map_err(|e| AgentError::Api {
                    status: 0,
                    message: format!("failed to read response: {e}"),
                })?;

            if n == 0 {
                break;
            }

            frame_buffer.push(&buffer[..n]);

            while let Some(frame_result) = frame_buffer.next_frame() {
                let frame = frame_result.map_err(|e| AgentError::Api {
                    status: 0,
                    message: format!("invalid connect frame: {e}"),
                })?;

                if frame.end_stream {
                    // Parse trailer JSON for errors
                    let payload = decode_frame_payload(&frame).map_err(|e| AgentError::Api {
                        status: 0,
                        message: format!("failed to decode trailer: {e}"),
                    })?;
                    let trailer = String::from_utf8_lossy(&payload);
                    if !trailer.trim().is_empty() {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&trailer)
                            && let Some(code) =
                                value.get("code").and_then(serde_json::Value::as_str)
                            && !code.is_empty()
                            && code != "ok"
                        {
                            let msg = value
                                .get("message")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_else(|| code);
                            return Err(AgentError::Api {
                                status: 0,
                                message: format!("devin stream failed: {code}: {msg}"),
                            });
                        }
                        debug!("devin trailer: {}", trailer);
                    }
                    continue;
                }

                let payload = decode_frame_payload(&frame).map_err(|e| AgentError::Api {
                    status: 0,
                    message: format!("failed to decode frame payload: {e}"),
                })?;

                let response =
                    decode_get_chat_message_response(&payload).map_err(|e| AgentError::Api {
                        status: 0,
                        message: format!("failed to decode chat response: {e}"),
                    })?;

                if !response.delta_text.is_empty() {
                    let delta = response.delta_text;
                    text.push_str(&delta);
                    let _ = event_tx
                        .send_async(ProviderEvent::TextDelta { text: delta })
                        .await;
                }

                if !response.delta_thinking.is_empty() {
                    let delta = response.delta_thinking;
                    thinking.push_str(&delta);
                    let _ = event_tx
                        .send_async(ProviderEvent::ThinkingDelta { text: delta })
                        .await;
                }

                for tc in response.delta_tool_calls {
                    if !tool_calls.contains_key(&tc.id) {
                        let _ = event_tx
                            .send_async(ProviderEvent::ToolUseStart {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                            })
                            .await;
                        tool_calls.insert(tc.id.clone(), (tc.name.clone(), tc.arguments_json));
                    }
                }

                if response.stop_reason != 0 {
                    stop_reason = match response.stop_reason {
                        3 => StopReason::MaxTokens,
                        10 => StopReason::ToolUse,
                        _ => StopReason::EndTurn,
                    };
                }

                if let Some(u) = response.usage {
                    usage.input = u32::try_from(u.input_tokens).unwrap_or_else(|_| 0);
                    usage.output = u32::try_from(u.output_tokens).unwrap_or_else(|_| 0);
                    usage.cache_read = u32::try_from(u.cache_read_tokens).unwrap_or_else(|_| 0);
                    usage.cache_creation =
                        u32::try_from(u.cache_write_tokens).unwrap_or_else(|_| 0);
                }
            }
        }

        let mut content_blocks = Vec::new();
        if !thinking.is_empty() {
            content_blocks.push(crate::types::ContentBlock::Thinking {
                thinking,
                signature: None,
            });
        }
        if !text.is_empty() {
            content_blocks.push(crate::types::ContentBlock::Text { text });
        }
        for (id, (name, arguments_json)) in tool_calls {
            let input =
                serde_json::from_str(&arguments_json).unwrap_or_else(|_| serde_json::Value::Null);
            content_blocks.push(crate::types::ContentBlock::ToolUse { id, name, input });
        }
        if content_blocks.is_empty() {
            content_blocks.push(crate::types::ContentBlock::Text {
                text: String::new(),
            });
        }

        let message = Message {
            role: Role::Assistant,
            content: content_blocks,
            display_text: None,
            control: false,
        };

        Ok(StreamResponse {
            message,
            usage,
            stop_reason: Some(stop_reason),
        })
    }
}

impl Provider for Devin {
    fn stream_message<'a>(
        &'a self,
        model: &'a crate::model::Model,
        messages: &'a [Message],
        system: &'a System,
        tools: &'a serde_json::Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        _session_id: Option<&'a n00n_storage::id::SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            self.stream_chat_message(model, messages, system, tools, event_tx, opts)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        Box::pin(async move {
            let models = models()
                .iter()
                .map(|e| crate::model::ModelInfo {
                    id: e.prefixes[0].to_string(),
                    name: None,
                    context_window: Some(e.context_window),
                    max_output_tokens: Some(e.max_output_tokens),
                    pricing: Some(e.pricing),
                    supports_thinking: None,
                    supports_vision: Some(e.vision),
                    tier: Some(e.tier),
                    is_free: None,
                    is_promo: None,
                    provider_info: None,
                })
                .collect();
            Ok(models)
        })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async { Ok(false) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_session_token_adds_prefix() {
        assert_eq!(
            normalize_session_token("abc123"),
            "devin-session-token$abc123"
        );
    }

    #[test]
    fn normalize_session_token_preserves_prefix() {
        assert_eq!(
            normalize_session_token("devin-session-token$abc123"),
            "devin-session-token$abc123"
        );
    }
}
