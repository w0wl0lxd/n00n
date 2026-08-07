//! Native Devin provider using Connect protocol over gRPC-Web.
//!
//! Protocol:
//! 1. Read `~/.local/share/devin/credentials.toml` or env for session token
//! 2. Call `POST /exa.auth_pb.AuthService/GetUserJwt` (application/proto) to get user JWT
//! 3. Call `POST /exa.api_server_pb.ApiServerService/GetChatMessage` (application/connect+proto)
//!    with gzip-framed request, stream of gzip-framed responses
//! 4. Parse Connect frames, gunzip, decode protobuf, emit `ProviderEvents`

use std::collections::HashMap;
use std::io::{ErrorKind, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures_lite::io::{AsyncReadExt, BufReader};

use flate2::Compression;
use flate2::write::GzEncoder;
use flume::Sender;
use isahc::{AsyncReadResponseExt, HttpClient};
use serde::Deserialize;
use tracing::{debug, warn};

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
    ChatMessagePromptInput, ChatToolCall, ChatToolDefinition, ImageData, ModelUsageStats,
    STOP_REASON_MAX_TOKENS, STOP_REASON_TOOL_USE, STOP_REASON_UNSPECIFIED,
    decode_cli_model_configs, decode_get_chat_message_response, decode_get_user_jwt_response,
    encode_chat_message_prompt, encode_chat_tool_definition, encode_get_chat_message_request,
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
const MAX_TRAILER_CODE_LEN: usize = 64;

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
    fn from_env() -> Result<Option<Self>, AgentError> {
        let session_token = match optional_env("WINDSURF_API_KEY")? {
            Some(token) => Some(token),
            None => optional_env("DEVIN_API_KEY")?,
        };
        Ok(session_token.map(|token| Self {
            session_token: normalize_session_token(&token),
            api_server_url: DEVIN_API_URL.to_string(),
        }))
    }

    fn from_file() -> Result<Option<Self>, AgentError> {
        let Some(home) = optional_env("HOME")? else {
            return Ok(None);
        };
        Self::from_path(&PathBuf::from(home).join(".local/share/devin/credentials.toml"))
    }

    fn from_path(creds_path: &Path) -> Result<Option<Self>, AgentError> {
        let content = match std::fs::read_to_string(creds_path) {
            Ok(content) => content,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AgentError::Config {
                    message: format!(
                        "failed to read Devin credentials at {}: {error}",
                        creds_path.display()
                    ),
                });
            }
        };
        let creds: TomlCredentials =
            toml::from_str(&content).map_err(|error| AgentError::Config {
                message: format!(
                    "failed to parse Devin credentials at {}: {error}",
                    creds_path.display()
                ),
            })?;
        let session_token = creds.windsurf_api_key.ok_or_else(|| AgentError::Config {
            message: format!(
                "Devin credentials at {} are missing windsurf_api_key",
                creds_path.display()
            ),
        })?;
        if session_token.trim().is_empty() {
            return Err(AgentError::Config {
                message: format!(
                    "Devin credentials at {} contain an empty windsurf_api_key",
                    creds_path.display()
                ),
            });
        }
        Ok(Some(Self {
            session_token: normalize_session_token(&session_token),
            api_server_url: match creds.api_server_url {
                Some(url) => url,
                None => DEVIN_API_URL.to_string(),
            },
        }))
    }
}

fn optional_env(name: &'static str) -> Result<Option<String>, AgentError> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error @ std::env::VarError::NotUnicode(_)) => Err(AgentError::Config {
            message: format!("environment variable {name} is not valid Unicode: {error}"),
        }),
    }
}

fn resolve_api_server_url(configured: String, explicit: Option<&str>) -> String {
    explicit.map_or(configured, str::to_string)
}

fn discover_credentials() -> Result<Option<DevinCredentials>, AgentError> {
    match DevinCredentials::from_env()? {
        Some(credentials) => Ok(Some(credentials)),
        None => DevinCredentials::from_file(),
    }
}

fn normalize_session_token(token: &str) -> String {
    if token.starts_with(DEVIN_SESSION_TOKEN_PREFIX) {
        token.to_string()
    } else {
        format!("{DEVIN_SESSION_TOKEN_PREFIX}{token}")
    }
}

