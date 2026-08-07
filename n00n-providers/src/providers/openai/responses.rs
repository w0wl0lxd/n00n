use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Write;
use std::io::{Error as IoError, ErrorKind};
use std::time::{Duration, Instant};

use flume::Sender;
use futures_lite::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
use isahc::{HttpClient, Request};
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::providers::{DEFAULT_TOOL_DESCRIPTION_MAX_CHARS, ResolvedAuth, trim_tool_description};
use crate::types::{
    ReasoningContext, ReasoningMode, TOOL_RESULT_ERROR_PREFIX, ThinkingFieldConfig,
};
use crate::{
    AgentError, ContentBlock, Message, ProviderEvent, RequestDeliveryMetadata,
    RequestDeliveryPhase, RequestOptions, Role, StopReason, StreamResponse, System, TokenUsage,
    dialect,
};

const RESPONSES_PATH: &str = "/responses";
const RESPONSE_IN_FLIGHT_TIMEOUT_MULTIPLIER: u32 = 6;
const MAX_RESPONSE_IN_FLIGHT_TIMEOUT: Duration = Duration::from_mins(30);
const PROMPT_CACHE_TTL: &str = "30m";
const MAX_SAFETY_IDENTIFIER_CHARS: usize = 64;
pub(super) const MODERATION_MODEL: &str = "omni-moderation-latest";
const OPENAI_BUILTIN_ORIGIN: &str = "openai";
const INPUT_FIELD: &str = "input";
const REASONING_EFFORT_PATH: &str = "reasoning.effort";

pub(crate) fn response_in_flight_timeout(stream_timeout: Duration) -> Duration {
    stream_timeout
        .saturating_mul(RESPONSE_IN_FLIGHT_TIMEOUT_MULTIPLIER)
        .min(MAX_RESPONSE_IN_FLIGHT_TIMEOUT)
}

pub(crate) fn build_body(
    model: &crate::model::Model,
    messages: &[Message],
    system: &System,
    tools: &Value,
    previous_response_id: Option<&str>,
    prompt_cache_key: Option<&str>,
    store: bool,
    opts: &RequestOptions,
    parallel_tool_calls: bool,
) -> Value {
    let input = convert_input(messages, system, opts.message_cache_breakpoints, model);
    let has_prompt_cache_breakpoint = contains_prompt_cache_breakpoint(&input);
    let wire_tools = convert_tools(tools, model);

    let mut body = json!({
        "model": model.id,
        "input": input,
        "stream": true,
        "store": store,
        "include": ["reasoning.encrypted_content"],
        "reasoning": {"summary": "auto"},
    });

    // Add instructions as top-level field (not moved into input array)
    let instructions = system.to_string();
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }

    if let Some(previous_response_id) = previous_response_id {
        body["previous_response_id"] = json!(previous_response_id);
    }
    if let Some(prompt_cache_key) = prompt_cache_key {
        body["prompt_cache_key"] = json!(prompt_cache_key);
    }

    if has_prompt_cache_breakpoint {
        body["prompt_cache_options"] = json!({
            "mode": "explicit",
            "ttl": PROMPT_CACHE_TTL
        });
    }

    // Service tier for fast mode
    if opts.fast && model.supports_fast() {
        body["service_tier"] = json!("fast");
    }

    if let Some(ref safety_id) = opts.safety_identifier {
        let safety_id_chars = safety_id.chars().count();
        if safety_id_chars <= MAX_SAFETY_IDENTIFIER_CHARS {
            body["safety_identifier"] = json!(safety_id);
        } else {
            warn!(
                safety_id_chars,
                "safety_identifier exceeds 64 chars, omitting"
            );
        }
    }

    if opts.moderation {
        body["moderation"] = json!({"model": MODERATION_MODEL});
    }

    // Reasoning effort with extended dialect for xhigh/max
    let extras = opts.thinking.extras();
    let reasoning_fields = ThinkingFieldConfig {
        effort_path: Some(REASONING_EFFORT_PATH.into()),
        ..Default::default()
    };
    opts.thinking.apply_thinking(
        &mut body,
        model,
        &dialect::OPENAI_EXTENDED,
        &reasoning_fields,
    );

    // Reasoning mode and context from extras
    if let Some(mode) = extras.reasoning_mode {
        body["reasoning"]["mode"] = json!(match mode {
            ReasoningMode::Standard => "standard",
            ReasoningMode::Pro => "pro",
        });
    }
    if let Some(context) = extras.reasoning_context {
        body["reasoning"]["context"] = json!(match context {
            ReasoningContext::Auto => "auto",
            ReasoningContext::CurrentTurn => "current_turn",
            ReasoningContext::AllTurns => "all_turns",
        });
    }

    if wire_tools.as_array().is_some_and(|a| !a.is_empty()) {
        body["tools"] = wire_tools;
        if parallel_tool_calls {
            body["parallel_tool_calls"] = json!(true);
        }
    }

    super::super::apply_body_overrides(&mut body, model, &[INPUT_FIELD]);
    body
}

fn contains_prompt_cache_breakpoint(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_prompt_cache_breakpoint),
        Value::Object(map) => {
            map.contains_key("prompt_cache_breakpoint")
                || map.values().any(contains_prompt_cache_breakpoint)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

pub(crate) fn convert_input(
    messages: &[Message],
    _system: &System,
    message_cache_breakpoints: usize,
    model: &crate::model::Model,
) -> Value {
    let mut input = Vec::new();
    let supports_breakpoint = model.supports_prompt_cache_breakpoint();

    let breakpoint_indices: HashSet<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m.role, Role::User))
        .map(|(i, _)| i)
        .rev()
        .take(message_cache_breakpoints)
        .collect();

    for (msg_idx, msg) in messages.iter().enumerate() {
        match msg.role {
            Role::User => {
                let input_start = input.len();
                let mut content_blocks = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            content_blocks.push(json!({
                                "type": "input_text",
                                "text": text
                            }));
                        }
                        ContentBlock::Image { source } => {
                            content_blocks.push(source.to_input_image_payload());
                        }
                        ContentBlock::File { source } => {
                            let mut file_obj = json!({
                                "type": "input_file",
                            });
                            if let Some(ref file_id) = source.file_id {
                                file_obj["file_id"] = json!(file_id);
                            }
                            if let Some(ref file_url) = source.file_url {
                                file_obj["file_url"] = json!(file_url);
                            }
                            if let Some(ref file_data) = source.file_data {
                                file_obj["file_data"] = json!(file_data);
                            }
                            if let Some(ref filename) = source.filename {
                                file_obj["filename"] = json!(filename);
                            }
                            if let Some(detail) = source.detail {
                                file_obj["detail"] = json!(detail.to_string());
                            }
                            content_blocks.push(file_obj);
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
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": output,
                            }));
                        }
                        ContentBlock::ToolUse { .. }
                        | ContentBlock::Thinking { .. }
                        | ContentBlock::RedactedThinking { .. } => {}
                    }
                }

                let add_breakpoint = supports_breakpoint && breakpoint_indices.contains(&msg_idx);
                if add_breakpoint && let Some(last) = content_blocks.last_mut() {
                    last["prompt_cache_breakpoint"] = json!({"mode": "explicit"});
                }

                if !content_blocks.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": content_blocks
                    }));
                } else if add_breakpoint
                    && input.len() > input_start
                    && let Some(last) = input.last_mut()
                {
                    last["prompt_cache_breakpoint"] = json!({"mode": "explicit"});
                }
            }
            Role::Assistant => {
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => input.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}]
                        })),
                        ContentBlock::ToolUse {
                            id,
                            name,
                            input: arguments,
                        } => {
                            input.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": arguments.to_string(),
                            }));
                        }
                        ContentBlock::RedactedThinking { data } => {
                            if let Ok(item) = serde_json::from_str::<Value>(data)
                                && item["type"].as_str() == Some("reasoning")
                            {
                                input.push(item);
                            }
                        }
                        ContentBlock::ToolResult { .. }
                        | ContentBlock::Image { .. }
                        | ContentBlock::Thinking { .. }
                        | ContentBlock::File { .. } => {}
                    }
                }
            }
        }
    }

    Value::Array(input)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RequestDiagnostics {
    pub(crate) input_items: usize,
    pub(crate) request_bytes: usize,
    pub(crate) text_items: usize,
    pub(crate) text_bytes: usize,
    pub(crate) tool_items: usize,
    pub(crate) tool_bytes: usize,
    pub(crate) image_items: usize,
    pub(crate) image_bytes: usize,
    pub(crate) reasoning_items: usize,
    pub(crate) reasoning_bytes: usize,
}

impl RequestDiagnostics {
    fn add_text(&mut self, value: &Value) {
        self.text_items += 1;
        self.text_bytes += serialized_len(value);
    }

    fn add_tool(&mut self, value: &Value) {
        self.tool_items += 1;
        self.tool_bytes += serialized_len(value);
    }

    fn add_image(&mut self, value: &Value) {
        self.image_items += 1;
        self.image_bytes += serialized_len(value);
    }

    fn add_reasoning(&mut self, value: &Value) {
        self.reasoning_items += 1;
        self.reasoning_bytes += serialized_len(value);
    }
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}

pub(crate) fn request_diagnostics(body: &Value) -> RequestDiagnostics {
    let mut diagnostics = RequestDiagnostics {
        request_bytes: serialized_len(body),
        ..Default::default()
    };
    if let Some(instructions) = body.get("instructions") {
        diagnostics.add_text(instructions);
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            diagnostics.add_tool(tool);
        }
    }
    let Some(input) = body.get("input").and_then(Value::as_array) else {
        return diagnostics;
    };
    diagnostics.input_items = input.len();
    for item in input {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call" | "function_call_output") => diagnostics.add_tool(item),
            Some("reasoning") => diagnostics.add_reasoning(item),
            Some("message") => {
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for block in content {
                        if block.get("type").and_then(Value::as_str) == Some("input_image") {
                            diagnostics.add_image(block);
                        } else {
                            diagnostics.add_text(block);
                        }
                    }
                }
            }
            _ => diagnostics.add_text(item),
        }
    }
    diagnostics
}

