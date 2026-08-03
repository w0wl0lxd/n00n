use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use flume::Sender;
use futures_lite::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
use isahc::{AsyncReadResponseExt, HttpClient, Request};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, warn};

use super::ResolvedAuth;
use crate::types::{ImageDetail, TOOL_RESULT_ERROR_PREFIX};
use crate::{
    AgentError, CacheControl, ContentBlock, Message, ProviderEvent, RequestDeliveryMetadata,
    RequestDeliveryPhase, RequestOptions, Role, StopReason, StreamResponse, System, TokenUsage,
};

const STREAM_DONE: &str = "[DONE]";

fn contains_prompt_cache_breakpoint(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_prompt_cache_breakpoint),
        Value::Object(object) => {
            object.contains_key("prompt_cache_breakpoint")
                || object.values().any(contains_prompt_cache_breakpoint)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn value_hash(value: &Value) -> u64 {
    /// Writes bytes directly into the underlying `Hasher`, avoiding the
    /// intermediate string allocations of the recursive implementation.
    struct HashWriter<'a>(&'a mut DefaultHasher);

    impl std::io::Write for HashWriter<'_> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut hasher = DefaultHasher::new();
    if let Err(e) = serde_json::to_writer(HashWriter(&mut hasher), value) {
        warn!(error = %e, "failed to hash value; using partial hash");
    }
    hasher.finish()
}

#[derive(Copy, Clone)]
pub(crate) struct OpenAiCompatConfig {
    pub slug: &'static str,
    pub api_key_env: &'static str,
    pub base_url: &'static str,
    pub max_tokens_field: &'static str,
    pub include_stream_usage: bool,
    pub provider_name: &'static str,
    pub supports_prompt_cache_key: bool,
    pub supports_prompt_cache_breakpoint: bool,
    pub emit_reasoning_content: bool,
    pub supports_parallel_tool_calls: bool,
}

pub(crate) struct OpenAiCompatProvider {
    client: HttpClient,
    config: &'static OpenAiCompatConfig,
    stream_timeout: Duration,
    cached_tools: Mutex<Option<(u64, Value)>>,
}

impl OpenAiCompatProvider {
    pub fn new(
        config: &'static OpenAiCompatConfig,
        timeouts: super::Timeouts,
    ) -> Result<Self, AgentError> {
        Ok(Self {
            client: super::http_client(timeouts)?,
            config,
            stream_timeout: timeouts.stream,
            cached_tools: Mutex::new(None),
        })
    }

    pub(crate) fn client(&self) -> &HttpClient {
        &self.client
    }

    pub(crate) fn config(&self) -> &'static OpenAiCompatConfig {
        self.config
    }

    pub(crate) fn stream_timeout(&self) -> Duration {
        self.stream_timeout
    }

    pub(crate) async fn get_text(
        &self,
        auth: &ResolvedAuth,
        url: &str,
    ) -> Result<String, AgentError> {
        let request = auth
            .configure_request(
                Request::builder()
                    .method("GET")
                    .uri(url)
                    .header("user-agent", super::user_agent()),
            )
            .body(())?;
        let mut response = self.client.send_async(request).await?;
        if response.status().as_u16() != 200 {
            return Err(AgentError::from_response(response).await);
        }
        Ok(response.text().await?)
    }

    pub(crate) async fn post_text(
        &self,
        auth: &ResolvedAuth,
        url: &str,
        content_type: &str,
        body: &[u8],
    ) -> Result<String, AgentError> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(url)
            .header("user-agent", super::user_agent());
        for (key, value) in &auth.headers {
            builder = builder.header(key.as_str(), value.as_str());
        }
        let request = builder
            .header("content-type", content_type)
            .body(body.to_vec())?;
        let mut response = self.client.send_async(request).await?;
        if response.status().as_u16() != 200 {
            return Err(AgentError::from_response(response).await);
        }
        Ok(response.text().await?)
    }

    fn wire_tools(&self, tools: &Value) -> Value {
        if tools.as_array().is_none_or(std::vec::Vec::is_empty) {
            return json!([]);
        }

        let key = value_hash(tools);
        {
            let guard = self
                .cached_tools
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((cached_key, cached_tools)) = guard.as_ref()
                && *cached_key == key
            {
                return cached_tools.clone();
            }
        }

        let converted = convert_tools(tools);
        *self
            .cached_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((key, converted.clone()));
        converted
    }

    pub fn build_body_with_session(
        &self,
        model: &crate::model::Model,
        messages: &[Message],
        system: &System,
        tools: &Value,
        session_id: Option<&str>,
        system_prefix: Option<&str>,
        message_cache_breakpoints: usize,
        fast: bool,
    ) -> Value {
        let supports_breakpoints = self.config.supports_prompt_cache_breakpoint
            && model.supports_prompt_cache_breakpoint();
        let message_cache_breakpoints = (supports_breakpoints && message_cache_breakpoints > 0)
            .then_some(message_cache_breakpoints);

        let mut wire_messages = convert_messages_with_breakpoints(
            messages,
            None,
            self.config.emit_reasoning_content,
            message_cache_breakpoints,
        );
        if let Some(system_message) = self.build_system_message(system, system_prefix, model) {
            wire_messages.insert(0, system_message);
        }
        let has_explicit_breakpoint = wire_messages.iter().any(contains_prompt_cache_breakpoint);
        let wire_tools = self.wire_tools(tools);

        let mut body = json!({
            "model": model.id,
            "messages": wire_messages,
            "stream": true,
        });
        if let Some(max_output) = model.max_output_tokens {
            body[self.config.max_tokens_field] = json!(max_output);
        }
        if self.config.include_stream_usage {
            body["stream_options"] = json!({"include_usage": true});
        }
        if wire_tools.as_array().is_some_and(|a| !a.is_empty()) {
            body["tools"] = wire_tools;
            if self.config.supports_parallel_tool_calls {
                body["parallel_tool_calls"] = json!(true);
            }
        }
        if let Some(sid) = session_id
            && self.config.supports_prompt_cache_key
        {
            body["prompt_cache_key"] = json!(sid);
        }
        if supports_breakpoints && has_explicit_breakpoint {
            body["prompt_cache_options"] = json!({"mode": "explicit"});
        }
        if fast && model.supports_fast() {
            body["service_tier"] = json!("fast");
        }
        body
    }

    fn build_system_message(
        &self,
        system: &System,
        prefix: Option<&str>,
        model: &crate::model::Model,
    ) -> Option<Value> {
        let mut blocks: Vec<(&str, bool)> = Vec::new();
        if let Some(prefix) = prefix
            && !prefix.is_empty()
        {
            blocks.push((prefix, false));
        }
        for block in system.blocks() {
            blocks.push((
                block.text.as_str(),
                block.cache == CacheControl::Ephemeral
                    && self.config.supports_prompt_cache_breakpoint
                    && model.supports_prompt_cache_breakpoint(),
            ));
        }
        if blocks.is_empty() {
            return None;
        }
        if blocks.iter().any(|(_, breakpoint)| *breakpoint) {
            Some(json!({
                "role": "system",
                "content": blocks.iter().map(|(text, breakpoint)| {
                    let mut obj = json!({"type": "text", "text": text});
                    if *breakpoint {
                        obj["prompt_cache_breakpoint"] = json!({"mode": "explicit"});
                    }
                    obj
                }).collect::<Vec<Value>>()
            }))
        } else {
            let text = blocks.iter().map(|(text, _)| *text).collect::<String>();
            Some(json!({"role": "system", "content": text}))
        }
    }

    /// Effective base URL: an auth-supplied value (dynamic/custom providers)
    /// wins, then the `<SLUG>_BASE_URL` env override, then the static default.
    fn base_url(&self, auth: &ResolvedAuth) -> String {
        if let Some(explicit) = auth.base_url.as_deref() {
            return explicit.to_string();
        }
        n00n_config::providers::base_url_override(self.config.slug)
            .unwrap_or_else(|| self.config.base_url.to_string())
    }

    fn build_request(
        &self,
        method: &str,
        path: &str,
        auth: &ResolvedAuth,
    ) -> isahc::http::request::Builder {
        let base = self.base_url(auth);
        auth.configure_request(
            Request::builder()
                .method(method)
                .uri(format!("{base}{path}"))
                .header("user-agent", super::user_agent()),
        )
    }

    pub async fn do_stream(
        &self,
        model: &crate::model::Model,
        extra_headers: &[(&str, &str)],
        body: &Value,
        event_tx: &Sender<ProviderEvent>,
        auth: &ResolvedAuth,
        opts: &RequestOptions,
    ) -> Result<StreamResponse, AgentError> {
        let json_body = serde_json::to_vec(body)?;
        let mut request = self
            .build_request("POST", "/chat/completions", auth)
            .header("content-type", "application/json");
        if let Some(key) = opts.idempotency_key.as_deref() {
            request = request.header("Idempotency-Key", key);
        }
        for &(key, value) in extra_headers {
            request = request.header(key, value);
        }

        let request = request.body(json_body)?;

        debug!(
            model = %model.id,
            provider = self.config.provider_name,
            "sending API request"
        );

        let response = self.client.send_async(request).await?;
        let status = response.status().as_u16();

        if status == 200 {
            parse_sse(
                BufReader::new(response.into_body()),
                event_tx,
                self.stream_timeout,
                opts,
            )
            .await
        } else {
            Err(AgentError::from_response(response).await)
        }
    }

    pub async fn fetch_and_parse_models(
        &self,
        auth: &ResolvedAuth,
        parse_fn: impl Fn(&Value) -> Option<crate::model::ModelInfo>,
    ) -> Result<Vec<crate::model::ModelInfo>, AgentError> {
        let base = auth
            .base_url
            .as_deref()
            .unwrap_or_else(|| self.config.base_url);
        let url = format!("{base}/models");
        let body_text = self.get_text(auth, &url).await?;
        let body: Value = serde_json::from_str(&body_text)?;

        let mut models: Vec<crate::model::ModelInfo> = body["data"]
            .as_array()
            .map_or_else(Default::default, |arr| {
                arr.iter().filter_map(parse_fn).collect()
            });
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    fn default_model_parser(m: &Value) -> Option<crate::model::ModelInfo> {
        let id = m["id"].as_str()?;
        let context_window = m["context_length"]
            .as_u64()
            .or_else(|| m["max_model_len"].as_u64())
            .or_else(|| m["max_context_length"].as_u64())
            .and_then(|v| u32::try_from(v).ok());
        let max_output_tokens = m["max_tokens"].as_u64().and_then(|v| u32::try_from(v).ok());
        let pricing = m["pricing"]
            .as_object()
            .and_then(|p| {
                Some(crate::model::ModelPricing {
                    input: p.get("prompt")?.as_str()?.parse().ok()?,
                    output: p.get("completion")?.as_str()?.parse().ok()?,
                    cache_write: p
                        .get("cache_creation")?
                        .as_str()?
                        .parse::<f64>()
                        .ok()
                        .unwrap_or_else(|| 0.0),
                    cache_read: p
                        .get("cache_read")?
                        .as_str()?
                        .parse::<f64>()
                        .ok()
                        .unwrap_or_else(|| 0.0),
                    fast: None,
                })
            })
            .unwrap_or_else(Default::default);
        Some(crate::model::ModelInfo {
            id: id.to_string(),
            name: None,
            context_window,
            max_output_tokens,
            pricing: Some(pricing),
            supports_thinking: None,
            supports_vision: None,
            tier: None,
            is_free: None,
            is_promo: None,
            provider_info: None,
        })
    }

    pub async fn do_list_models(
        &self,
        auth: &ResolvedAuth,
    ) -> Result<Vec<crate::model::ModelInfo>, AgentError> {
        self.fetch_and_parse_models(auth, Self::default_model_parser)
            .await
    }
}