fn chat_message_id(cascade_id: &str, message_index: usize, role: &str) -> String {
    if role == "assistant" {
        format!("bot-{cascade_id}-{message_index}-{role}")
    } else {
        format!("{cascade_id}-{message_index}-{role}")
    }
}

fn max_tokens_for_model(max_output_tokens: Option<u32>) -> u64 {
    u64::from(match max_output_tokens {
        Some(max_output_tokens) => max_output_tokens,
        None => DEFAULT_MAX_TOKENS,
    })
}

fn clamp_tokens(field: &'static str, value: u64) -> u32 {
    if let Ok(value) = u32::try_from(value) {
        value
    } else {
        warn!(
            field,
            value, "Devin usage token count out of range; clamping"
        );
        u32::MAX
    }
}

fn devin_usage_to_token_usage(u: &ModelUsageStats) -> TokenUsage {
    // Devin gRPC usage reports input_tokens as the total prompt tokens
    // (including cache reads and writes), with cache fields as details.
    // TokenUsage.input must be the non-cached portion so that total_input()
    // and cost() are consistent with the rest of the providers. Some
    // responses already report input_tokens as the non-cached remainder;
    // keep that reported value instead of saturating to zero.
    let cached = u.cache_read_tokens.saturating_add(u.cache_write_tokens);
    let (input, cache_read, cache_creation) = if u.input_tokens >= cached {
        (
            u.input_tokens.saturating_sub(cached),
            u.cache_read_tokens,
            u.cache_write_tokens,
        )
    } else {
        debug!(
            input_tokens = u.input_tokens,
            cache_read_tokens = u.cache_read_tokens,
            cache_write_tokens = u.cache_write_tokens,
            "Devin input_tokens is less than cached tokens; treating as non-cached"
        );
        (u.input_tokens, u.cache_read_tokens, u.cache_write_tokens)
    };
    TokenUsage {
        input: clamp_tokens("input", input),
        output: clamp_tokens("output", u.output_tokens),
        cache_creation: clamp_tokens("cache_write", cache_creation),
        cache_read: clamp_tokens("cache_read", cache_read),
    }
}

fn sanitize_trailer_code(code: &str) -> &str {
    if !code.is_empty()
        && code.len() <= MAX_TRAILER_CODE_LEN
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        code
    } else {
        "invalid"
    }
}

fn parse_devin_trailer(payload: &[u8]) -> Result<Option<String>, AgentError> {
    if payload.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let trailer = std::str::from_utf8(payload).map_err(|_| AgentError::Api {
        status: 0,
        message: "invalid Devin end-stream trailer encoding".to_string(),
    })?;
    let value: serde_json::Value = serde_json::from_str(trailer).map_err(|_| AgentError::Api {
        status: 0,
        message: "invalid Devin end-stream trailer JSON".to_string(),
    })?;
    let code = value
        .get("error")
        .and_then(|error| error.get("code"))
        .or_else(|| value.get("code"))
        .and_then(serde_json::Value::as_str)
        .map(sanitize_trailer_code);
    match code {
        Some("ok") | None => Ok(code.map(str::to_string)),
        Some(code) => Err(AgentError::Api {
            status: 0,
            message: format!("Devin stream failed with trailer code {code}"),
        }),
    }
}

fn encode_devin_tools(tools: &serde_json::Value) -> Result<Vec<Vec<u8>>, AgentError> {
    // Accept null or empty object as "no tools"
    if tools.is_null()
        || (tools.is_object() && tools.as_object().map_or(false, serde_json::Map::is_empty))
    {
        return Ok(Vec::new());
    }
    let arr = tools.as_array().ok_or_else(|| AgentError::Config {
        message: "Devin tools must be an array".to_string(),
    })?;
    if arr.is_empty() {
        return Ok(Vec::new());
    }
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
        let schema_string = match function.get("input_schema") {
            Some(v) => serde_json::to_string(v).map_err(|e| AgentError::Config {
                message: format!("failed to serialize tool schema: {e}"),
            })?,
            None => "{}".to_string(),
        };
        let strict = tool
            .get("strict")
            .or_else(|| function.get("strict"))
            .and_then(serde_json::Value::as_bool)
            .map_or(false, std::convert::identity);
        encoded.push(encode_chat_tool_definition(&ChatToolDefinition {
            name: name.to_string(),
            description: description.map_or(String::new(), std::string::ToString::to_string),
            json_schema_string: schema_string.clone(),
            strict,
        }));
    }
    Ok(encoded)
}