const CLIENT_EXECUTED_BUILTIN_TOOLS: &[&str] = &["computer", "computer_use_preview"];
const BUILTIN_TOOLS: &[&str] = &[
    "web_search",
    "file_search",
    "computer",
    "computer_use_preview",
    "code_interpreter",
    "mcp",
    "shell",
    "local_shell",
    "apply_patch",
    "skills",
    "tool_search",
    "programmatic_tool_calling",
    "image_generation",
    "namespace",
    "function",
];
const BUILTIN_CONFIG_KEYS: &[(&[&str], &[&str])] = &[
    (
        &["file_search"],
        &[
            "vector_store_ids",
            "filters",
            "max_num_results",
            "ranking_options",
        ],
    ),
    (
        &["web_search"],
        &["filters", "search_context_size", "user_location"],
    ),
    (&["code_interpreter"], &["container", "allowed_callers"]),
    (
        &["shell", "local_shell"],
        &["environment", "allowed_callers"],
    ),
    (
        &["mcp"],
        &[
            "server_label",
            "server_url",
            "connector_id",
            "tunnel_id",
            "allowed_callers",
            "allowed_tools",
            "require_approval",
            "headers",
        ],
    ),
    (
        &["computer", "computer_use_preview"],
        &["environment", "display_width", "display_height"],
    ),
];
pub(crate) fn convert_tools(anthropic_tools: &Value, model: &crate::model::Model) -> Value {
    let Some(tools) = anthropic_tools.as_array() else {
        return json!([]);
    };

    Value::Array(
        tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?;
                let is_openai_builtin =
                    t.get("origin").and_then(Value::as_str) == Some(OPENAI_BUILTIN_ORIGIN);
                if is_openai_builtin && CLIENT_EXECUTED_BUILTIN_TOOLS.contains(&name) {
                    warn!(
                        tool = name,
                        "omitting unsupported client-executed OpenAI tool"
                    );
                    return None;
                }
                // Check if this is a built-in tool from OpenAI and the model supports it
                if is_openai_builtin
                    && BUILTIN_TOOLS.contains(&name)
                    && model.supports_responses_built_in_tools()
                {
                    // If the tool already has a "type" field matching a built-in name,
                    // prefer that and merge any other fields
                    if let Some(tool_type) = t.get("type").and_then(Value::as_str)
                        && BUILTIN_TOOLS.contains(&tool_type)
                    {
                        let mut built_in = json!({"type": tool_type});
                        copy_builtin_config(&mut built_in, t, tool_type);
                        return Some(built_in);
                    }
                    // Otherwise, construct from name
                    let mut built_in = json!({"type": name});
                    copy_builtin_config(&mut built_in, t, name);
                    return Some(built_in);
                }
                // Regular function tool
                let description = t.get("description").and_then(Value::as_str).map(|d| {
                    trim_tool_description(d, DEFAULT_TOOL_DESCRIPTION_MAX_CHARS).into_owned()
                })?;
                Some(json!({
                    "type": "function",
                    "name": name,
                    "description": description,
                    "parameters": t.get("input_schema")?,
                    "strict": t.get("strict").and_then(Value::as_bool) == Some(true),
                }))
            })
            .collect(),
    )
}

fn copy_builtin_config(built_in: &mut Value, source: &Value, tool_type: &str) {
    let Some((_, keys)) = BUILTIN_CONFIG_KEYS
        .iter()
        .find(|(tool_types, _)| tool_types.contains(&tool_type))
    else {
        return;
    };
    for key in *keys {
        if let Some(value) = source.get(*key) {
            built_in[*key] = value.clone();
        }
    }
}

pub(crate) fn base_url(auth: &ResolvedAuth) -> &str {
    match auth.base_url.as_deref() {
        Some(base_url) => base_url,
        None => super::OPENAI_API_BASE_URL,
    }
}

pub(crate) async fn do_stream(
    client: &HttpClient,
    model: &crate::model::Model,
    body: &Value,
    event_tx: &Sender<ProviderEvent>,
    auth: &ResolvedAuth,
    stream_timeout: Duration,
) -> Result<(Option<String>, StreamResponse), AgentError> {
    let base_url = base_url(auth);
    let json_body = serde_json::to_vec(body)?;

    let request = auth
        .configure_request(
            Request::builder()
                .method("POST")
                .uri(format!("{base_url}{RESPONSES_PATH}"))
                .header("content-type", "application/json")
                .header("user-agent", super::super::user_agent()),
        )
        .body(json_body)?;

    debug!(
        model = %model.id,
        provider = "OpenAI Coding Plan",
        "sending Responses API request"
    );

    let response = client.send_async(request).await?;
    let status = response.status().as_u16();

    if status == 200 {
        parse_sse(
            BufReader::new(response.into_body()),
            event_tx,
            stream_timeout,
        )
        .await
    } else {
        let retry_after = super::websocket::retry_after(
            response
                .headers()
                .get("retry-after")
                .map(isahc::http::HeaderValue::as_bytes),
        );
        let error = AgentError::from_response(response).await;
        if auth.base_url.as_deref() == Some(super::auth::CODING_PLAN_BASE_URL)
            && matches!(&error, AgentError::Api { status: 403, message } if message.trim().is_empty())
        {
            Err(AgentError::CodingPlanAdmission { retry_after })
        } else {
            Err(error)
        }
    }
}

struct ToolAccumulator {
    output_index: u64,
    call_id: String,
    name: String,
    arguments: String,
}

pub(crate) struct ResponseAccumulator {
    text: String,
    reasoning_summary_text: String,
    response_id: Option<String>,
    accepted: bool,
    reasoning_items: Vec<(u64, Value)>,
    tool_accumulators: Vec<ToolAccumulator>,
    usage: TokenUsage,
    stop_reason: Option<StopReason>,
    is_first_content: bool,
    emitted_event: bool,
}

pub(crate) fn is_semantic_progress_event(event_type: &str, data: &Value) -> bool {
    match event_type {
        "response.created" => data.get("response").is_some_and(Value::is_object),
        "response.in_progress" => {
            data.get("response").is_some_and(Value::is_object)
                || data.get("prompt_progress").is_some_and(Value::is_object)
        }
        "response.output_text.delta"
        | "response.reasoning_summary_text.delta"
        | "response.reasoning_text.delta" => data
            .get("delta")
            .and_then(Value::as_str)
            .is_some_and(|delta| !delta.is_empty()),
        "response.function_call_arguments.delta" => data.get("delta").is_some_and(|delta| {
            delta.as_str().is_some_and(|delta| !delta.is_empty())
                || delta.as_object().is_some_and(|delta| !delta.is_empty())
        }),
        "response.output_item.added" | "response.output_item.done" => {
            data.get("item").is_some_and(Value::is_object)
        }
        "response.web_search_call.completed"
        | "response.file_search_call.completed"
        | "response.computer_call.completed"
        | "response.code_interpreter_call.completed" => true,
        "response.content_part.added" => data
            .get("part")
            .is_some_and(|part| part["type"].as_str() == Some("output_text")),
        "response.reasoning_summary_part.added" => data.get("part").is_some_and(Value::is_object),
        _ => false,
    }
}

impl ResponseAccumulator {
    fn has_reasoning_item(&self, item: &Value) -> bool {
        let id = item["id"].as_str();
        self.reasoning_items.iter().any(|(_, stored)| {
            id.is_some() && stored["id"].as_str() == id || id.is_none() && stored == item
        })
    }

    pub fn new() -> Self {
        Self {
            text: String::new(),
            reasoning_summary_text: String::new(),
            response_id: None,
            accepted: false,
            reasoning_items: Vec::new(),
            tool_accumulators: Vec::new(),
            usage: TokenUsage::default(),
            stop_reason: None,
            is_first_content: true,
            emitted_event: false,
        }
    }

    pub fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    pub fn emitted_event(&self) -> bool {
        self.emitted_event
    }

    pub fn delivery_metadata(&self) -> RequestDeliveryMetadata {
        let phase = if self.accepted || self.response_id.is_some() {
            RequestDeliveryPhase::Accepted
        } else {
            RequestDeliveryPhase::SentAwaitingAcceptance
        };
        let mut metadata = RequestDeliveryMetadata::new(phase);
        metadata.response_id.clone_from(&self.response_id);
        metadata.emitted_event = self.emitted_event;
        metadata
    }

    #[allow(clippy::too_many_lines)]
    pub async fn handle_event(
        &mut self,
        event_type: &str,
        data: &Value,
        event_tx: &Sender<ProviderEvent>,
    ) -> Result<bool, AgentError> {
        if event_type == "response.created" {
            self.accepted = true;
        }
        if let Some(response_id) = data["response"]["id"].as_str() {
            self.response_id = Some(response_id.to_owned());
            self.accepted = true;
        }

        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = data["delta"].as_str()
                    && !delta.is_empty()
                {
                    let delta = if self.is_first_content {
                        self.is_first_content = false;
                        delta.trim_start().to_string()
                    } else {
                        delta.to_string()
                    };
                    if !delta.is_empty() {
                        self.text.push_str(&delta);
                        self.emitted_event = true;
                        event_tx
                            .send_async(ProviderEvent::TextDelta { text: delta })
                            .await?;
                    }
                }
            }