pub fn convert_messages(
    messages: &[Message],
    system: Option<&str>,
    emit_reasoning_content: bool,
) -> Vec<Value> {
    convert_messages_with_breakpoints(messages, system, emit_reasoning_content, None)
}

pub fn convert_messages_with_breakpoints(
    messages: &[Message],
    system: Option<&str>,
    emit_reasoning_content: bool,
    message_cache_breakpoints: Option<usize>,
) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(system) = system {
        out.push(json!({"role": "system", "content": system}));
    }

    // Compute breakpoint indices if requested
    let breakpoints = if let Some(num_breakpoints) = message_cache_breakpoints {
        let mut bp_set = std::collections::HashSet::new();

        // Find user message indices (in reverse order for last N)
        let user_message_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| matches!(m.role, Role::User))
            .map(|(i, _)| i)
            .collect();

        for idx in user_message_indices.iter().rev().take(num_breakpoints) {
            bp_set.insert((*idx, messages[*idx].content.len().saturating_sub(1)));
        }

        // Find last tool result block index
        let mut last_tool_result_idx = None;
        for (msg_idx, msg) in messages.iter().enumerate().rev() {
            for (block_idx, block) in msg.content.iter().enumerate().rev() {
                if matches!(block, ContentBlock::ToolResult { .. }) {
                    last_tool_result_idx = Some((msg_idx, block_idx));
                    break;
                }
            }
            if last_tool_result_idx.is_some() {
                break;
            }
        }

        if let Some((msg_idx, block_idx)) = last_tool_result_idx {
            bp_set.insert((msg_idx, block_idx));
        }

        Some(bp_set)
    } else {
        None
    };

    for (msg_idx, msg) in messages.iter().enumerate() {
        match msg.role {
            Role::User => {
                let mut tool_results = Vec::new();
                let mut text_parts: Vec<String> = Vec::new();
                let mut content_parts = Vec::new();
                let mut has_images = false;

                for (block_idx, block) in msg.content.iter().enumerate() {
                    match block {
                        ContentBlock::Text { text } => {
                            text_parts.push(text.clone());
                            content_parts.push(json!({"type": "text", "text": text}));
                        }
                        ContentBlock::Image { source } => {
                            if let Some(ref file_id) = source.file_id {
                                // Chat Completions cannot use file_id, emit a note
                                let text = format!("[image file omitted: {file_id}]");
                                text_parts.push(text.clone());
                                content_parts.push(json!({"type": "text", "text": text}));
                            } else {
                                let url = source
                                    .url
                                    .as_deref()
                                    .map_or_else(|| source.to_data_url(), ToString::to_string);
                                let detail = source.detail.map_or_else(
                                    || "auto".to_string(),
                                    |d| {
                                        if d == ImageDetail::Original {
                                            "auto".to_string()
                                        } else {
                                            d.to_string()
                                        }
                                    },
                                );
                                content_parts.push(json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": url,
                                        "detail": detail
                                    }
                                }));
                                has_images = true;
                            }
                        }
                        ContentBlock::File { source } => {
                            let identifier = source.identifier().unwrap_or_else(|| "unknown");
                            let text = format!("[file omitted: {identifier}]");
                            text_parts.push(text.clone());
                            content_parts.push(json!({"type": "text", "text": text}));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            let output =
                                if *is_error && !content.starts_with(TOOL_RESULT_ERROR_PREFIX) {
                                    format!("{TOOL_RESULT_ERROR_PREFIX}{content}")
                                } else {
                                    content.clone()
                                };
                            let mut tool_msg = json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": output,
                            });

                            if breakpoints
                                .as_ref()
                                .is_some_and(|bp| bp.contains(&(msg_idx, block_idx)))
                            {
                                tool_msg["content"] = json!([{
                                    "type": "text",
                                    "text": output,
                                    "prompt_cache_breakpoint": {"mode": "explicit"}
                                }]);
                            }

                            tool_results.push(tool_msg);
                        }
                        ContentBlock::ToolUse { .. }
                        | ContentBlock::Thinking { .. }
                        | ContentBlock::RedactedThinking { .. } => {}
                    }
                }

                // Tool messages must directly follow the assistant's
                // tool_calls, before any user content.
                out.extend(tool_results);

                // A trailing tool result gets its breakpoint on the generated
                // tool message above. All other emitted user content is eligible.
                let mark_user_breakpoint = msg.content.last().is_some_and(|last| {
                    matches!(
                        last,
                        ContentBlock::Text { .. }
                            | ContentBlock::Image { .. }
                            | ContentBlock::File { .. }
                    )
                }) && breakpoints
                    .as_ref()
                    .is_some_and(|bp| bp.contains(&(msg_idx, msg.content.len().saturating_sub(1))));

                if has_images {
                    if mark_user_breakpoint && let Some(last) = content_parts.last_mut() {
                        last["prompt_cache_breakpoint"] = json!({"mode": "explicit"});
                    }
                    out.push(json!({"role": "user", "content": content_parts}));
                } else if !text_parts.is_empty() {
                    let text = text_parts.join("\n");
                    if mark_user_breakpoint {
                        let content_array = json!([{
                            "type": "text",
                            "text": text,
                            "prompt_cache_breakpoint": {"mode": "explicit"}
                        }]);
                        out.push(json!({"role": "user", "content": content_array}));
                    } else {
                        out.push(json!({"role": "user", "content": text}));
                    }
                }
            }
            Role::Assistant => {
                let mut text = String::new();
                let mut reasoning_text = String::new();
                let mut tool_calls = Vec::new();

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text: t } => text.push_str(t),
                        ContentBlock::Thinking { thinking, .. } => {
                            reasoning_text.push_str(thinking);
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": input.to_string(),
                                }
                            }));
                        }
                        ContentBlock::ToolResult { .. }
                        | ContentBlock::Image { .. }
                        | ContentBlock::RedactedThinking { .. }
                        | ContentBlock::File { .. } => {}
                    }
                }

                if !text.is_empty() || !tool_calls.is_empty() || !reasoning_text.is_empty() {
                    let mut msg_obj = json!({"role": "assistant"});
                    if emit_reasoning_content {
                        // Emit reasoning_content as a separate field (Mistral)
                        if !reasoning_text.is_empty() {
                            msg_obj["reasoning_content"] = Value::String(reasoning_text.clone());
                        }
                        if !text.is_empty() {
                            msg_obj["content"] = Value::String(text);
                        } else if reasoning_text.is_empty() {
                            // No text and no reasoning - set empty content
                            msg_obj["content"] = Value::String(String::new());
                        }
                    } else {
                        // Current behavior: put reasoning in content if no text
                        if !text.is_empty() {
                            msg_obj["content"] = Value::String(text);
                        } else if !reasoning_text.is_empty() {
                            msg_obj["content"] = Value::String(reasoning_text);
                        }
                    }
                    if !tool_calls.is_empty() {
                        msg_obj["tool_calls"] = Value::Array(tool_calls);
                    }
                    out.push(msg_obj);
                }
            }
        }
    }

    out
}