fn merge_tool_call(
    tool_calls: &mut HashMap<String, (String, String)>,
    tool_call: ChatToolCall,
) -> bool {
    if let Some((name, arguments_json)) = tool_calls.get_mut(&tool_call.id) {
        if !tool_call.name.is_empty() {
            *name = tool_call.name;
        }
        arguments_json.push_str(&tool_call.arguments_json);
        false
    } else {
        tool_calls.insert(tool_call.id, (tool_call.name, tool_call.arguments_json));
        true
    }
}

fn ordered_tool_call_blocks(
    mut tool_calls: HashMap<String, (String, String)>,
    tool_call_order: Vec<String>,
) -> Result<Vec<ContentBlock>, AgentError> {
    let mut blocks = Vec::with_capacity(tool_call_order.len());
    for id in tool_call_order {
        let (name, arguments_json) = tool_calls.remove(&id).ok_or_else(|| AgentError::Api {
            status: 0,
            message: "Devin tool-call ordering state is inconsistent".to_string(),
        })?;
        let input = serde_json::from_str(&arguments_json).map_err(|error| AgentError::Api {
            status: 0,
            message: format!("invalid Devin tool arguments for {name}: {error}"),
        })?;
        blocks.push(ContentBlock::ToolUse { id, name, input });
    }
    Ok(blocks)
}