            "response.content_part.added" => {
                let part = &data["part"];
                if part["type"].as_str() == Some("output_text")
                    && let Some(text) = part["text"].as_str()
                    && !text.is_empty()
                {
                    let text = if self.is_first_content {
                        self.is_first_content = false;
                        text.trim_start().to_string()
                    } else {
                        text.to_string()
                    };
                    if !text.is_empty() {
                        self.text.push_str(&text);
                        self.emitted_event = true;
                        event_tx
                            .send_async(ProviderEvent::TextDelta { text })
                            .await?;
                    }
                }
            }

            "response.output_item.added" => {
                let item = &data["item"];
                let output_index = data["output_index"]
                    .as_u64()
                    .unwrap_or_else(|| self.tool_accumulators.len() as u64);
                match item["type"].as_str() {
                    Some("function_call") => {
                        let call_id = item["call_id"]
                            .as_str()
                            .map_or_else(String::new, ToString::to_string);
                        let name = item["name"]
                            .as_str()
                            .map_or_else(String::new, ToString::to_string);
                        if !name.is_empty() {
                            self.emitted_event = true;
                            event_tx
                                .send_async(ProviderEvent::ToolUseStart {
                                    id: call_id.clone(),
                                    name: name.clone(),
                                })
                                .await?;
                        }
                        self.tool_accumulators.push(ToolAccumulator {
                            output_index,
                            call_id,
                            name,
                            arguments: String::new(),
                        });
                    }
                    Some(
                        tool_type @ ("web_search_call"
                        | "file_search_call"
                        | "computer_call"
                        | "code_interpreter_call"),
                    ) => {
                        debug!(tool_type, "OpenAI built-in tool call started");
                    }
                    Some("program") => debug!("OpenAI program item"),
                    Some("program_output") => debug!("OpenAI program_output item"),
                    Some("reasoning" | "message") => {}
                    _ => {
                        let item_type = item["type"].as_str().unwrap_or_else(|| "unknown");
                        warn!(item_type, "Unknown OpenAI output item type");
                    }
                }
            }

            "response.function_call_arguments.delta" => {
                let delta: Cow<'_, str> = if let Some(s) = data["delta"].as_str() {
                    Cow::Borrowed(s)
                } else if let Some(obj) = data["delta"].as_object() {
                    match serde_json::to_string(obj) {
                        Ok(s) => Cow::Owned(s),
                        Err(e) => {
                            warn!(error = %e, "failed to serialize delta object, using empty string");
                            Cow::Borrowed("")
                        }
                    }
                } else {
                    Cow::Borrowed("")
                };
                if !delta.is_empty() {
                    let acc = if let Some(idx) = data["output_index"].as_u64() {
                        self.tool_accumulators
                            .iter_mut()
                            .find(|a| a.output_index == idx)
                    } else {
                        self.tool_accumulators.last_mut()
                    };
                    if let Some(acc) = acc {
                        acc.arguments.push_str(&delta);
                    }
                }
            }

            "response.created" => {
                if let Some(id) = data["response"]["id"].as_str() {
                    self.response_id = Some(id.to_string());
                }
            }

            "response.in_progress" => {
                if let Some(pp) = data.get("prompt_progress") {
                    #[allow(clippy::cast_possible_truncation)]
                    let processed = pp["processed"].as_u64().map_or(0, |v| v as u32);
                    #[allow(clippy::cast_possible_truncation)]
                    let total = pp["total"].as_u64().map_or(0, |v| v as u32);
                    #[allow(clippy::cast_possible_truncation)]
                    let cache = pp["cache"].as_u64().map_or(0, |v| v as u32);
                    self.emitted_event = true;
                    event_tx
                        .send_async(ProviderEvent::PromptProgress {
                            processed,
                            total,
                            cache,
                        })
                        .await?;
                }
            }

            "response.output_item.done" => {
                let item = &data["item"];
                let previous_text_len = self.text.len();
                let output_index = data["output_index"].as_u64().unwrap_or_else(|| {
                    (self.reasoning_items.len() + self.tool_accumulators.len()) as u64
                });
                match item["type"].as_str() {
                    Some("reasoning") => {
                        if !self.has_reasoning_item(item) {
                            self.reasoning_items.push((output_index, item.clone()));
                        }
                    }
                    Some("message") if self.text.is_empty() => {
                        if let Some(content) = item["content"].as_array() {
                            for part in content {
                                if part["type"].as_str() == Some("output_text")
                                    && let Some(snapshot) = part["text"].as_str()
                                {
                                    self.text.push_str(snapshot);
                                }
                            }
                        }
                    }
                    Some("function_call") => {
                        let call_id = item["call_id"]
                            .as_str()
                            .map_or_else(String::new, ToString::to_string);
                        let name = item["name"]
                            .as_str()
                            .map_or_else(String::new, ToString::to_string);
                        let arguments = if let Some(s) = item["arguments"].as_str() {
                            s.to_string()
                        } else if let Some(obj) = item["arguments"].as_object() {
                            match serde_json::to_string(obj) {
                                Ok(s) => s,
                                Err(e) => {
                                    warn!(error = %e, "failed to serialize arguments, using empty string");
                                    String::new()
                                }
                            }
                        } else {
                            String::new()
                        };
                        let idx = data["output_index"].as_u64();
                        let acc = if let Some(idx) = idx {
                            self.tool_accumulators
                                .iter_mut()
                                .find(|acc| acc.output_index == idx)
                        } else {
                            self.tool_accumulators.last_mut()
                        };
                        if let Some(acc) = acc {
                            let should_emit_start = acc.name.is_empty() && !name.is_empty();
                            if acc.call_id.is_empty() {
                                acc.call_id.clone_from(&call_id);
                            }
                            if acc.name.is_empty() {
                                acc.name.clone_from(&name);
                            }
                            if !arguments.is_empty() {
                                acc.arguments = arguments;
                            }
                            if should_emit_start {
                                self.emitted_event = true;
                                event_tx
                                    .send_async(ProviderEvent::ToolUseStart {
                                        id: acc.call_id.clone(),
                                        name: acc.name.clone(),
                                    })
                                    .await?;
                            }
                        } else {
                            if !name.is_empty() {
                                self.emitted_event = true;
                                event_tx
                                    .send_async(ProviderEvent::ToolUseStart {
                                        id: call_id.clone(),
                                        name: name.clone(),
                                    })
                                    .await?;
                            }
                            self.tool_accumulators.push(ToolAccumulator {
                                output_index: idx
                                    .unwrap_or_else(|| self.tool_accumulators.len() as u64),
                                call_id,
                                name,
                                arguments,
                            });
                        }
                    }
                    Some("web_search_call") => {
                        if let Some(action) = item.get("action") {
                            if let Some(queries) = action.get("queries").and_then(Value::as_array) {
                                for query in queries {
                                    let q = query
                                        .as_str()
                                        .or_else(|| query.get("text").and_then(Value::as_str));
                                    if let Some(q) = q {
                                        let _ = write!(self.text, "[search: {q}]");
                                    }
                                }
                            }
                            if let Some(results) = action.get("results").and_then(Value::as_array) {
                                for result in results {
                                    if let Some(title) = result.get("title").and_then(Value::as_str)
                                        && let Some(url) = result.get("url").and_then(Value::as_str)
                                    {
                                        let _ = write!(self.text, "[{title}]({url})");
                                    }
                                }
                            }
                        }
                    }
                    Some("file_search_call") => {
                        if let Some(results) = item.get("results").and_then(Value::as_array) {
                            for result in results {
                                if let Some(filename) =
                                    result.get("filename").and_then(Value::as_str)
                                    && let Some(file_id) =
                                        result.get("file_id").and_then(Value::as_str)
                                {
                                    let _ = write!(self.text, "[file:{filename} ({file_id})]");
                                }
                            }
                        }
                    }
                    Some("code_interpreter_call") => {
                        if let Some(outputs) = item.get("outputs").and_then(Value::as_array) {
                            for output in outputs {
                                if let Some(text) = output.get("text").and_then(Value::as_str) {
                                    self.text.push_str(text);
                                }
                                if let Some(image) = output.get("image")
                                    && let Some(file_id) =
                                        image.get("file_id").and_then(Value::as_str)
                                {
                                    let _ = write!(self.text, "[image:{file_id}]");
                                }
                            }
                        }
                    }
                    Some("computer_call") => {
                        warn!("received unsupported client-executed OpenAI computer call");
                    }
                    Some("computer_call_output") => {
                        if let Some(output) = item.get("output")
                            && let Some(screenshot) = output.get("screenshot")
                        {
                            if let Some(file_id) = screenshot.get("file_id").and_then(Value::as_str)
                            {
                                let _ = write!(self.text, "[screenshot:{file_id}]");
                            }
                            if let Some(image_url) =
                                screenshot.get("image_url").and_then(Value::as_str)
                            {
                                let _ = write!(self.text, "[screenshot:{image_url}]");
                            }
                        }
                    }
                    Some("program") => {
                        if let Some(code) = item.get("code").and_then(Value::as_str) {
                            self.reasoning_summary_text.push_str(code);
                        }
                    }
                    Some("program_output") => {
                        if let Some(output) = item.get("output").and_then(Value::as_str) {
                            self.text.push_str(output);
                        }
                    }
                    Some("tool_search_call") => {
                        debug!("OpenAI tool_search_call item");
                        self.text.push_str("[tool_search]");
                    }
                    Some("tool_search_output") => {
                        if let Some(tools) = item.get("tools").and_then(Value::as_array) {
                            let _ = write!(self.text, "[loaded {} tools]", tools.len());
                        }
                    }
                    _ => {}
                }
                if self.text.len() > previous_text_len {
                    let text = self.text[previous_text_len..].to_owned();
                    self.is_first_content = false;
                    self.emitted_event = true;
                    event_tx
                        .send_async(ProviderEvent::TextDelta { text })
                        .await?;
                }
            }

            "response.web_search_call.completed"
            | "response.file_search_call.completed"
            | "response.computer_call.completed"
            | "response.code_interpreter_call.completed" => {
                self.accepted = true;
                self.emitted_event = true;
                debug!(event_type, "OpenAI built-in tool call completed");
            }

            "response.reasoning_summary_text.delta" => {
                if let Some(delta) = data["delta"].as_str()
                    && !delta.is_empty()
                {
                    self.reasoning_summary_text.push_str(delta);
                    self.emitted_event = true;
                    event_tx
                        .send_async(ProviderEvent::ThinkingDelta {
                            text: delta.to_string(),
                        })
                        .await?;
                }
            }

            "response.reasoning_summary_part.added" if !self.reasoning_summary_text.is_empty() => {
                self.reasoning_summary_text.push_str("\n\n");
            }

            "response.completed" => {
                let resp = &data["response"];

                if let Some(output) = resp["output"].as_array() {
                    for (index, item) in output.iter().enumerate() {
                        if item["type"].as_str() == Some("reasoning")
                            && !self.has_reasoning_item(item)
                        {
                            self.reasoning_items.push((index as u64, item.clone()));
                        } else if item["type"].as_str() == Some("message")
                            && self.text.is_empty()
                            && let Some(content) = item["content"].as_array()
                        {
                            for part in content {
                                if part["type"].as_str() == Some("output_text")
                                    && let Some(snapshot) = part["text"].as_str()
                                {
                                    self.text.push_str(snapshot);
                                }
                            }
                        }
                    }
                }

                if let Some(u) = resp.get("usage") {
                    self.usage = parse_usage(u);
                }

                let status = resp["status"].as_str().unwrap_or_else(|| "completed");
                self.stop_reason = Some(match status {
                    "completed" => {
                        if self.tool_accumulators.is_empty() {
                            StopReason::EndTurn
                        } else {
                            StopReason::ToolUse
                        }
                    }
                    "incomplete" => StopReason::MaxTokens,
                    _ => StopReason::EndTurn,
                });
                return Ok(true);
            }

            "response.incomplete" => {
                let resp = &data["response"];
                if let Some(u) = resp.get("usage") {
                    self.usage = parse_usage(u);
                }
                self.stop_reason = Some(StopReason::MaxTokens);
                return Ok(true);
            }

            "response.failed" => {
                let resp = &data["response"];
                let error = &resp["error"];
                let message = error["message"].as_str().map_or_else(
                    || "response generation failed".to_string(),
                    ToString::to_string,
                );
                let code = error["code"].as_str().unwrap_or_else(|| "server_error");
                let status = match code {
                    "rate_limit_exceeded" => 429,
                    _ => 500,
                };
                return Err(AgentError::Api { status, message });
            }

            _ => {}
        }

        Ok(false)
    }

    pub fn into_stream_response(mut self) -> StreamResponse {
        let mut ordered_blocks =
            Vec::with_capacity(self.reasoning_items.len() + self.tool_accumulators.len());
        ordered_blocks.extend(self.reasoning_items.drain(..).map(|(index, item)| {
            (
                index,
                ContentBlock::RedactedThinking {
                    data: item.to_string(),
                },
            )
        }));

        for acc in self.tool_accumulators.drain(..) {
            let input: Value = match serde_json::from_str(&acc.arguments) {
                Ok(v) => {
                    debug!(
                        tool = %acc.name,
                        argument_bytes = acc.arguments.len(),
                        "parsed tool input JSON"
                    );
                    v
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        tool = %acc.name,
                        argument_bytes = acc.arguments.len(),
                        "malformed tool JSON, falling back to {{}}"
                    );
                    Value::Object(Default::default())
                }
            };
            ordered_blocks.push((
                acc.output_index,
                ContentBlock::ToolUse {
                    id: acc.call_id,
                    name: acc.name,
                    input,
                },
            ));
        }
        ordered_blocks.sort_by_key(|(index, _)| *index);
        let mut content_blocks: Vec<ContentBlock> =
            ordered_blocks.into_iter().map(|(_, block)| block).collect();

        if !self.reasoning_summary_text.is_empty() {
            content_blocks.push(ContentBlock::Thinking {
                thinking: self.reasoning_summary_text,
                signature: None,
            });
        }

        if !self.text.is_empty() {
            content_blocks.push(ContentBlock::Text { text: self.text });
        }

        StreamResponse {
            message: Message {
                role: Role::Assistant,
                content: content_blocks,
                ..Default::default()
            },
            usage: self.usage,
            stop_reason: self.stop_reason,
        }
    }
}