pub fn convert_tools(anthropic_tools: &Value) -> Value {
    let Some(tools) = anthropic_tools.as_array() else {
        return json!([]);
    };

    Value::Array(
        tools
            .iter()
            .filter_map(|t| {
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": t.get("name")?,
                        "description": t.get("description")?,
                        "parameters": t.get("input_schema")?,
                    }
                }))
            })
            .collect(),
    )
}

#[derive(Deserialize)]
struct ToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ChunkDelta {
    content: Option<ContentDelta>,
    #[serde(alias = "reasoning")]
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum ContentDelta {
    Array(Vec<ContentDeltaPart>),
    String(String),
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ContentDeltaPart {
    Text { text: String },
    Thinking { thinking: Vec<ThinkingDelta> },
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum ThinkingDelta {
    Block(ThinkingDeltaBlock),
    String(String),
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ThinkingDeltaBlock {
    Text { text: String },
}

#[derive(Deserialize)]
struct ChunkChoice {
    #[serde(alias = "message")]
    delta: Option<ChunkDelta>,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Clone)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
    #[serde(default)]
    cache_write_tokens: u32,
}

#[derive(Deserialize)]
struct ChunkUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u32>,
    #[serde(default)]
    prompt_cache_miss_tokens: Option<u32>,
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Deserialize)]
struct SseChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    usage: Option<ChunkUsage>,
}

struct ToolAccumulator {
    id: String,
    name: String,
    arguments: String,
}