fn encode_devin_chat_message_prompts(
    messages: &[Message],
    cascade_id: &str,
) -> Result<Vec<Vec<u8>>, AgentError> {
    let mut prompts = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        match message.role {
            Role::User => {
                let mut prompt_text = String::new();
                let mut images = Vec::new();
                let mut user_part = 0usize;
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => prompt_text.push_str(text),
                        ContentBlock::Image { source } => images.push(ImageData {
                            base64_data: source.data.to_string(),
                            mime_type: source.media_type.mime().to_string(),
                            caption: String::new(),
                        }),
                        ContentBlock::File { source } => {
                            let identifier = source.identifier().unwrap_or_else(|| "unknown");
                            prompt_text.push_str("[file omitted: ");
                            prompt_text.push_str(identifier);
                            prompt_text.push(']');
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            if !prompt_text.is_empty() || !images.is_empty() {
                                prompts.push(encode_chat_message_prompt(&ChatMessagePromptInput {
                                    message_id: &chat_message_id(
                                        cascade_id,
                                        index,
                                        &format!("user-{user_part}"),
                                    ),
                                    source: CHAT_MESSAGE_SOURCE_USER,
                                    prompt: &prompt_text,
                                    images: &images,
                                    ..ChatMessagePromptInput::default()
                                }));
                                prompt_text.clear();
                                images.clear();
                                user_part += 1;
                            }
                            prompts.push(encode_chat_message_prompt(&ChatMessagePromptInput {
                                message_id: &chat_message_id(
                                    cascade_id,
                                    index,
                                    &format!("tool-{tool_use_id}"),
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
                        message_id: &chat_message_id(
                            cascade_id,
                            index,
                            &format!("user-{user_part}"),
                        ),
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
                            let arguments_json = serde_json::to_string(input).map_err(|error| {
                                AgentError::Config {
                                    message: format!(
                                        "failed to serialize Devin tool arguments: {error}"
                                    ),
                                }
                            })?;
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
    Ok(prompts)
}

pub struct Devin {
    credentials: Option<DevinCredentials>,
    client: HttpClient,
    client_model_configs: Mutex<Option<HashMap<String, String>>>,
    timeouts: super::Timeouts,
}

impl Devin {
    pub fn new(timeouts: super::Timeouts) -> Result<Self, AgentError> {
        Ok(Self {
            credentials: discover_credentials()?,
            client: super::http_client(timeouts)?,
            client_model_configs: Mutex::new(None),
            timeouts,
        })
    }

    #[allow(clippy::unnecessary_wraps)]
    pub fn with_auth(
        auth: &Arc<Mutex<ResolvedAuth>>,
        timeouts: super::Timeouts,
    ) -> Result<Self, AgentError> {
        let resolved = match auth.lock() {
            Ok(guard) => guard,
            Err(e) => e.into_inner(),
        };

        let resolved_base_url = resolved.base_url.clone();

        let session_token = resolved
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .and_then(|(_, v)| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(normalize_session_token);

        let credentials = match session_token {
            Some(token) => Some(DevinCredentials {
                session_token: token,
                api_server_url: DEVIN_API_URL.to_string(),
            }),
            None => discover_credentials()?,
        }
        .map(|mut credentials| {
            credentials.api_server_url =
                resolve_api_server_url(credentials.api_server_url, resolved_base_url.as_deref());
            credentials
        });

        Ok(Self {
            credentials,
            client: super::http_client(timeouts)?,
            client_model_configs: Mutex::new(None),
            timeouts,
        })
    }

    fn http_client(&self) -> &HttpClient {
        &self.client
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

        let request = isahc::Request::post(&url)
            .header("content-type", "application/proto")
            .header("connect-protocol-version", "1")
            .body(request_bytes)
            .map_err(|e| AgentError::Config {
                message: format!("failed to build auth request: {e}"),
            })?;
        let mut response =
            self.http_client()
                .send_async(request)
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
    ) -> Result<HashMap<String, String>, AgentError> {
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
        let request = isahc::Request::post(&url)
            .header("content-type", "application/proto")
            .header("connect-protocol-version", "1")
            .body(request_bytes)
            .map_err(|e| AgentError::Config {
                message: format!("failed to build model configs request: {e}"),
            })?;
        let mut response =
            self.http_client()
                .send_async(request)
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
        // Devin cannot express thinking, fast-mode, or cache/history replay options.
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

        let chat_message_prompts = encode_devin_chat_message_prompts(messages, &cascade_id)?;
        let chat_tools = encode_devin_tools(tools)?;

        let max_tokens = max_tokens_for_model(model.max_output_tokens);
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

        let request = isahc::Request::post(&url)
            .header("content-type", "application/connect+proto")
            .header("connect-protocol-version", "1")
            .header("connect-content-encoding", "gzip")
            .header("accept-encoding", "identity")
            .header("user-agent", "connect-go/1.18.1 (go1.26.3)")
            .header("connect-accept-encoding", "gzip")
            .body(frame)
            .map_err(|e| AgentError::Config {
                message: format!("failed to build chat request: {e}"),
            })?;
        let mut response =
            self.http_client()
                .send_async(request)
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
        let mut signature = String::new();
        let mut usage = TokenUsage::default();
        let mut stream_deadline = Instant::now() + self.timeouts.stream;
        let mut stop_reason = StopReason::EndTurn;
        let mut tool_calls: HashMap<String, (String, String)> = HashMap::new();
        let mut tool_call_order = Vec::new();

        let mut buffer = vec![0u8; 8192];

        'stream: loop {
            let n = futures_lite::future::or(
                async {
                    reader.read(&mut buffer).await.map_err(|e| AgentError::Api {
                        status: 0,
                        message: format!("failed to read response: {e}"),
                    })
                },
                async {
                    smol::Timer::after(stream_deadline.saturating_duration_since(Instant::now()))
                        .await;
                    Err(AgentError::Timeout {
                        secs: self.timeouts.stream.as_secs(),
                    })
                },
            )
            .await?;

            if n == 0 {
                let message = if frame_buffer.is_empty() {
                    "Devin stream ended before the end-stream trailer"
                } else {
                    "truncated Devin Connect frame at end of stream"
                };
                return Err(AgentError::Api {
                    status: 0,
                    message: message.to_string(),
                });
            }
            stream_deadline = Instant::now() + self.timeouts.stream;

            frame_buffer.push(&buffer[..n]);

            while let Some(frame_result) = frame_buffer.next_frame() {
                let frame = frame_result.map_err(|e| AgentError::Api {
                    status: 0,
                    message: format!("invalid connect frame: {e}"),
                })?;

                if frame.end_stream {
                    let payload = decode_frame_payload(&frame).map_err(|e| AgentError::Api {
                        status: 0,
                        message: format!("failed to decode trailer: {e}"),
                    })?;
                    if let Some(code) = parse_devin_trailer(&payload)? {
                        debug!(
                            trailer_code = code,
                            trailer_bytes = payload.len(),
                            "Devin end-stream trailer received"
                        );
                    } else {
                        debug!(
                            trailer_bytes = payload.len(),
                            "Devin end-stream trailer received"
                        );
                    }
                    break 'stream;
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
                    event_tx
                        .send_async(ProviderEvent::TextDelta { text: delta })
                        .await
                        .map_err(|_| {
                            debug!("Devin event receiver closed; ending stream");
                            AgentError::Channel
                        })?;
                }

                if !response.delta_thinking.is_empty() {
                    let delta = response.delta_thinking;
                    thinking.push_str(&delta);
                    event_tx
                        .send_async(ProviderEvent::ThinkingDelta { text: delta })
                        .await
                        .map_err(|_| {
                            debug!("Devin event receiver closed; ending stream");
                            AgentError::Channel
                        })?;
                }
                signature.push_str(&response.delta_signature);

                for tc in response.delta_tool_calls {
                    let id = tc.id.clone();
                    let name = tc.name.clone();
                    if merge_tool_call(&mut tool_calls, tc) {
                        tool_call_order.push(id.clone());
                        event_tx
                            .send_async(ProviderEvent::ToolUseStart { id, name })
                            .await
                            .map_err(|_| {
                                debug!("Devin event receiver closed; ending stream");
                                AgentError::Channel
                            })?;
                    }
                }

                if response.stop_reason != STOP_REASON_UNSPECIFIED {
                    stop_reason = match response.stop_reason {
                        STOP_REASON_MAX_TOKENS => StopReason::MaxTokens,
                        STOP_REASON_TOOL_USE => StopReason::ToolUse,
                        unknown => {
                            debug!(stop_reason = unknown, "unknown Devin stop reason");
                            StopReason::EndTurn
                        }
                    };
                }

                if let Some(u) = response.usage {
                    usage = devin_usage_to_token_usage(&u);
                }
            }
        }

        let mut content_blocks = Vec::new();
        if !thinking.is_empty() || !signature.is_empty() {
            content_blocks.push(crate::types::ContentBlock::Thinking {
                thinking,
                signature: (!signature.is_empty()).then_some(signature),
            });
        }
        if !text.is_empty() {
            content_blocks.push(crate::types::ContentBlock::Text { text });
        }
        content_blocks.extend(ordered_tool_call_blocks(tool_calls, tool_call_order)?);
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
    use prost::Message as ProstMessage;

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

    #[test]
    fn devin_usage_maps_total_input_to_non_cached() {
        let stats = ModelUsageStats {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 10,
            cache_write_tokens: 10,
        };
        let usage = devin_usage_to_token_usage(&stats);
        assert_eq!(usage.input, 80);
        assert_eq!(usage.output, 50);
        assert_eq!(usage.cache_read, 10);
        assert_eq!(usage.cache_creation, 10);
        assert_eq!(usage.total_input(), 100);
    }

    #[test]
    fn devin_usage_preserves_input_when_cache_exceeds_total() {
        // Some responses report input_tokens as the non-cached remainder.
        // Keep the reported value instead of saturating to zero.
        let stats = ModelUsageStats {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 100,
            cache_write_tokens: 50,
        };
        let usage = devin_usage_to_token_usage(&stats);
        assert_eq!(usage.input, 10);
        assert_eq!(usage.cache_read, 100);
        assert_eq!(usage.cache_creation, 50);
        assert_eq!(usage.total_input(), 160);
    }

    #[test]
    fn devin_usage_handles_cache_equal_to_total_input() {
        let stats = ModelUsageStats {
            input_tokens: 50,
            output_tokens: 10,
            cache_read_tokens: 30,
            cache_write_tokens: 20,
        };
        let usage = devin_usage_to_token_usage(&stats);
        assert_eq!(usage.input, 0);
        assert_eq!(usage.cache_read, 30);
        assert_eq!(usage.cache_creation, 20);
        assert_eq!(usage.total_input(), 50);
    }

    #[test]
    fn devin_usage_with_no_cache() {
        let stats = ModelUsageStats {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let usage = devin_usage_to_token_usage(&stats);
        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 50);
        assert_eq!(usage.cache_read, 0);
        assert_eq!(usage.cache_creation, 0);
        assert_eq!(usage.total_input(), 100);
    }

    #[test]
    fn encode_devin_tools_uses_input_schema() {
        let tools = serde_json::json!([{
            "name": "read",
            "description": "Read a file",
            "input_schema": {"type": "object"}
        }]);

        let encoded = encode_devin_tools(&tools).expect("encode tools");
        assert_eq!(encoded.len(), 1);
        assert!(
            encoded[0]
                .windows(br#"{"type":"object"}"#.len())
                .any(|window| window == br#"{"type":"object"}"#)
        );
    }

    #[test]
    fn merge_tool_call_appends_argument_deltas() {
        let mut tool_calls = std::collections::HashMap::new();
        assert!(merge_tool_call(
            &mut tool_calls,
            ChatToolCall {
                id: "call-1".to_string(),
                name: "read".to_string(),
                arguments_json: "{\"path\":\"".to_string(),
            },
        ));
        assert!(!merge_tool_call(
            &mut tool_calls,
            ChatToolCall {
                id: "call-1".to_string(),
                name: String::new(),
                arguments_json: "src/lib.rs\"}".to_string(),
            },
        ));

        assert_eq!(
            tool_calls.get("call-1"),
            Some(&(
                String::from("read"),
                String::from("{\"path\":\"src/lib.rs\"}")
            ))
        );
    }

    const CASCADE_ID: &str = "cascade-1";
    const TRAILER_ERROR: &str = "Devin stream failed with trailer code unavailable";
    const TRAILER_JSON_ERROR: &str = "invalid Devin end-stream trailer JSON";

    fn prompt_string_field(prompt: &[u8], field_number: u64) -> Option<String> {
        let msg = crate::providers::devin_proto::ChatMessagePrompt::decode(prompt).ok()?;
        match field_number {
            1 => Some(msg.message_id),
            3 => Some(msg.prompt),
            7 => Some(msg.tool_call_id),
            _ => None,
        }
    }

    #[test]
    fn credentials_file_distinguishes_absent_unreadable_and_malformed() {
        let temp_dir = tempfile::tempdir().expect("temp directory");
        let absent = temp_dir.path().join("absent.toml");
        assert!(
            DevinCredentials::from_path(&absent)
                .expect("absent credentials are optional")
                .is_none()
        );

        let malformed = temp_dir.path().join("malformed.toml");
        std::fs::write(&malformed, "windsurf_api_key = [").expect("write malformed credentials");
        assert!(matches!(
            DevinCredentials::from_path(&malformed),
            Err(AgentError::Config { message }) if message.contains("failed to parse Devin credentials")
        ));

        assert!(matches!(
            DevinCredentials::from_path(temp_dir.path()),
            Err(AgentError::Config { message }) if message.contains("failed to read Devin credentials")
        ));
    }

    #[test]
    fn explicit_api_server_url_takes_precedence() {
        assert_eq!(
            resolve_api_server_url(
                "https://configured.example".to_string(),
                Some("https://explicit.example")
            ),
            "https://explicit.example"
        );
    }

    #[test]
    fn configured_api_server_url_is_preserved_without_explicit_url() {
        assert_eq!(
            resolve_api_server_url("https://configured.example".to_string(), None),
            "https://configured.example"
        );
    }

    #[test]
    fn chat_message_ids_are_stable_and_keep_bot_prefix() {
        assert_eq!(
            chat_message_id(CASCADE_ID, 2, "assistant"),
            "bot-cascade-1-2-assistant"
        );
        assert_eq!(
            chat_message_id(CASCADE_ID, 2, "user-0"),
            chat_message_id(CASCADE_ID, 2, "user-0")
        );
    }

    #[test]
    fn encode_devin_tools_rejects_non_array() {
        assert!(matches!(
            encode_devin_tools(&serde_json::json!({"name": "read"})),
            Err(AgentError::Config { message }) if message == "Devin tools must be an array"
        ));
    }

    #[test]
    fn encode_devin_tools_accepts_null_as_empty() {
        let encoded = encode_devin_tools(&serde_json::json!(null)).expect("null tools");
        assert!(encoded.is_empty());
    }

    #[test]
    fn encode_devin_tools_accepts_empty_object_as_empty() {
        let encoded = encode_devin_tools(&serde_json::json!({})).expect("empty object tools");
        assert!(encoded.is_empty());
    }

    #[test]
    fn encode_devin_tools_accepts_empty_array_as_empty() {
        let encoded = encode_devin_tools(&serde_json::json!([])).expect("empty array tools");
        assert!(encoded.is_empty());
    }

    #[test]
    fn model_max_tokens_are_not_capped_by_fallback() {
        assert_eq!(max_tokens_for_model(Some(128_000)), 128_000);
        assert_eq!(max_tokens_for_model(None), u64::from(DEFAULT_MAX_TOKENS));
    }

    #[test]
    fn trailer_parser_accepts_success_and_rejects_sanitized_error() {
        assert_eq!(
            parse_devin_trailer(br#"{"code":"ok","message":"private"}"#)
                .expect("successful trailer"),
            Some("ok".to_string())
        );
        let error = parse_devin_trailer(br#"{"code":"unavailable","message":"private"}"#)
            .expect_err("error trailer");
        assert!(matches!(
            error,
            AgentError::Api { status: 0, message } if message == TRAILER_ERROR
        ));

        let nested_error = parse_devin_trailer(
            br#"{"error":{"code":"unavailable","message":"nested private payload"}}"#,
        )
        .expect_err("nested error trailer");
        assert!(matches!(
            nested_error,
            AgentError::Api { status: 0, message } if message == TRAILER_ERROR
        ));

        let malicious = parse_devin_trailer(br#"{"code":"bad token: secret"}"#)
            .expect_err("invalid code is rejected");
        assert!(matches!(
            malicious,
            AgentError::Api { status: 0, message }
                if message == "Devin stream failed with trailer code invalid"
        ));
    }

    #[test]
    fn trailer_parser_rejects_malformed_json_without_echoing_payload() {
        let error = parse_devin_trailer(b"secret raw payload").expect_err("malformed trailer");
        assert!(matches!(
            error,
            AgentError::Api { status: 0, message } if message == TRAILER_JSON_ERROR
        ));
    }

    #[test]
    fn tool_result_splits_surrounding_user_text_into_stable_prompts() {
        let messages = [Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "before".to_string(),
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call-1".to_string(),
                    content: "result".to_string(),
                    is_error: false,
                },
                ContentBlock::Text {
                    text: "after".to_string(),
                },
            ],
            display_text: None,
            control: false,
        }];

        let prompts = encode_devin_chat_message_prompts(&messages, CASCADE_ID)
            .expect("encode message prompts");
        assert_eq!(prompts.len(), 3);
        assert_eq!(
            prompt_string_field(&prompts[0], 1).as_deref(),
            Some("cascade-1-0-user-0")
        );
        assert_eq!(
            prompt_string_field(&prompts[0], 3).as_deref(),
            Some("before")
        );
        assert_eq!(
            prompt_string_field(&prompts[1], 1).as_deref(),
            Some("cascade-1-0-tool-call-1")
        );
        assert_eq!(
            prompt_string_field(&prompts[1], 7).as_deref(),
            Some("call-1")
        );
        assert_eq!(
            prompt_string_field(&prompts[2], 1).as_deref(),
            Some("cascade-1-0-user-1")
        );
        assert_eq!(
            prompt_string_field(&prompts[2], 3).as_deref(),
            Some("after")
        );
    }

    #[test]
    fn file_reference_is_rendered_as_omitted_marker() {
        let messages = [Message {
            role: Role::User,
            content: vec![ContentBlock::File {
                source: crate::types::FileSource::file_id("file-123", None),
            }],
            ..Default::default()
        }];

        let prompts = encode_devin_chat_message_prompts(&messages, CASCADE_ID)
            .expect("encode message prompts");
        assert_eq!(prompts.len(), 1);
        assert_eq!(
            prompt_string_field(&prompts[0], 3).as_deref(),
            Some("[file omitted: file-123]")
        );
    }

    #[test]
    fn ordered_tool_call_blocks_follow_first_arrival_order() {
        let tool_calls = HashMap::from([
            (
                "second".to_string(),
                ("write".to_string(), "{}".to_string()),
            ),
            ("first".to_string(), ("read".to_string(), "{}".to_string())),
        ]);
        let blocks =
            ordered_tool_call_blocks(tool_calls, vec!["first".to_string(), "second".to_string()])
                .expect("ordered tool blocks");

        assert!(matches!(&blocks[0], ContentBlock::ToolUse { id, .. } if id == "first"));
        assert!(matches!(&blocks[1], ContentBlock::ToolUse { id, .. } if id == "second"));
    }
}