pub(crate) async fn parse_sse(
    reader: impl AsyncBufRead + Unpin,
    event_tx: &Sender<ProviderEvent>,
    stream_timeout: Duration,
) -> Result<(Option<String>, StreamResponse), AgentError> {
    let mut lines = reader.lines();

    let mut acc = ResponseAccumulator::new();
    let mut deadline = Instant::now() + stream_timeout;
    let response_deadline = Instant::now() + response_in_flight_timeout(stream_timeout);
    let mut current_event = String::new();

    loop {
        deadline = deadline.min(response_deadline);
        let line = match crate::providers::next_sse_line(&mut lines, &mut deadline, stream_timeout)
            .await
        {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                return Err(error.suppress_retry_after_send(Some(acc.delivery_metadata())));
            }
        };
        if line.is_empty() {
            current_event.clear();
            continue;
        }

        if let Some(event_type) = line.strip_prefix("event:") {
            current_event = event_type.trim().to_string();
            continue;
        }

        let data = match line.strip_prefix("data:") {
            Some(d) => d.trim(),
            None => continue,
        };

        if current_event == "error" {
            let error =
                if let Ok(ev) = serde_json::from_str::<crate::providers::SseErrorPayload>(data) {
                    warn!(error_type = %ev.error.r#type, "SSE error in stream");
                    ev.into_agent_error()
                } else {
                    let parsed: Value = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(_) => Value::Object(Default::default()),
                    };
                    let message = parsed["message"]
                        .as_str()
                        .map_or_else(|| "unknown error".to_string(), ToString::to_string);
                    AgentError::Api {
                        status: 500,
                        message,
                    }
                };
            return Err(error.suppress_retry_after_send(Some(acc.delivery_metadata())));
        }

        let parsed_event = if current_event.is_empty() {
            serde_json::from_str::<Value>(data)
                .ok()
                .and_then(|value| value["type"].as_str().map(ToOwned::to_owned))
                .unwrap_or_else(String::new)
        } else {
            current_event.clone()
        };

        let parsed: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if acc
            .handle_event(&parsed_event, &parsed, event_tx)
            .await
            .map_err(|e| e.suppress_retry_after_send(Some(acc.delivery_metadata())))?
        {
            break;
        }
    }

    if acc.stop_reason.is_none() {
        let error = IoError::new(
            ErrorKind::UnexpectedEof,
            "Responses API stream ended without a terminal event",
        );
        return Err(AgentError::RequestSent {
            message: error.to_string(),
            metadata: Some(acc.delivery_metadata()),
        });
    }

    let response_id = acc.response_id().map(ToOwned::to_owned);
    Ok((response_id, acc.into_stream_response()))
}

#[allow(clippy::manual_unwrap_or)]
fn parse_usage(u: &Value) -> TokenUsage {
    let input_tokens = u["input_tokens"].as_u64().map_or_else(
        || 0,
        |v| match u32::try_from(v) {
            Ok(v) => v,
            Err(_) => u32::MAX,
        },
    );
    let output_tokens = u["output_tokens"].as_u64().map_or_else(
        || 0,
        |v| match u32::try_from(v) {
            Ok(v) => v,
            Err(_) => u32::MAX,
        },
    );

    let cached = u["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .map_or_else(
            || 0,
            |v| match u32::try_from(v) {
                Ok(v) => v,
                Err(_) => u32::MAX,
            },
        );
    let cache_write = u["input_tokens_details"]["cache_write_tokens"]
        .as_u64()
        .map_or_else(
            || 0,
            |v| match u32::try_from(v) {
                Ok(v) => v,
                Err(_) => u32::MAX,
            },
        );

    // Extract reasoning_tokens if present
    let reasoning_tokens = u["output_tokens_details"]["reasoning_tokens"]
        .as_u64()
        .map_or_else(
            || 0,
            |v| match u32::try_from(v) {
                Ok(v) => v,
                Err(_) => u32::MAX,
            },
        );

    let fresh_input = input_tokens
        .saturating_sub(cached)
        .saturating_sub(cache_write);
    debug!(
        fresh_input_tokens = fresh_input,
        cache_read_tokens = cached,
        cache_write_tokens = cache_write,
        output_tokens,
        reasoning_tokens,
        "OpenAI Responses token usage"
    );
    TokenUsage {
        input: fresh_input,
        output: output_tokens,
        cache_read: cached,
        cache_creation: cache_write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Model;
    use futures_lite::io::{AsyncReadExt, AsyncWriteExt, Cursor};
    use serde_json::json;
    use test_case::test_case;

    const TEST_STREAM_TIMEOUT: Duration = Duration::from_mins(5);

    async fn run_sse(
        sse: &str,
    ) -> (
        Result<(Option<String>, StreamResponse), AgentError>,
        Vec<ProviderEvent>,
    ) {
        let (tx, rx) = flume::unbounded();
        let result = parse_sse(Cursor::new(sse.as_bytes()), &tx, TEST_STREAM_TIMEOUT).await;
        (result, rx.drain().collect())
    }

    #[test]
    fn opaque_reasoning_delta_counts_as_semantic_progress() {
        assert!(is_semantic_progress_event(
            "response.reasoning_text.delta",
            &json!({"delta": "active reasoning"}),
        ));
        assert!(!is_semantic_progress_event(
            "response.reasoning_text.delta",
            &json!({"delta": ""}),
        ));
    }

    #[test]
    fn parse_sse_text_and_usage() {
        smol::block_on(async {
            let sse = "\
event: response.output_text.delta\n\
data: {\"delta\":\"Hello\"}\n\
\n\
event: response.output_text.delta\n\
data: {\"delta\":\" world\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":40}}}}\n\
\n";

            let (resp, events) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            assert_eq!(resp.usage.input, 60);
            assert_eq!(resp.usage.output, 10);
            assert_eq!(resp.usage.cache_read, 40);
            assert_eq!(resp.stop_reason, Some(StopReason::EndTurn));
            assert!(
                matches!(&resp.message.content[0], ContentBlock::Text { text } if text == "Hello world")
            );

            let deltas: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    ProviderEvent::TextDelta { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(deltas, vec!["Hello", " world"]);
        });
    }

    #[test]
    fn parse_sse_tool_calls() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.added\n\