#[allow(clippy::too_many_lines)]
pub async fn parse_sse(
    reader: impl AsyncBufRead + Unpin,
    event_tx: &Sender<ProviderEvent>,
    stream_timeout: Duration,
    opts: &RequestOptions,
) -> Result<StreamResponse, AgentError> {
    let mut lines = reader.lines();

    let mut text = String::new();
    let mut reasoning_text = String::new();
    let mut tool_accumulators: Vec<ToolAccumulator> = Vec::new();
    let mut usage = TokenUsage::default();
    let mut stop_reason: Option<StopReason> = None;
    let mut is_first_content = true;
    let mut emitted_event = false;
    let mut deadline = Instant::now() + stream_timeout;

    let idempotency_key = opts.idempotency_key.clone();
    let delivery_metadata = |emitted_event| {
        let mut metadata =
            RequestDeliveryMetadata::new(RequestDeliveryPhase::SentAwaitingAcceptance);
        metadata.idempotency_key.clone_from(&idempotency_key);
        metadata.emitted_event = emitted_event;
        metadata
    };

    loop {
        let line = match super::next_sse_line(&mut lines, &mut deadline, stream_timeout).await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                return Err(error.suppress_retry_after_send(Some(delivery_metadata(emitted_event))));
            }
        };

        let data = match line.strip_prefix("data:") {
            Some(d) => d.trim(),
            None => continue,
        };

        if data == STREAM_DONE {
            break;
        }

        if data.contains("\"error\"")
            && let Ok(ev) = serde_json::from_str::<super::SseErrorPayload>(data)
        {
            warn!(error_type = %ev.error.r#type, message = %ev.error.message, "SSE error in stream");
            return Err(ev
                .into_agent_error()
                .suppress_retry_after_send(Some(delivery_metadata(emitted_event))));
        }

        let chunk: SseChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "failed to parse SSE chunk");
                continue;
            }
        };

        if let Some(u) = chunk.usage {
            let (cache_read, input) = if let Some(hit_tokens) = u.prompt_cache_hit_tokens {
                let miss_tokens = u
                    .prompt_cache_miss_tokens
                    .unwrap_or_else(|| u.prompt_tokens.saturating_sub(hit_tokens));
                (hit_tokens, miss_tokens)
            } else {
                let cached = u
                    .prompt_tokens_details
                    .as_ref()
                    .map_or(0, |d| d.cached_tokens);
                let cache_write = u
                    .prompt_tokens_details
                    .as_ref()
                    .map_or(0, |d| d.cache_write_tokens);
                (
                    cached,
                    u.prompt_tokens
                        .saturating_sub(cached)
                        .saturating_sub(cache_write),
                )
            };
            let cache_write = u
                .prompt_tokens_details
                .as_ref()
                .map_or(0, |d| d.cache_write_tokens);
            usage = TokenUsage {
                input,
                output: u.completion_tokens,
                cache_read,
                cache_creation: cache_write,
            };
        }

        let Some(choice) = chunk.choices.into_iter().next() else {
            continue;
        };

        if let Some(reason) = choice.finish_reason {
            stop_reason = Some(StopReason::from_openai(&reason));
        }

        let Some(delta) = choice.delta else {
            continue;
        };

        if let Some(reasoning) = delta.reasoning_content
            && !reasoning.is_empty()
        {
            reasoning_text.push_str(&reasoning);
            emitted_event = true;
            event_tx
                .send_async(ProviderEvent::ThinkingDelta { text: reasoning })
                .await?;
        }

        match delta.content {
            Some(ContentDelta::String(content_str)) if !content_str.is_empty() => {
                let content = if is_first_content {
                    is_first_content = false;
                    content_str.trim_start().to_string()
                } else {
                    content_str
                };

                if !content.is_empty() {
                    text.push_str(&content);
                    emitted_event = true;
                    event_tx
                        .send_async(ProviderEvent::TextDelta { text: content })
                        .await?;
                }
            }
            Some(ContentDelta::Array(content_array)) => {
                for part in content_array {
                    match part {
                        ContentDeltaPart::Thinking { thinking } => {
                            for thinking_block in thinking {
                                let content = match thinking_block {
                                    ThinkingDelta::Block(ThinkingDeltaBlock::Text {
                                        text: content_str,
                                    })
                                    | ThinkingDelta::String(content_str) => content_str,
                                };

                                if content.is_empty() {
                                    continue;
                                }

                                reasoning_text.push_str(&content);
                                emitted_event = true;
                                event_tx
                                    .send_async(ProviderEvent::ThinkingDelta { text: content })
                                    .await?;
                            }
                        }
                        ContentDeltaPart::Text { text: content_str } => {
                            let content = if is_first_content {
                                is_first_content = false;
                                content_str.trim_start().to_string()
                            } else {
                                content_str
                            };

                            if !content.is_empty() {
                                text.push_str(&content);
                                emitted_event = true;
                                event_tx
                                    .send_async(ProviderEvent::TextDelta { text: content })
                                    .await?;
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        if let Some(tc_deltas) = delta.tool_calls {
            for tc in tc_deltas {
                while tool_accumulators.len() <= tc.index {
                    tool_accumulators.push(ToolAccumulator {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                }
                let acc = &mut tool_accumulators[tc.index];
                let was_unnamed = acc.name.is_empty();
                if let Some(id) = tc.id {
                    acc.id = id;
                }
                if let Some(func) = tc.function {
                    if let Some(name) = func.name {
                        acc.name = name;
                    }
                    if let Some(args) = func.arguments {
                        acc.arguments.push_str(&args);
                    }
                }
                if was_unnamed && !acc.name.is_empty() {
                    emitted_event = true;
                    event_tx
                        .send_async(ProviderEvent::ToolUseStart {
                            id: acc.id.clone(),
                            name: acc.name.clone(),
                        })
                        .await?;
                }
            }
        }
    }

    let mut content_blocks: Vec<ContentBlock> = Vec::new();

    if !reasoning_text.is_empty() {
        content_blocks.push(ContentBlock::Thinking {
            thinking: reasoning_text,
            signature: None,
        });
    }

    if !text.is_empty() {
        content_blocks.push(ContentBlock::Text { text });
    }

    for (idx, acc) in tool_accumulators.into_iter().enumerate() {
        let input: Value = match serde_json::from_str(&acc.arguments) {
            Ok(v) => {
                debug!(tool = %acc.name, json = %acc.arguments, "tool input JSON");
                v
            }
            Err(e) => {
                warn!(error = %e, tool = %acc.name, json = %acc.arguments, "malformed tool JSON, falling back to {{}}");
                Value::Object(serde_json::Map::default())
            }
        };
        let id = if acc.id.is_empty() {
            warn!(raw_name = %acc.name, raw_args = %acc.arguments, "provider sent empty tool_use id; substituting placeholder");
            format!("n00n_unnamed_{idx}")
        } else {
            acc.id
        };
        let name = if acc.name.is_empty() {
            warn!(%id, raw_args = %acc.arguments, "provider sent empty tool_use name; substituting placeholder");
            "n00n_unknown_tool".to_owned()
        } else {
            acc.name
        };
        content_blocks.push(ContentBlock::ToolUse { id, name, input });
    }

    Ok(StreamResponse {
        message: Message {
            role: Role::Assistant,
            content: content_blocks,
            ..Default::default()
        },
        usage,
        stop_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::io::Cursor;
    use test_case::test_case;

    const TEST_STREAM_TIMEOUT: Duration = Duration::from_mins(5);

    const BREAKPOINT_TEST_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
        slug: "test",
        api_key_env: "TEST_KEY",
        base_url: "https://test.com",
        max_tokens_field: "max_tokens",
        include_stream_usage: false,
        provider_name: "Test",
        supports_prompt_cache_key: false,
        supports_prompt_cache_breakpoint: true,
        emit_reasoning_content: false,
        supports_parallel_tool_calls: false,
    };

    #[test]
    fn post_response_transport_error_is_not_retried() {
        let error = AgentError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "stream ended",
        ));
        let metadata = RequestDeliveryMetadata::new(RequestDeliveryPhase::SentAwaitingAcceptance);

        assert!(matches!(
            error.suppress_retry_after_send(Some(metadata)),
            AgentError::RequestSent { .. }
        ));
    }

    #[test]
    fn parse_sse_text_and_usage() {
        smol::block_on(async {
            let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\
\n\
data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\
\n\
data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":10,\"prompt_tokens_details\":{\"cached_tokens\":40}}}\n\
\n\
data: [DONE]\n";

            let (tx, rx) = flume::unbounded();
            let resp = parse_sse(
                Cursor::new(sse.as_bytes()),
                &tx,
                TEST_STREAM_TIMEOUT,
                &RequestOptions::default(),
            )
            .await
            .unwrap();

            assert_eq!(resp.usage.input, 60);
            assert_eq!(resp.usage.output, 10);
            assert_eq!(resp.usage.cache_read, 40);
            assert_eq!(resp.stop_reason, Some(StopReason::EndTurn));
            assert!(
                matches!(&resp.message.content[0], ContentBlock::Text { text } if text == "Hello world")
            );
            assert!(!resp.message.has_tool_calls());

            let mut deltas = Vec::new();
            while let Ok(e) = rx.try_recv() {
                if let ProviderEvent::TextDelta { text } = e {
                    deltas.push(text);
                }
            }
            assert_eq!(deltas, vec!["Hello", " world"]);
        });
    }

    #[test]
    fn parse_sse_with_cache_write_tokens() {
        smol::block_on(async {
            let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\
|\n\
data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":10,\"prompt_tokens_details\":{\"cached_tokens\":30,\"cache_write_tokens\":20}}}\n\
|\n\
data: [DONE]\n";

            let (tx, _rx) = flume::unbounded();
            let resp = parse_sse(
                Cursor::new(sse.as_bytes()),
                &tx,
                TEST_STREAM_TIMEOUT,
                &RequestOptions::default(),
            )
            .await
            .unwrap();

            assert_eq!(resp.usage.input, 50);
            assert_eq!(resp.usage.output, 10);
            assert_eq!(resp.usage.cache_read, 30);
            assert_eq!(resp.usage.cache_creation, 20);
        });
    }

    #[test]
    fn parse_sse_reasoning_and_content() {
        smol::block_on(async {
            let sse = "\
data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Let me think\"}}]}\n\
\n\
data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"...\"}}]}\n\
\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\
\n\
data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\
\n\
data: [DONE]\n";

            let (tx, rx) = flume::unbounded();
            let resp = parse_sse(
                Cursor::new(sse.as_bytes()),
                &tx,
                TEST_STREAM_TIMEOUT,
                &RequestOptions::default(),
            )
            .await
            .unwrap();

            assert!(
                matches!(&resp.message.content[0], ContentBlock::Thinking { thinking, .. } if thinking == "Let me think...")
            );
            assert!(
                matches!(&resp.message.content[1], ContentBlock::Text { text } if text == "Hello")
            );

            let mut thinking = Vec::new();
            let mut text_deltas = Vec::new();
            while let Ok(e) = rx.try_recv() {
                match e {
                    ProviderEvent::ThinkingDelta { text } => thinking.push(text),
                    ProviderEvent::TextDelta { text } => text_deltas.push(text),
                    ProviderEvent::ToolUseStart { .. }
                    | ProviderEvent::PromptProgress { .. }
                    | ProviderEvent::CacheHealth { .. } => {}
                }
            }
            assert_eq!(thinking, vec!["Let me think", "..."]);
            assert_eq!(text_deltas, vec!["Hello"]);
        });
    }

    #[test]
    fn convert_messages_structure() {
        let messages = vec![
            Message::user("hello".to_string()),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "thinking...".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: "tc_1".to_string(),
                        name: "bash".to_string(),
                        input: json!({"command": "ls"}),
                    },
                ],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tc_1".to_string(),
                    content: "file.txt".to_string(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];

        let wire = convert_messages(&messages, Some("be helpful"), false);

        assert_eq!(wire[0]["role"], "system");
        assert_eq!(wire[0]["content"], "be helpful");
        assert_eq!(wire[1]["role"], "user");
        assert_eq!(wire[1]["content"], "hello");
        assert_eq!(wire[2]["role"], "assistant");
        assert_eq!(wire[2]["content"], "thinking...");
        assert_eq!(wire[2]["tool_calls"][0]["id"], "tc_1");
        assert_eq!(wire[2]["tool_calls"][0]["type"], "function");
        assert_eq!(wire[2]["tool_calls"][0]["function"]["name"], "bash");
        assert_eq!(wire[3]["role"], "tool");
        assert_eq!(wire[3]["tool_call_id"], "tc_1");
        assert_eq!(wire[3]["content"], "file.txt");
    }

    #[test]
    fn convert_tools_structure() {
        let anthropic = json!([{
            "name": "bash",
            "description": "Run a command",
            "input_schema": {
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }
        }]);

        let openai = convert_tools(&anthropic);
        let tool = &openai[0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "bash");
        assert_eq!(tool["function"]["description"], "Run a command");
        assert_eq!(tool["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn parse_sse_multiple_parallel_tool_calls() {
        smol::block_on(async {
            let sse = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]}}]}\n\
\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]}}]}\n\
\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"command\\\": \\\"ls\\\"}\"}}]}}]}\n\
\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"{\\\"path\\\": \\\"/tmp\\\"}\"}}]}}]}\n\
\n\
data: {\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3}}\n\
\n\
data: [DONE]\n";

            let (tx, rx) = flume::unbounded();
            let resp = parse_sse(
                Cursor::new(sse.as_bytes()),
                &tx,
                TEST_STREAM_TIMEOUT,
                &RequestOptions::default(),
            )
            .await
            .unwrap();

            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 2);
            assert_eq!(tools[0].0, "c1");
            assert_eq!(tools[0].1, "bash");
            assert_eq!(tools[0].2["command"], "ls");
            assert_eq!(tools[1].0, "c2");
            assert_eq!(tools[1].1, "read");
            assert_eq!(tools[1].2["path"], "/tmp");
            assert_eq!(resp.stop_reason, Some(StopReason::ToolUse));

            let starts: Vec<_> = rx
                .drain()
                .filter_map(|e| match e {
                    ProviderEvent::ToolUseStart { id, name } => Some((id, name)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                starts,
                vec![("c1".into(), "bash".into()), ("c2".into(), "read".into()),]
            );
        });
    }

    #[test]
    fn parse_sse_error_payload_returns_err() {
        smol::block_on(async {
            let sse = "\
data: {\"error\":{\"message\":\"Server overloaded\",\"type\":\"overloaded_error\"}}\n";

            let (tx, _rx) = flume::unbounded();
            let err = parse_sse(
                Cursor::new(sse.as_bytes()),
                &tx,
                TEST_STREAM_TIMEOUT,
                &RequestOptions::default(),
            )
            .await
            .unwrap_err();

            match err {
                AgentError::Api { status, message } => {
                    assert_eq!(status, 529);
                    assert_eq!(message, "Server overloaded");
                }
                other => panic!("expected Api error, got: {other:?}"),
            }
        });
    }

    #[test]
    fn parse_sse_text_error_after_partial_output_is_not_retryable() {
        smol::block_on(async {
            let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n\
data: {\"error\":{\"message\":\"Server overloaded\",\"type\":\"overloaded_error\"}}\n";

            let (tx, _rx) = flume::unbounded();
            let err = parse_sse(
                Cursor::new(sse.as_bytes()),
                &tx,
                TEST_STREAM_TIMEOUT,
                &RequestOptions::default(),
            )
            .await
            .unwrap_err();

            match err {
                AgentError::RequestSent {
                    metadata: Some(meta),
                    ..
                } => {
                    assert!(meta.emitted_event);
                    assert_eq!(
                        meta.phase,
                        crate::RequestDeliveryPhase::SentAwaitingAcceptance
                    );
                }
                other => panic!("expected RequestSent, got: {other:?}"),
            }
        });
    }

    #[test]
    fn parse_sse_tool_error_after_partial_output_is_not_retryable() {
        smol::block_on(async {
            let sse = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"bash\"}}]}}]}\n\n\
data: {\"error\":{\"message\":\"Server overloaded\",\"type\":\"overloaded_error\"}}\n";

            let (tx, _rx) = flume::unbounded();
            let err = parse_sse(
                Cursor::new(sse.as_bytes()),
                &tx,
                TEST_STREAM_TIMEOUT,
                &RequestOptions::default(),
            )
            .await
            .unwrap_err();

            match err {
                AgentError::RequestSent {
                    metadata: Some(meta),
                    ..
                } => {
                    assert!(meta.emitted_event);
                    assert_eq!(
                        meta.phase,
                        crate::RequestDeliveryPhase::SentAwaitingAcceptance
                    );
                }
                other => panic!("expected RequestSent, got: {other:?}"),
            }
        });
    }

    #[test]
    fn parse_sse_empty_tool_id_and_name_get_placeholders() {
        smol::block_on(async {
            let sse = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"tool_calls\\\":[{\\\"tool\\\":\\\"read\\\"}]}\"}}]}}]}\n\
\n\
data: {\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\
\n\
data: [DONE]\n";

            let (tx, _rx) = flume::unbounded();
            let resp = parse_sse(
                Cursor::new(sse.as_bytes()),
                &tx,
                TEST_STREAM_TIMEOUT,
                &RequestOptions::default(),
            )
            .await
            .unwrap();

            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 1);
            assert!(!tools[0].0.is_empty(), "id must be non-empty for Bedrock");
            assert!(!tools[0].1.is_empty(), "name must be non-empty for Bedrock");
        });
    }

    #[test]
    fn parse_sse_malformed_tool_json_yields_empty_object() {
        smol::block_on(async {
            let sse = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]}}]}\n\
\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{broken\"}}]}}]}\n\
\n\
data: {\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\
\n\
data: [DONE]\n";

            let (tx, _rx) = flume::unbounded();
            let resp = parse_sse(
                Cursor::new(sse.as_bytes()),
                &tx,
                TEST_STREAM_TIMEOUT,
                &RequestOptions::default(),
            )
            .await
            .unwrap();

            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].1, "bash");
            assert_eq!(*tools[0].2, Value::Object(Default::default()));
        });
    }

    #[test]
    fn convert_messages_user_with_image() {
        use crate::types::{ImageMediaType, ImageSource};
        use std::sync::Arc;
        let source = ImageSource::new(ImageMediaType::Png, Arc::from("abc123"));
        let msgs = vec![Message::user_with_images("describe".into(), vec![source])];
        let result = convert_messages(&msgs, Some("system"), false);
        let user = &result[1];
        let content = user["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "image_url");
        assert!(
            content[0]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "describe");
    }

    #[test]
    fn convert_messages_preserves_text_image_text_order() {
        use std::sync::Arc;

        use crate::types::{ImageMediaType, ImageSource};

        let messages = vec![Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "before".into(),
                },
                ContentBlock::Image {
                    source: ImageSource::new(ImageMediaType::Png, Arc::from("abc123")),
                },
                ContentBlock::Text {
                    text: "after".into(),
                },
            ],
            ..Default::default()
        }];

        let result = convert_messages(&messages, None, false);
        let content = result[0]["content"].as_array().unwrap();

        assert_eq!(content.len(), 3);
        assert_eq!(content[0], json!({"type": "text", "text": "before"}));
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[2], json!({"type": "text", "text": "after"}));
    }

    #[test]
    fn convert_messages_marks_trailing_file_breakpoint() {
        use crate::types::FileSource;

        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::File {
                source: FileSource::file_id("file_123", None),
            }],
            ..Default::default()
        }];

        let result = convert_messages_with_breakpoints(&messages, None, false, Some(1));
        let content = result[0]["content"].as_array().unwrap();

        assert_eq!(content[0]["text"], "[file omitted: file_123]");
        assert_eq!(
            content[0]["prompt_cache_breakpoint"],
            json!({"mode": "explicit"})
        );
    }

    #[test]
    fn convert_messages_tool_results_precede_tool_returned_image() {
        use crate::types::{ImageMediaType, ImageSource};
        use std::sync::Arc;
        let msgs = vec![Message {
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
        let result = convert_messages(&msgs, Some("system"), false);
        assert_eq!(result[1]["role"], "tool");
        assert_eq!(result[1]["tool_call_id"], "t1");
        assert_eq!(result[2]["role"], "user");
        assert_eq!(result[2]["content"][0]["type"], "image_url");
    }

    #[test]
    fn convert_messages_prefixes_error_tool_result() {
        let msgs = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "sub-agent error: API 500".into(),
                is_error: true,
            }],
            ..Default::default()
        }];
        let result = convert_messages(&msgs, None, false);
        let output = result[0]["content"].as_str().unwrap();
        assert!(output.starts_with(TOOL_RESULT_ERROR_PREFIX));
        assert!(output.contains("sub-agent error: API 500"));
    }

    #[test]
    fn convert_messages_user_text_only_stays_string() {
        let msgs = vec![Message::user("hello".into())];
        let result = convert_messages(&msgs, Some("system"), false);
        assert!(result[1]["content"].is_string());
    }

    #[test]
    fn convert_messages_assistant_with_reasoning() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "Let me think...".into(),
                    signature: None,
                },
                ContentBlock::Text {
                    text: "Hello".into(),
                },
            ],
            ..Default::default()
        }];
        let wire = convert_messages(&messages, Some(""), false);
        let asst = &wire[1];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["content"], "Hello");
        assert!(!asst.as_object().unwrap().contains_key("reasoning_content"));
    }

    #[test]
    fn convert_messages_assistant_reasoning_only() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                thinking: "Just thinking...".into(),
                signature: None,
            }],
            ..Default::default()
        }];
        let wire = convert_messages(&messages, Some(""), false);
        let asst = &wire[1];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["content"], "Just thinking...");
        assert!(!asst.as_object().unwrap().contains_key("reasoning_content"));
    }

    #[test]
    fn parse_sse_empty_stream() {
        smol::block_on(async {
            let sse = "data: [DONE]\n";
            let (tx, _rx) = flume::unbounded();
            let resp = parse_sse(
                Cursor::new(sse.as_bytes()),
                &tx,
                TEST_STREAM_TIMEOUT,
                &RequestOptions::default(),
            )
            .await
            .unwrap();
            assert!(resp.message.content.is_empty());
            assert_eq!(resp.usage, TokenUsage::default());
            assert_eq!(resp.stop_reason, None);
        });
    }

    #[test]
    fn parse_sse_content_as_array_with_thinking() {
        smol::block_on(async {
            // Test parsing content as an array with thinking blocks
            let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":[{\"type\":\"thinking\",\"thinking\":[{\"type\":\"text\",\"text\":\"Let me think\"}]}]}}]}\n\
\n\
data: {\"choices\":[{\"delta\":{\"content\":[{\"type\":\"thinking\",\"thinking\":[{\"type\":\"text\",\"text\":\"...\"}]}]}}]}\n\
\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\
\n\
data: [DONE]\n";

            let (tx, rx) = flume::unbounded();
            let resp = parse_sse(
                Cursor::new(sse.as_bytes()),
                &tx,
                TEST_STREAM_TIMEOUT,
                &RequestOptions::default(),
            )
            .await
            .unwrap();

            assert!(
                matches!(&resp.message.content[0], ContentBlock::Thinking { thinking, .. } if thinking == "Let me think..."),
                "{:?}",
                resp.message.content[0],
            );
            assert!(
                matches!(&resp.message.content[1], ContentBlock::Text { text } if text == "Hello")
            );

            let mut thinking_deltas = Vec::new();
            let mut text_deltas = Vec::new();
            while let Ok(e) = rx.try_recv() {
                match e {
                    ProviderEvent::ThinkingDelta { text } => thinking_deltas.push(text),
                    ProviderEvent::TextDelta { text } => text_deltas.push(text),
                    _ => {}
                }
            }

            assert_eq!(text_deltas, vec!["Hello"]);
            assert_eq!(thinking_deltas, vec!["Let me think", "..."]);
        });
    }

    #[test]
    fn build_body_with_session_adds_prompt_cache_key() {
        static TEST_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
            slug: "test",
            api_key_env: "TEST_KEY",
            base_url: "https://test.com",
            max_tokens_field: "max_tokens",
            include_stream_usage: false,
            provider_name: "Test",
            supports_prompt_cache_key: true,
            supports_prompt_cache_breakpoint: false,
            emit_reasoning_content: false,
            supports_parallel_tool_calls: false,
        };
        let provider =
            OpenAiCompatProvider::new(&TEST_CONFIG, crate::providers::Timeouts::default()).unwrap();
        let model = crate::model::Model::from_spec("openai/gpt-4o").unwrap();
        let messages = vec![Message::user("hello".to_string())];
        let tools = json!([]);

        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::from("system"),
            &tools,
            Some("session-123"),
            None,
            0,
            false,
        );

        assert_eq!(body["prompt_cache_key"], "session-123");
    }

    #[test]
    fn build_body_without_session_no_prompt_cache_key() {
        static TEST_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
            slug: "test",
            api_key_env: "TEST_KEY",
            base_url: "https://test.com",
            max_tokens_field: "max_tokens",
            include_stream_usage: false,
            provider_name: "Test",
            supports_prompt_cache_key: true,
            supports_prompt_cache_breakpoint: false,
            emit_reasoning_content: false,
            supports_parallel_tool_calls: false,
        };
        let provider =
            OpenAiCompatProvider::new(&TEST_CONFIG, crate::providers::Timeouts::default()).unwrap();
        let model = crate::model::Model::from_spec("openai/gpt-4o").unwrap();
        let messages = vec![Message::user("hello".to_string())];
        let tools = json!([]);

        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::from("system"),
            &tools,
            None,
            None,
            0,
            false,
        );

        assert!(body.get("prompt_cache_key").is_none());
    }

    #[test]
    fn build_body_adds_system_cache_breakpoint_and_explicit_mode_for_gpt_5_6() {
        static TEST_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
            slug: "test",
            api_key_env: "TEST_KEY",
            base_url: "https://test.com",
            max_tokens_field: "max_tokens",
            include_stream_usage: false,
            provider_name: "Test",
            supports_prompt_cache_key: false,
            supports_prompt_cache_breakpoint: true,
            emit_reasoning_content: false,
            supports_parallel_tool_calls: false,
        };
        let provider =
            OpenAiCompatProvider::new(&TEST_CONFIG, crate::providers::Timeouts::default()).unwrap();
        let model = crate::model::Model::from_spec("openai/gpt-5.6-luna").unwrap();
        let messages = vec![Message::user("hello".to_string())];
        let tools = json!([]);

        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::from("be helpful"),
            &tools,
            Some("session-123"),
            None,
            0,
            false,
        );

        let system_msg = &body["messages"][0];
        assert_eq!(system_msg["role"], "system");
        assert!(system_msg["content"].is_array());
        let content_array = system_msg["content"].as_array().unwrap();
        assert_eq!(content_array.len(), 1);
        assert_eq!(content_array[0]["type"], "text");
        assert_eq!(content_array[0]["text"], "be helpful");
        assert_eq!(
            content_array[0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );
        assert_eq!(body["prompt_cache_options"]["mode"], "explicit");
    }

    #[test]
    fn breakpoint_support_uses_normalized_model_version() {
        let open_router_model =
            crate::model::Model::from_spec("openrouter/openai/gpt-5.6-luna").unwrap();
        let future_model = crate::model::Model::from_spec("openai/gpt-6").unwrap();
        let codex_model = crate::model::Model::from_spec("openai/gpt-6-codex").unwrap();

        assert!(open_router_model.supports_prompt_cache_breakpoint());
        assert!(future_model.supports_prompt_cache_breakpoint());
        assert!(!codex_model.supports_prompt_cache_breakpoint());
    }

    #[test]
    fn build_body_omits_explicit_mode_without_a_breakpoint() {
        let provider = OpenAiCompatProvider::new(
            &BREAKPOINT_TEST_CONFIG,
            crate::providers::Timeouts::default(),
        )
        .unwrap();
        let model = crate::model::Model::from_spec("openai/gpt-5.6-luna").unwrap();
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "answer".to_string(),
            }],
            ..Default::default()
        }];

        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::default(),
            &json!([]),
            None,
            None,
            2,
            false,
        );

        assert!(body.get("prompt_cache_options").is_none());
    }

    #[test]
    fn build_body_with_session_adds_message_cache_breakpoints() {
        static TEST_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
            slug: "test",
            api_key_env: "TEST_KEY",
            base_url: "https://test.com",
            max_tokens_field: "max_tokens",
            include_stream_usage: false,
            provider_name: "Test",
            supports_prompt_cache_key: false,
            supports_prompt_cache_breakpoint: true,
            emit_reasoning_content: false,
            supports_parallel_tool_calls: false,
        };
        let provider =
            OpenAiCompatProvider::new(&TEST_CONFIG, crate::providers::Timeouts::default()).unwrap();
        let model = crate::model::Model::from_spec("openai/gpt-5.6-luna").unwrap();
        let messages = vec![
            Message::user("first message".to_string()),
            Message::user("second message".to_string()),
            Message::user("third message".to_string()),
        ];
        let tools = json!([]);

        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::from("be helpful"),
            &tools,
            None,
            None,
            2,
            false,
        );

        // Check that prompt_cache_options is set to explicit mode
        assert_eq!(body["prompt_cache_options"]["mode"], "explicit");

        // Check that the last 2 user messages have breakpoints
        let msgs = body["messages"].as_array().unwrap();
        // Skip system message (index 0)
        let user_msg_1 = &msgs[1];
        let user_msg_2 = &msgs[2];
        let user_msg_3 = &msgs[3];

        // First user message should NOT have a breakpoint (only last 2)
        assert!(user_msg_1["content"].is_string());

        // Second user message should have a breakpoint
        assert!(user_msg_2["content"].is_array());
        let content_array = user_msg_2["content"].as_array().unwrap();
        assert_eq!(
            content_array[0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );

        // Third user message should have a breakpoint
        assert!(user_msg_3["content"].is_array());
        let content_array = user_msg_3["content"].as_array().unwrap();
        assert_eq!(
            content_array[0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );
    }

    #[test]
    fn build_body_omits_breakpoints_when_zero_and_system_is_empty() {
        static TEST_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
            slug: "test",
            api_key_env: "TEST_KEY",
            base_url: "https://test.com",
            max_tokens_field: "max_tokens",
            include_stream_usage: false,
            provider_name: "Test",
            supports_prompt_cache_key: false,
            supports_prompt_cache_breakpoint: true,
            emit_reasoning_content: false,
            supports_parallel_tool_calls: false,
        };
        let provider =
            OpenAiCompatProvider::new(&TEST_CONFIG, crate::providers::Timeouts::default()).unwrap();
        let model = crate::model::Model::from_spec("openai/gpt-5.6-luna").unwrap();
        let messages = vec![Message::user("hello".to_string())];
        let tools = json!([]);

        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::default(),
            &tools,
            None,
            None,
            0,
            false,
        );

        assert!(body.get("prompt_cache_options").is_none());
    }

    #[test]
    fn build_body_with_session_no_breakpoints_for_unsupported_model() {
        static TEST_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
            slug: "test",
            api_key_env: "TEST_KEY",
            base_url: "https://test.com",
            max_tokens_field: "max_tokens",
            include_stream_usage: false,
            provider_name: "Test",
            supports_prompt_cache_key: false,
            supports_prompt_cache_breakpoint: true,
            emit_reasoning_content: false,
            supports_parallel_tool_calls: false,
        };
        let provider =
            OpenAiCompatProvider::new(&TEST_CONFIG, crate::providers::Timeouts::default()).unwrap();
        let model = crate::model::Model::from_spec("openai/gpt-4o").unwrap();
        let messages = vec![Message::user("hello".to_string())];
        let tools = json!([]);

        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::from("be helpful"),
            &tools,
            None,
            None,
            2,
            false,
        );

        // No prompt_cache_options for unsupported model
        assert!(body.get("prompt_cache_options").is_none());
    }

    #[test]
    fn build_body_with_session_adds_tool_result_breakpoint() {
        static TEST_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
            slug: "test",
            api_key_env: "TEST_KEY",
            base_url: "https://test.com",
            max_tokens_field: "max_tokens",
            include_stream_usage: false,
            provider_name: "Test",
            supports_prompt_cache_key: false,
            supports_prompt_cache_breakpoint: true,
            emit_reasoning_content: false,
            supports_parallel_tool_calls: false,
        };
        let provider =
            OpenAiCompatProvider::new(&TEST_CONFIG, crate::providers::Timeouts::default()).unwrap();
        let model = crate::model::Model::from_spec("openai/gpt-5.6-luna").unwrap();
        let messages = vec![
            Message::user("use a tool".to_string()),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_123".to_string(),
                    name: "test_tool".to_string(),
                    input: json!({"arg": "value"}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_123".to_string(),
                    content: "tool output".to_string(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];
        let tools = json!([{
            "name": "test_tool",
            "description": "A test tool",
            "input_schema": {"type": "object"}
        }]);

        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::from("be helpful"),
            &tools,
            None,
            None,
            1,
            false,
        );

        // Check that prompt_cache_options is set
        assert_eq!(body["prompt_cache_options"]["mode"], "explicit");

        // Find the tool result message and check it has a breakpoint
        let msgs = body["messages"].as_array().unwrap();
        let tool_msg = msgs.iter().find(|m| m["role"] == "tool").unwrap();
        assert!(tool_msg["content"].is_array());
        let content_array = tool_msg["content"].as_array().unwrap();
        assert_eq!(
            content_array[0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );
    }

    #[test]
    fn convert_messages_with_breakpoints_none() {
        let messages = vec![Message::user("hello".into())];
        let result = convert_messages_with_breakpoints(&messages, Some("system"), false, None);
        assert!(result[1]["content"].is_string());
    }

    #[test]
    fn convert_messages_with_breakpoints_some() {
        let messages = vec![
            Message::user("first".into()),
            Message::user("second".into()),
        ];
        let result = convert_messages_with_breakpoints(&messages, Some("system"), false, Some(1));
        // Last user message should have breakpoint
        assert!(result[2]["content"].is_array());
        let content = result[2]["content"].as_array().unwrap();
        assert_eq!(content[0]["prompt_cache_breakpoint"]["mode"], "explicit");
    }

    #[test]
    fn build_body_with_tools_adds_parallel_tool_calls_when_supported() {
        static TEST_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
            slug: "test",
            api_key_env: "TEST_KEY",
            base_url: "https://test.com",
            max_tokens_field: "max_tokens",
            include_stream_usage: false,
            provider_name: "Test",
            supports_prompt_cache_key: false,
            supports_prompt_cache_breakpoint: false,
            emit_reasoning_content: false,
            supports_parallel_tool_calls: true,
        };
        let provider =
            OpenAiCompatProvider::new(&TEST_CONFIG, crate::providers::Timeouts::default()).unwrap();
        let model = crate::model::Model::from_spec("openai/gpt-4o").unwrap();
        let messages = vec![Message::user("hello".to_string())];
        let tools = json!([{
            "name": "bash",
            "description": "run shell commands",
            "input_schema": {"type": "object"}
        }]);

        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::from("system"),
            &tools,
            None,
            None,
            0,
            false,
        );
        assert_eq!(body["parallel_tool_calls"], true);
    }

    #[test]
    fn build_body_with_tools_skips_parallel_tool_calls_when_unsupported() {
        static TEST_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
            slug: "test",
            api_key_env: "TEST_KEY",
            base_url: "https://test.com",
            max_tokens_field: "max_tokens",
            include_stream_usage: false,
            provider_name: "Test",
            supports_prompt_cache_key: false,
            supports_prompt_cache_breakpoint: false,
            emit_reasoning_content: false,
            supports_parallel_tool_calls: false,
        };
        let provider =
            OpenAiCompatProvider::new(&TEST_CONFIG, crate::providers::Timeouts::default()).unwrap();
        let model = crate::model::Model::from_spec("openai/gpt-4o").unwrap();
        let messages = vec![Message::user("hello".to_string())];
        let tools = json!([{
            "name": "bash",
            "description": "run shell commands",
            "input_schema": {"type": "object"}
        }]);

        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::from("system"),
            &tools,
            None,
            None,
            0,
            false,
        );
        assert!(body.get("parallel_tool_calls").is_none());
    }

    #[test_case(true, Some("fast") ; "fast")]
    #[test_case(false, None ; "standard")]
    fn build_body_service_tier(fast: bool, expected: Option<&str>) {
        static TEST_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
            slug: "test",
            api_key_env: "TEST_KEY",
            base_url: "https://test.com",
            max_tokens_field: "max_tokens",
            include_stream_usage: false,
            provider_name: "Test",
            supports_prompt_cache_key: false,
            supports_prompt_cache_breakpoint: false,
            emit_reasoning_content: false,
            supports_parallel_tool_calls: false,
        };
        let provider =
            OpenAiCompatProvider::new(&TEST_CONFIG, crate::providers::Timeouts::default()).unwrap();
        let mut model = crate::model::Model::from_spec("anthropic/claude-opus-4-8").unwrap();
        model.pricing.fast = Some(crate::model::FastPricing {
            input: 10.0,
            output: 60.0,
        });
        let messages = vec![Message::user("hello".to_string())];
        let body = provider.build_body_with_session(
            &model,
            &messages,
            &System::from("system"),
            &json!([]),
            None,
            None,
            0,
            fast,
        );
        assert_eq!(body.get("service_tier").and_then(Value::as_str), expected);
    }

    #[test_case("abc123", "auto" ; "image_detail_auto")]
    fn convert_messages_includes_image_detail(data: &str, expected_detail: &str) {
        use std::sync::Arc;

        use crate::types::{ImageMediaType, ImageSource};

        let source = ImageSource::new(ImageMediaType::Png, Arc::from(data));
        let msgs = vec![Message::user_with_images("describe".into(), vec![source])];
        let result = convert_messages(&msgs, Some("system"), false);
        let user = &result[1];
        let content = user["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "image_url");
        assert_eq!(content[0]["image_url"]["detail"], expected_detail);
    }
}