data: {\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\"}}\n\
\n\
event: response.output_item.added\n\
data: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"c2\",\"name\":\"read\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"output_index\":0,\"delta\":\"{\\\"command\\\": \\\"ls\\\"}\"}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"output_index\":1,\"delta\":\"{\\\"path\\\": \\\"/tmp\\\"}\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\
\n";

            let (resp, events) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 2);
            assert_eq!((tools[0].0, tools[0].1), ("c1", "bash"));
            assert_eq!(tools[0].2["command"], "ls");
            assert_eq!((tools[1].0, tools[1].1), ("c2", "read"));
            assert_eq!(tools[1].2["path"], "/tmp");
            assert_eq!(resp.stop_reason, Some(StopReason::ToolUse));

            let starts: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    ProviderEvent::ToolUseStart { id, name } => Some((id.as_str(), name.as_str())),
                    _ => None,
                })
                .collect();
            assert_eq!(starts, vec![("c1", "bash"), ("c2", "read")]);
        });
    }

    #[test]
    fn parse_sse_builtin_lifecycle_uses_assistant_text_only() {
        smol::block_on(async {
            let sse = "event: response.output_item.added\ndata: {\"output_index\":0,\"item\":{\"type\":\"web_search_call\",\"content\":[{\"output_text\":\"dead snapshot\"}]}}\n\nevent: response.web_search_call.completed\ndata: {\"output_index\":0,\"item_id\":\"ws_1\"}\n\nevent: response.content_part.added\ndata: {\"part\":{\"type\":\"output_text\",\"text\":\"verified answer\"}}\n\nevent: response.completed\ndata: {\"response\":{\"status\":\"completed\"}}\n\n";
            let (response, events) = run_sse(sse).await;
            let (_, response) = response.unwrap();
            assert!(
                matches!(&response.message.content[0], ContentBlock::Text { text } if text == "verified answer")
            );
            let deltas: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    ProviderEvent::TextDelta { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(deltas, vec!["verified answer"]);
        });
    }

    #[test_case("response.web_search_call.completed" ; "web_search")]
    #[test_case("response.file_search_call.completed" ; "file_search")]
    #[test_case("response.computer_call.completed" ; "computer")]
    #[test_case("response.code_interpreter_call.completed" ; "code_interpreter")]
    fn builtin_completed_event_marks_output_emitted(event_type: &str) {
        smol::block_on(async {
            let (tx, _rx) = flume::unbounded();
            let mut accumulator = ResponseAccumulator::new();
            accumulator
                .handle_event(event_type, &json!({}), &tx)
                .await
                .unwrap();
            assert!(accumulator.emitted_event());
        });
    }

    #[test]
    fn synthesized_builtin_output_emits_text_deltas() {
        smol::block_on(async {
            let cases = [
                (
                    json!({"type":"web_search_call","action":{"queries":["rust async"]}}),
                    "[search: rust async]",
                ),
                (
                    json!({"type":"file_search_call","results":[{"filename":"doc.txt","file_id":"file_123"}]}),
                    "[file:doc.txt (file_123)]",
                ),
                (
                    json!({"type":"code_interpreter_call","outputs":[{"text":"Output: 42"}]}),
                    "Output: 42",
                ),
                (json!({"type":"program_output","output":"done"}), "done"),
                (
                    json!({"type":"tool_search_output","tools":[{}, {}]}),
                    "[loaded 2 tools]",
                ),
            ];

            for (item, expected) in cases {
                let (tx, rx) = flume::unbounded();
                let mut accumulator = ResponseAccumulator::new();
                accumulator
                    .handle_event(
                        "response.output_item.done",
                        &json!({"output_index":0,"item":item}),
                        &tx,
                    )
                    .await
                    .unwrap();
                let deltas: Vec<_> = rx
                    .drain()
                    .filter_map(|event| match event {
                        ProviderEvent::TextDelta { text } => Some(text),
                        _ => None,
                    })
                    .collect();

                assert_eq!(deltas, vec![expected]);
            }
        });
    }

    #[test]
    fn parse_sse_error_event() {
        smol::block_on(async {
            let sse = "\
event: error\n\
data: {\"error\":{\"message\":\"Server overloaded\",\"type\":\"overloaded_error\"}}\n\
\n";

            let (err, _) = run_sse(sse).await;
            match err.unwrap_err() {
                AgentError::Api { status, message } => {
                    assert_eq!(status, 529);
                    assert_eq!(message, "Server overloaded");
                }
                other => panic!("expected Api error, got: {other:?}"),
            }
        });
    }

    #[test]
    fn parse_sse_error_after_acceptance_is_not_retryable() {
        smol::block_on(async {
            let sse = "event: response.created\ndata: {\"response\":{\"id\":\"resp_1\"}}\n\nevent: error\ndata: {\"error\":{\"message\":\"Server overloaded\",\"type\":\"overloaded_error\"}}\n\n";

            let (err, _) = run_sse(sse).await;
            let err = err.unwrap_err();

            assert!(matches!(err, AgentError::RequestSent { .. }));
            assert!(!err.is_retryable());
        });
    }

    #[test]
    fn parse_sse_response_failed() {
        smol::block_on(async {
            let sse = "\
event: response.failed\n\
data: {\"response\":{\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit hit\"}}}\n\
\n";

            let (err, _) = run_sse(sse).await;
            match err.unwrap_err() {
                AgentError::Api { status, message } => {
                    assert_eq!(status, 429);
                    assert_eq!(message, "Rate limit hit");
                }
                other => panic!("expected Api error, got: {other:?}"),
            }
        });
    }

    #[test]
    fn parse_sse_text_error_after_partial_output_is_not_retryable() {
        smol::block_on(async {
            let sse = "\
event: response.output_text.delta\ndata: {\"delta\":\"partial\"}\n\n\
event: error\ndata: {\"error\":{\"message\":\"Server overloaded\",\"type\":\"overloaded_error\"}}\n\n";

            let (err, _) = run_sse(sse).await;
            match err.unwrap_err() {
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
event: response.output_item.added\ndata: {\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\"}}\n\n\
event: error\ndata: {\"error\":{\"message\":\"Server overloaded\",\"type\":\"overloaded_error\"}}\n\n";

            let (err, _) = run_sse(sse).await;
            match err.unwrap_err() {
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
    fn parse_sse_incomplete_response() {
        smol::block_on(async {
            let sse = "\
event: response.output_text.delta\n\
data: {\"delta\":\"partial\"}\n\
\n\
event: response.incomplete\n\
data: {\"response\":{\"status\":\"incomplete\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
\n";

            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();
            assert_eq!(resp.stop_reason, Some(StopReason::MaxTokens));
            assert!(
                matches!(&resp.message.content[0], ContentBlock::Text { text } if text == "partial")
            );
        });
    }

    #[test]
    fn convert_input_structure() {
        let model = Model::from_spec("openai/gpt-4.1").unwrap();
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

        let input = convert_input(&messages, &System::default(), 0, &model);
        let items = input.as_array().unwrap();

        assert_eq!(items[0]["type"], "message");
        assert_eq!(items[0]["role"], "user");
        assert_eq!(items[0]["content"][0]["type"], "input_text");
        assert_eq!(items[0]["content"][0]["text"], "hello");

        assert_eq!(items[1]["type"], "message");
        assert_eq!(items[1]["role"], "assistant");
        assert_eq!(items[1]["content"][0]["type"], "output_text");
        assert_eq!(items[1]["content"][0]["text"], "thinking...");

        assert_eq!(items[2]["type"], "function_call");
        assert_eq!(items[2]["call_id"], "tc_1");
        assert_eq!(items[2]["name"], "bash");

        assert_eq!(items[3]["type"], "function_call_output");
        assert_eq!(items[3]["call_id"], "tc_1");
        assert_eq!(items[3]["output"], "file.txt");
    }

    #[test]
    fn convert_input_prefixes_error_tool_result() {
        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tc_1".into(),
                content: "sub-agent error: API 500".into(),
                is_error: true,
            }],
            ..Default::default()
        }];
        let input = convert_input(
            &messages,
            &System::default(),
            0,
            &Model::from_spec("openai/gpt-4.1").unwrap(),
        );
        let output = input[0]["output"].as_str().unwrap();
        assert!(output.starts_with(TOOL_RESULT_ERROR_PREFIX));
        assert!(output.contains("sub-agent error: API 500"));
    }

    #[test]
    fn parse_sse_opaque_reasoning_text_delta_is_not_displayed() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.added\n\
data: {\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[],\"content\":[],\"encrypted_content\":\"\",\"status\":\"in_progress\"}}\n\
\n\
event: response.reasoning_text.delta\n\
data: {\"delta\":\"opaque reasoning\"}\n\
\n\
event: response.output_item.done\n\
data: {\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[],\"content\":[],\"encrypted_content\":\"encrypted\",\"status\":\"completed\"}}\n\
\n\
event: response.output_item.added\n\
data: {\"output_index\":1,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"status\":\"in_progress\",\"content\":[],\"role\":\"assistant\"}}\n\
\n\
event: response.output_text.delta\n\
data: {\"delta\":\"Hello world\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"input_tokens_details\":{\"cached_tokens\":10},\"output_tokens_details\":{\"reasoning_tokens\":5}}}}\n\
\n";

            let (resp, events) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            assert_eq!(resp.usage.input, 90);
            assert_eq!(resp.usage.output, 20);
            assert_eq!(resp.usage.cache_read, 10);

            assert_eq!(resp.message.content.len(), 2);
            assert!(
                matches!(&resp.message.content[0], ContentBlock::RedactedThinking { data } if data.contains("encrypted"))
            );
            assert!(
                matches!(&resp.message.content[1], ContentBlock::Text { text } if text == "Hello world")
            );

            let thinking_deltas: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    ProviderEvent::ThinkingDelta { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert!(thinking_deltas.is_empty());

            let text_deltas: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    ProviderEvent::TextDelta { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(text_deltas, vec!["Hello world"]);
        });
    }

    #[test]
    fn parse_sse_reasoning_summary_text_delta() {
        smol::block_on(async {
            let sse = "\
event: response.reasoning_summary_text.delta\n\
data: {\"delta\":\"Summary part\"}\n\
\n\
event: response.output_text.delta\n\
data: {\"delta\":\"Answer\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
\n";

            let (resp, events) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            assert!(
                matches!(&resp.message.content[0], ContentBlock::Thinking { thinking, .. } if thinking == "Summary part")
            );

            let thinking_deltas: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    ProviderEvent::ThinkingDelta { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(thinking_deltas, vec!["Summary part"]);
        });
    }

    #[test]
    fn parse_sse_opaque_reasoning_only_is_not_persisted_as_thinking() {
        smol::block_on(async {
            let sse = "\
event: response.reasoning_text.delta\n\
data: {\"delta\":\"Thinking only\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"output_tokens_details\":{\"reasoning_tokens\":5}}}}\n\
\n";

            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            assert!(resp.message.content.is_empty());
            assert_eq!(resp.usage.output, 5);
        });
    }

    #[test]
    fn parse_sse_malformed_tool_json_yields_empty_object() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.added\n\
data: {\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":\"{broken\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\
\n";

            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();
            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].1, "bash");
            assert_eq!(*tools[0].2, Value::Object(Default::default()));
        });
    }

    // llama.cpp's /v1/responses endpoint omits output_index in SSE events
    // (see https://github.com/ggml-org/llama.cpp/issues/20607)

    #[test]
    fn parse_sse_tool_call_without_output_index() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":\"{\\\"command\\\": \\\"ls\\\"}\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\
\n";

            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].0, "c1");
            assert_eq!(tools[0].1, "bash");
            assert_eq!(tools[0].2["command"], "ls");
            assert_eq!(resp.stop_reason, Some(StopReason::ToolUse));
        });
    }

    #[test]
    fn parse_sse_sequential_tool_calls_without_output_index() {
        smol::block_on(async {
            // Simulates llama.cpp streaming two sequential tool calls without output_index
            let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":\"{\\\"command\\\": \\\"ls\\\"}\"}\n\
\n\
event: response.output_item.done\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\",\"arguments\":\"{\\\"command\\\": \\\"ls\\\"}\"}}\n\
\n\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c2\",\"name\":\"read\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":\"{\\\"path\\\": \\\"/tmp\\\"}\"}\n\
\n\
event: response.output_item.done\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c2\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\": \\\"/tmp\\\"}\"}}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\
\n";

            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 2);
            assert_eq!((tools[0].0, tools[0].1), ("c1", "bash"));
            assert_eq!(tools[0].2["command"], "ls");
            assert_eq!((tools[1].0, tools[1].1), ("c2", "read"));
            assert_eq!(tools[1].2["path"], "/tmp");
        });
    }

    #[test]
    fn parse_sse_tool_done_without_output_index_updates_last_acc() {
        smol::block_on(async {
            // done event without output_index should update the last accumulator
            let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"glob\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":\"{\\\"pattern\\\": \\\"*.rs\\\"}\"}\n\
\n\
event: response.output_item.done\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"glob\",\"arguments\":\"{\\\"pattern\\\": \\\"*.rs\\\", \\\"path\\\": \\\"src\\\"}\"}}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\
\n";

            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].1, "glob");
            assert_eq!(tools[0].2["pattern"], "*.rs");
            assert_eq!(tools[0].2["path"], "src");
        });
    }

    #[test]
    fn parse_sse_prompt_progress_events() {
        smol::block_on(async {
            let sse = "\
event: response.in_progress\n\
data: {\"prompt_progress\":{\"processed\":100,\"total\":1000,\"cache\":50}}\n\
\n\
event: response.in_progress\n\
data: {\"prompt_progress\":{\"processed\":500,\"total\":1000,\"cache\":50}}\n\
\n\
event: response.output_text.delta\n\
data: {\"delta\":\"Hello\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n\
\n";

            let (_resp, events) = run_sse(sse).await;

            let progress: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    ProviderEvent::PromptProgress {
                        processed,
                        total,
                        cache,
                    } => Some((*processed, *total, *cache)),
                    _ => None,
                })
                .collect();
            assert_eq!(progress, vec![(100, 1000, 50), (500, 1000, 50)]);
        });
    }

    #[test]
    fn parse_sse_done_arguments_as_json_object() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"read\"}}\n\
\n\
event: response.output_item.done\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"read\",\"arguments\":{\"path\":\"/tmp/file.txt\"}}}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}
\
\n";

            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].1, "read");
            assert_eq!(tools[0].2["path"], "/tmp/file.txt");
        });
    }

    #[test]
    fn parse_sse_reasoning_summary_part_added() {
        smol::block_on(async {
            let sse = "\
event: response.reasoning_summary_part.added\n\
data: {\"id\":\"sp_1\"}\n\
\n\
event: response.reasoning_summary_text.delta\n\
data: {\"delta\":\"First part\"}\n\
\n\
event: response.reasoning_summary_part.added\n\
data: {\"id\":\"sp_2\"}\n\
\n\
event: response.reasoning_summary_text.delta\n\
data: {\"delta\":\"Second part\"}\n\
\n\
event: response.output_text.delta\n\
data: {\"delta\":\"Answer\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
\n";

            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            assert!(
                matches!(&resp.message.content[0], ContentBlock::Thinking { thinking, .. } if thinking == "First part\n\nSecond part")
            );
        });
    }

    #[test]
    fn parse_sse_delta_arguments_as_json_object() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"grep\"}}\n\
\n\
event: response.function_call_arguments.delta\n\
data: {\"delta\":{\"pattern\":\"TODO\",\"path\":\"src\"}}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}
\
\n";

            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].1, "grep");
            assert_eq!(tools[0].2["pattern"], "TODO");
            assert_eq!(tools[0].2["path"], "src");
        });
    }

    #[test]
    fn parse_sse_done_object_args_overrides_empty_delta() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.added\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"edit\"}}\n\
\n\
event: response.output_item.done\n\
data: {\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"edit\",\"arguments\":{\"path\":\"foo.rs\",\"old_string\":\"a\",\"new_string\":\"b\"}}}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}
\
\n";

            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].1, "edit");
            assert_eq!(tools[0].2["path"], "foo.rs");
            assert_eq!(tools[0].2["old_string"], "a");
            assert_eq!(tools[0].2["new_string"], "b");
        });
    }

    #[test]
    fn build_body_includes_continuity_and_cache_keys() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions::default();
        let body = build_body(
            &model,
            &[],
            &System::from("system"),
            &json!([]),
            Some("resp_1"),
            Some("session_1"),
            true,
            &opts,
            true,
        );
        assert_eq!(body["previous_response_id"], "resp_1");
        assert_eq!(body["prompt_cache_key"], "session_1");
        assert_eq!(body["store"], true);
        assert_eq!(body["reasoning"], json!({"summary":"auto"}));
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }

    #[test_case(0, false ; "no_emitted_breakpoint")]
    #[test_case(1, true ; "emitted_breakpoint")]
    fn build_body_prompt_cache_options_require_emitted_breakpoint(
        message_cache_breakpoints: usize,
        expected: bool,
    ) {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions {
            message_cache_breakpoints,
            ..Default::default()
        };
        let body = build_body(
            &model,
            &[Message::user("cache me".into())],
            &System::default(),
            &json!([]),
            None,
            None,
            false,
            &opts,
            true,
        );
        assert_eq!(body.get("prompt_cache_options").is_some(), expected);
    }

    #[test_case(64, true ; "unicode_boundary")]
    #[test_case(65, false ; "unicode_over_limit")]
    fn build_body_safety_identifier_counts_characters(chars: usize, expected: bool) {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions {
            safety_identifier: Some("界".repeat(chars)),
            ..Default::default()
        };
        let body = build_body(
            &model,
            &[],
            &System::default(),
            &json!([]),
            None,
            None,
            false,
            &opts,
            true,
        );
        assert_eq!(body.get("safety_identifier").is_some(), expected);
    }

    #[test_case(true, Some(MODERATION_MODEL) ; "enabled")]
    #[test_case(false, None ; "disabled")]
    fn build_body_moderation_uses_model_shape(enabled: bool, expected: Option<&str>) {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions {
            moderation: enabled,
            ..Default::default()
        };
        let body = build_body(
            &model,
            &[],
            &System::default(),
            &json!([]),
            None,
            None,
            false,
            &opts,
            true,
        );
        assert_eq!(
            body.get("moderation")
                .and_then(|moderation| moderation.get("model"))
                .and_then(Value::as_str),
            expected
        );
    }

    #[test]
    fn build_body_thinking_off() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions {
            thinking: crate::types::ThinkingConfig::Off,
            ..Default::default()
        };
        let body = build_body(
            &model,
            &[],
            &System::from("system"),
            &json!([]),
            None,
            None,
            false,
            &opts,
            true,
        );
        assert_eq!(body["reasoning"], json!({"summary":"auto"}));
    }

    #[test]
    fn build_body_thinking_adaptive() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions {
            thinking: crate::types::ThinkingConfig::Adaptive,
            ..Default::default()
        };
        let body = build_body(
            &model,
            &[],
            &System::from("system"),
            &json!([]),
            None,
            None,
            false,
            &opts,
            true,
        );
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["reasoning"]["effort"], "medium");
    }

    #[test]
    fn build_body_thinking_effort_high() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions {
            thinking: crate::types::ThinkingConfig::Effort(crate::Effort::High),
            ..Default::default()
        };
        let body = build_body(
            &model,
            &[],
            &System::from("system"),
            &json!([]),
            None,
            None,
            false,
            &opts,
            true,
        );
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn build_body_thinking_budget() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions {
            thinking: crate::types::ThinkingConfig::Budget(1024),
            ..Default::default()
        };
        let body = build_body(
            &model,
            &[],
            &System::from("system"),
            &json!([]),
            None,
            None,
            false,
            &opts,
            true,
        );
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["reasoning"]["effort"], "minimal");
    }

    #[test]
    fn convert_input_preserves_response_item_order() {
        let reasoning_one =
            json!({"id":"rs_1","type":"reasoning","encrypted_content":"one","summary":[]});
        let reasoning_two =
            json!({"id":"rs_2","type":"reasoning","encrypted_content":"two","summary":[]});
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::RedactedThinking {
                    data: reasoning_one.to_string(),
                },
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "read".into(),
                    input: json!({"path":"one"}),
                },
                ContentBlock::RedactedThinking {
                    data: reasoning_two.to_string(),
                },
                ContentBlock::ToolUse {
                    id: "c2".into(),
                    name: "read".into(),
                    input: json!({"path":"two"}),
                },
            ],
            ..Default::default()
        }];
        let input = convert_input(
            &messages,
            &System::default(),
            0,
            &Model::from_spec("openai/gpt-4.1").unwrap(),
        );
        assert_eq!(input[0], reasoning_one);
        assert_eq!(input[1]["call_id"], "c1");
        assert_eq!(input[2], reasoning_two);
        assert_eq!(input[3]["call_id"], "c2");
    }

    #[test_case("tool_search" ; "registry_tool_name_collision")]
    #[test_case("load_namespace" ; "namespace_tool")]
    fn convert_tools_keeps_named_local_tools_as_functions(name: &str) {
        let tools = json!([{
            "name": name,
            "description": "local tool",
            "input_schema": {"type": "object"}
        }]);
        let converted = convert_tools(&tools, &Model::from_spec("openai/gpt-5.6").unwrap());
        assert_eq!(converted[0]["type"], "function");
        assert_eq!(converted[0]["name"], name);
    }

    #[test]
    fn convert_tools_keeps_custom_as_function() {
        let tools = json!([{
            "origin": OPENAI_BUILTIN_ORIGIN,
            "name": "custom",
            "description": "custom grammar tool",
            "input_schema": {"type": "object"}
        }]);
        let converted = convert_tools(&tools, &Model::from_spec("openai/gpt-5.6").unwrap());
        assert_eq!(converted[0]["type"], "function");
        assert_eq!(converted[0]["name"], "custom");
    }

    #[test_case("file_search", &["vector_store_ids", "filters", "max_num_results", "ranking_options"] ; "file_search")]
    #[test_case("web_search", &["filters", "search_context_size", "user_location"] ; "web_search")]
    #[test_case("code_interpreter", &["container", "allowed_callers"] ; "code_interpreter")]
    #[test_case("shell", &["environment", "allowed_callers"] ; "shell")]
    #[test_case("local_shell", &["environment", "allowed_callers"] ; "local_shell_alias")]
    #[test_case("mcp", &["server_label", "server_url", "connector_id", "tunnel_id", "allowed_callers", "allowed_tools", "require_approval", "headers"] ; "mcp")]
    #[test_case("computer", &["environment", "display_width", "display_height"] ; "computer")]
    #[test_case("computer_use_preview", &["environment", "display_width", "display_height"] ; "computer_alias")]
    fn copy_builtin_config_uses_expected_keys(tool_type: &str, expected_keys: &[&str]) {
        let source = json!({
            "vector_store_ids": [],
            "filters": {},
            "max_num_results": 1,
            "ranking_options": {},
            "search_context_size": "low",
            "user_location": {},
            "container": "auto",
            "allowed_callers": [],
            "environment": "linux",
            "server_label": "server",
            "server_url": "server-url",
            "connector_id": "connector",
            "tunnel_id": "tunnel",
            "allowed_tools": [],
            "require_approval": "never",
            "headers": {},
            "display_width": 1,
            "display_height": 1,
        });
        let mut built_in = json!({"type": tool_type});
        copy_builtin_config(&mut built_in, &source, tool_type);
        let object = built_in.as_object().unwrap();
        assert_eq!(object.len(), expected_keys.len() + 1);
        assert!(expected_keys.iter().all(|key| object.contains_key(*key)));
    }

    #[test_case("web_search" ; "explicit_openai_builtin")]
    fn convert_tools_requires_openai_origin_for_builtins(name: &str) {
        let tools = json!([{
            "origin": OPENAI_BUILTIN_ORIGIN,
            "name": name,
            "description": "provider built-in",
            "input_schema": {"type": "object"}
        }]);
        let converted = convert_tools(&tools, &Model::from_spec("openai/gpt-5.6").unwrap());
        assert_eq!(converted, json!([{"type": name}]));
    }

    #[test_case("computer" ; "computer")]
    #[test_case("computer_use_preview" ; "computer_preview")]
    fn convert_tools_omits_unsupported_client_executed_builtins(name: &str) {
        let tools = json!([{
            "origin": OPENAI_BUILTIN_ORIGIN,
            "name": name,
            "description": "client-executed provider tool",
            "input_schema": {"type": "object"}
        }]);
        let converted = convert_tools(&tools, &Model::from_spec("openai/gpt-5.6").unwrap());
        assert_eq!(converted, json!([]));
    }

    #[test]
    fn convert_input_keeps_tool_result_before_following_text() {
        let messages = vec![Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "result".into(),
                    is_error: false,
                },
                ContentBlock::Text {
                    text: "continue".into(),
                },
            ],
            ..Default::default()
        }];
        let input = convert_input(
            &messages,
            &System::default(),
            1,
            &Model::from_spec("openai/gpt-5.6").unwrap(),
        );
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["content"][0]["text"], "continue");
        assert_eq!(
            input[1]["content"][0]["prompt_cache_breakpoint"],
            json!({"mode": "explicit"})
        );
    }

    #[test]
    fn convert_input_marks_tool_only_user_message_breakpoint() {
        let messages = vec![Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "first".into(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_2".into(),
                    content: "last".into(),
                    is_error: false,
                },
            ],
            ..Default::default()
        }];
        let input = convert_input(
            &messages,
            &System::default(),
            1,
            &Model::from_spec("openai/gpt-5.6").unwrap(),
        );
        assert_eq!(input.as_array().map(Vec::len), Some(2));
        assert!(input[0].get("prompt_cache_breakpoint").is_none());
        assert_eq!(
            input[1]["prompt_cache_breakpoint"],
            json!({"mode": "explicit"})
        );
    }

    #[test]
    fn parse_sse_preserves_reasoning_and_tool_order() {
        smol::block_on(async {
            let sse = "event: response.output_item.done\ndata: {\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"one\",\"summary\":[]}}\n\nevent: response.output_item.done\ndata: {\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"one\\\"}\"}}\n\nevent: response.output_item.done\ndata: {\"output_index\":2,\"item\":{\"id\":\"rs_2\",\"type\":\"reasoning\",\"encrypted_content\":\"two\",\"summary\":[]}}\n\nevent: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n";
            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();
            assert!(
                matches!(&resp.message.content[0], ContentBlock::RedactedThinking { data } if data.contains("one"))
            );
            assert!(
                matches!(&resp.message.content[1], ContentBlock::ToolUse { id, .. } if id == "c1")
            );
            assert!(
                matches!(&resp.message.content[2], ContentBlock::RedactedThinking { data } if data.contains("two"))
            );
        });
    }

    #[test]
    fn parse_sse_preserves_out_of_order_reasoning_and_tool_order() {
        smol::block_on(async {
            // The tool has output_index 2 but arrives before the second reasoning
            // (output_index 1). The parser must use the event's output_index so the
            // final order is sorted correctly; stable sort alone would place the
            // tool too early if it defaulted to the current accumulator length.
            let sse = "event: response.output_item.done\ndata: {\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"one\",\"summary\":[]}}\n\nevent: response.output_item.done\ndata: {\"output_index\":2,\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"one\\\"}\"}}\n\nevent: response.output_item.done\ndata: {\"output_index\":1,\"item\":{\"id\":\"rs_2\",\"type\":\"reasoning\",\"encrypted_content\":\"two\",\"summary\":[]}}\n\nevent: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n";
            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();
            assert!(
                matches!(&resp.message.content[0], ContentBlock::RedactedThinking { data } if data.contains("one"))
            );
            assert!(
                matches!(&resp.message.content[1], ContentBlock::RedactedThinking { data } if data.contains("two"))
            );
            assert!(
                matches!(&resp.message.content[2], ContentBlock::ToolUse { id, .. } if id == "c1")
            );
        });
    }

    #[test]
    fn post_response_api_error_stays_retryable() {
        let error = AgentError::Api {
            status: 500,
            message: "provider rejected request".into(),
        };

        let suppressed = error.suppress_retry_after_send(None);
        assert!(matches!(suppressed, AgentError::Api { status: 500, .. }));
        assert!(suppressed.is_retryable());
    }

    #[test]
    fn post_response_eof_is_non_retryable() {
        let error: AgentError = IoError::new(
            ErrorKind::UnexpectedEof,
            "Responses API stream ended without a terminal event",
        )
        .into();
        let metadata = RequestDeliveryMetadata::new(RequestDeliveryPhase::SentAwaitingAcceptance);

        assert!(matches!(
            error.suppress_retry_after_send(Some(metadata)),
            AgentError::RequestSent { .. }
        ));
    }

    #[test]
    fn parse_sse_rejects_missing_terminal_event() {
        smol::block_on(async {
            let (resp, _) =
                run_sse("event: response.output_text.delta\ndata: {\"delta\":\"partial\"}\n\n")
                    .await;
            assert!(matches!(
                resp,
                Err(AgentError::RequestSent {
                    metadata: Some(crate::RequestDeliveryMetadata {
                        phase: crate::RequestDeliveryPhase::SentAwaitingAcceptance,
                        ..
                    }),
                    ..
                })
            ));
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn partial_sse_eof_preserves_response_id_without_a_second_post() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = smol::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(read, 0);
                    request.extend_from_slice(&chunk[..read]);
                }
                assert!(request.starts_with(b"POST /responses HTTP/1.1\r\n"));

                let sse = "event: response.created\ndata: {\"response\":{\"id\":\"resp_partial\"}}\n\nevent: response.output_text.delta\ndata: {\"delta\":\"partial\"}\n\n";
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                    sse.len() + 16
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.flush().await.unwrap();
            });
            let client = HttpClient::new().unwrap();
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}")),
                headers: Vec::new(),
            };
            let model = Model::from_spec("openai/gpt-5.6").unwrap();
            let (event_tx, _event_rx) = flume::unbounded();
            let error = do_stream(
                &client,
                &model,
                &json!({"model":"gpt-5.6","input":[],"stream":true}),
                &event_tx,
                &auth,
                Duration::from_secs(2),
            )
            .await
            .unwrap_err();
            server.await;

            assert!(
                matches!(
                    &error,
                    AgentError::RequestSent {
                        metadata: Some(crate::RequestDeliveryMetadata {
                            phase: crate::RequestDeliveryPhase::Accepted,
                            response_id: Some(response_id),
                            ..
                        }),
                        ..
                    } if response_id == "resp_partial"
                ),
                "unexpected error: {error:?}"
            );
        });
    }

    #[test]
    fn parse_sse_captures_response_id() {
        smol::block_on(async {
            let sse = "event: response.created\ndata: {\"response\":{\"id\":\"resp_1\"}}\n\nevent: response.completed\ndata: {\"response\":{\"status\":\"completed\"}}\n\n";
            let (result, _) = run_sse(sse).await;
            let (response_id, _) = result.unwrap();
            assert_eq!(response_id.as_deref(), Some("resp_1"));
        });
    }

    #[test]
    fn parse_sse_no_reasoning_tokens_in_usage() {
        smol::block_on(async {
            let sse = "\
event: response.output_text.delta\n\
data: {\"delta\":\"Hello\"}\n\
\n\
event: response.completed\n\
data: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":40}}}}\n\
\n";

            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();

            assert_eq!(resp.usage.input, 60);
            assert_eq!(resp.usage.output, 10);
            assert_eq!(resp.usage.cache_read, 40);
        });
    }

    #[test]
    fn request_diagnostics_count_categories_without_contents() {
        let body = json!({
            "instructions": "system",
            "input": [
                {"type":"message","content":[{"type":"input_text","text":"hello"}]},
                {"type":"message","content":[{"type":"input_image","image_url":"data:image/png;base64,secret"}]},
                {"type":"function_call_output","call_id":"call","output":"tool result"},
                {"type":"reasoning","encrypted_content":"secret reasoning"}
            ],
            "tools": [{"type":"function","name":"read","parameters":{}}]
        });

        let diagnostics = request_diagnostics(&body);

        assert_eq!(diagnostics.input_items, 4);
        assert_eq!(diagnostics.text_items, 2);
        assert_eq!(diagnostics.image_items, 1);
        assert_eq!(diagnostics.tool_items, 2);
        assert_eq!(diagnostics.reasoning_items, 1);
        assert!(diagnostics.request_bytes > diagnostics.text_bytes);
        assert!(diagnostics.image_bytes > 0);
        assert!(diagnostics.tool_bytes > 0);
        assert!(diagnostics.reasoning_bytes > 0);
    }

    #[test]
    fn parse_usage_accounts_for_cache_writes() {
        let usage = parse_usage(&json!({
            "input_tokens": 150,
            "output_tokens": 20,
            "input_tokens_details": {
                "cached_tokens": 40,
                "cache_write_tokens": 50
            }
        }));

        assert_eq!(usage.input, 60);
        assert_eq!(usage.output, 20);
        assert_eq!(usage.cache_read, 40);
        assert_eq!(usage.cache_creation, 50);
    }

    #[test]
    fn build_body_with_tools_adds_parallel_tool_calls_when_enabled() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let tools = json!([{
            "name": "bash",
            "description": "run shell commands",
            "input_schema": {"type": "object"}
        }]);
        let opts = RequestOptions::default();
        let body = build_body(
            &model,
            &[],
            &System::from("system"),
            &tools,
            None,
            None,
            false,
            &opts,
            true,
        );
        assert_eq!(body["parallel_tool_calls"], true);
    }

    #[test]
    fn build_body_with_tools_omits_parallel_tool_calls_when_disabled() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let tools = json!([{
            "name": "bash",
            "description": "run shell commands",
            "input_schema": {"type": "object"}
        }]);
        let opts = RequestOptions::default();
        let body = build_body(
            &model,
            &[],
            &System::from("system"),
            &tools,
            None,
            None,
            false,
            &opts,
            false,
        );
        assert!(body.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn parse_sse_web_search_call_item() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.done\ndata: {\"output_index\":0,\"item\":{\"type\":\"web_search_call\",\"action\":{\"queries\":[{\"text\":\"rust async\"}],\"results\":[{\"title\":\"Rust async book\",\"url\":\"https://example.com\"}]}}}\n\
event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
";
            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();
            let text = resp.message.first_text_content().unwrap();
            assert!(text.contains("[search: rust async]"));
            assert!(text.contains("[Rust async book](https://example.com)"));
        });
    }

    #[test]
    fn parse_sse_file_search_call_item() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.done\ndata: {\"output_index\":0,\"item\":{\"type\":\"file_search_call\",\"results\":[{\"filename\":\"doc.txt\",\"file_id\":\"file_123\"}]}}\n\
event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
";
            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();
            let text = resp.message.first_text_content().unwrap();
            assert!(text.contains("[file:doc.txt (file_123)]"));
        });
    }

    #[test]
    fn parse_sse_code_interpreter_call_item() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.done\ndata: {\"output_index\":0,\"item\":{\"type\":\"code_interpreter_call\",\"outputs\":[{\"text\":\"Output: 42\"},{\"image\":{\"file_id\":\"img_456\"}}]}}\n\
event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
";
            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();
            let text = resp.message.first_text_content().unwrap();
            assert!(text.contains("Output: 42"));
            assert!(text.contains("[image:img_456]"));
        });
    }

    #[test]
    fn parse_sse_computer_call_output_item() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.done\ndata: {\"output_index\":0,\"item\":{\"type\":\"computer_call_output\",\"output\":{\"screenshot\":{\"file_id\":\"screenshot_789\"}}}}\n\
event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
";
            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();
            let text = resp.message.first_text_content().unwrap();
            assert!(text.contains("[screenshot:screenshot_789]"));
        });
    }

    #[test]
    fn parse_sse_program_item() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.added\ndata: {\"output_index\":0,\"item\":{\"type\":\"program\",\"code\":\"print('hello')\"}}\n\
event: response.output_item.done\ndata: {\"output_index\":0,\"item\":{\"type\":\"program\",\"code\":\"print('hello')\"}}\n\
event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
";
            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();
            let thinking = resp.message.content.iter().find_map(|b| match b {
                ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            });
            assert_eq!(thinking, Some("print('hello')"));
        });
    }

    #[test]
    fn parse_sse_program_output_item() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.added\ndata: {\"output_index\":0,\"item\":{\"type\":\"program_output\",\"output\":\"hello world\"}}\n\
event: response.output_item.done\ndata: {\"output_index\":0,\"item\":{\"type\":\"program_output\",\"output\":\"hello world\"}}\n\
event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
";
            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();
            let text = resp.message.first_text_content().unwrap();
            assert_eq!(text, "hello world");
        });
    }

    #[test]
    fn parse_sse_tool_search_items() {
        smol::block_on(async {
            let sse = "\
event: response.output_item.done\ndata: {\"output_index\":0,\"item\":{\"type\":\"tool_search_call\"}}\n\
event: response.output_item.done\ndata: {\"output_index\":1,\"item\":{\"type\":\"tool_search_output\",\"tools\":[{\"name\":\"bash\"},{\"name\":\"read\"}]}}\n\
event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\
";
            let (resp, _) = run_sse(sse).await;
            let (_, resp) = resp.unwrap();
            let text = resp.message.first_text_content().unwrap();
            assert!(text.contains("[tool_search]"));
            assert!(text.contains("[loaded 2 tools]"));
        });
    }
}
