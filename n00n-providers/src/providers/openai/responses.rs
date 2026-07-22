use std::borrow::Cow;
use std::io::{Error as IoError, ErrorKind};
use std::time::{Duration, Instant};

use flume::Sender;
use futures_lite::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
use isahc::{HttpClient, Request};
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::providers::ResolvedAuth;
use crate::{
    AgentError, ContentBlock, Message, ProviderEvent, RequestDeliveryMetadata,
    RequestDeliveryPhase, Role, StopReason, StreamResponse, TokenUsage,
};

const RESPONSES_PATH: &str = "/responses";
const RESPONSE_IN_FLIGHT_TIMEOUT_MULTIPLIER: u32 = 6;
const MAX_RESPONSE_IN_FLIGHT_TIMEOUT: Duration = Duration::from_mins(30);

pub(crate) fn response_in_flight_timeout(stream_timeout: Duration) -> Duration {
    stream_timeout
        .saturating_mul(RESPONSE_IN_FLIGHT_TIMEOUT_MULTIPLIER)
        .min(MAX_RESPONSE_IN_FLIGHT_TIMEOUT)
}

pub(crate) fn build_body(
    model: &crate::model::Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    previous_response_id: Option<&str>,
    prompt_cache_key: Option<&str>,
    store: bool,
) -> Value {
    let input = convert_input(messages);
    let wire_tools = convert_tools(tools);

    let mut body = json!({
        "model": model.id,
        "instructions": system,
        "input": input,
        "stream": true,
        "store": store,
        "include": ["reasoning.encrypted_content"],
        "reasoning": {"summary": "auto"},
    });
    if let Some(previous_response_id) = previous_response_id {
        body["previous_response_id"] = json!(previous_response_id);
    }
    if let Some(prompt_cache_key) = prompt_cache_key {
        body["prompt_cache_key"] = json!(prompt_cache_key);
    }
    if wire_tools.as_array().is_some_and(|a| !a.is_empty()) {
        body["tools"] = wire_tools;
    }
    body
}

pub(crate) fn convert_input(messages: &[Message]) -> Value {
    let mut input = Vec::new();

    for msg in messages {
        match msg.role {
            Role::User => {
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            input.push(json!({
                                "type": "message",
                                "role": "user",
                                "content": [{"type": "input_text", "text": text}]
                            }));
                        }
                        ContentBlock::Image { source } => {
                            input.push(json!({
                                "type": "message",
                                "role": "user",
                                "content": [{"type": "input_image", "image_url": source.to_data_url()}]
                            }));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": content,
                            }));
                        }
                        ContentBlock::ToolUse { .. }
                        | ContentBlock::Thinking { .. }
                        | ContentBlock::RedactedThinking { .. } => {}
                    }
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
                        | ContentBlock::Thinking { .. } => {}
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

pub(crate) fn convert_tools(anthropic_tools: &Value) -> Value {
    let Some(tools) = anthropic_tools.as_array() else {
        return json!([]);
    };

    Value::Array(
        tools
            .iter()
            .filter_map(|t| {
                Some(json!({
                    "type": "function",
                    "name": t.get("name")?,
                    "description": t.get("description")?,
                    "parameters": t.get("input_schema")?,
                    "strict": false,
                }))
            })
            .collect(),
    )
}

fn suppress_retry_after_response(error: AgentError) -> AgentError {
    if error.is_retryable() {
        AgentError::RequestSent {
            message: error.to_string(),
            metadata: None,
        }
    } else {
        error
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
    let base = auth.base_url.as_deref().ok_or_else(|| AgentError::Config {
        message: "Responses API requires a base_url in auth".into(),
    })?;
    let json_body = serde_json::to_vec(body)?;

    let request = auth
        .configure_request(
            Request::builder()
                .method("POST")
                .uri(format!("{base}{RESPONSES_PATH}"))
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
        .map_err(suppress_retry_after_response)
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

            "response.output_item.added" => {
                let item = &data["item"];
                let output_index = data["output_index"]
                    .as_u64()
                    .unwrap_or_else(|| self.tool_accumulators.len() as u64);
                if item["type"].as_str() == Some("function_call") {
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
                let output_index = data["output_index"].as_u64().unwrap_or_else(|| {
                    (self.reasoning_items.len() + self.tool_accumulators.len()) as u64
                });
                if item["type"].as_str() == Some("reasoning") {
                    if !self.has_reasoning_item(item) {
                        self.reasoning_items.push((output_index, item.clone()));
                    }
                } else if item["type"].as_str() == Some("message") && self.text.is_empty() {
                    if let Some(content) = item["content"].as_array() {
                        for part in content {
                            if part["type"].as_str() == Some("output_text")
                                && let Some(snapshot) = part["text"].as_str()
                            {
                                self.text.push_str(snapshot);
                            }
                        }
                    }
                } else if item["type"].as_str() == Some("function_call") {
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
                    let acc = if let Some(idx) = data["output_index"].as_u64() {
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
                            output_index: self.tool_accumulators.len() as u64,
                            call_id,
                            name,
                            arguments,
                        });
                    }
                }
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
                return Err(AgentError::RequestSent {
                    message: error.to_string(),
                    metadata: Some(acc.delivery_metadata()),
                });
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
            if let Ok(ev) = serde_json::from_str::<crate::providers::SseErrorPayload>(data) {
                warn!(error_type = %ev.error.r#type, "SSE error in stream");
                return Err(ev.into_agent_error());
            }
            let parsed: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(_) => Value::Object(Default::default()),
            };
            let message = parsed["message"]
                .as_str()
                .map_or_else(|| "unknown error".to_string(), ToString::to_string);
            return Err(AgentError::Api {
                status: 500,
                message,
            });
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
        if acc.handle_event(&parsed_event, &parsed, event_tx).await? {
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

// map_or_else is required here to apply try_from conversion; unwrap_or would skip the conversion
#[allow(clippy::manual_unwrap_or)]
fn parse_usage(u: &Value) -> TokenUsage {
    // unwrap_or is disallowed; manual implementation with map_or_else is required for try_from conversion
    #[allow(clippy::manual_unwrap_or)]
    let input_tokens = u["input_tokens"].as_u64().map_or_else(
        || 0,
        |v| match u32::try_from(v) {
            Ok(v) => v,
            Err(_) => u32::MAX,
        },
    );
    // unwrap_or is disallowed; manual implementation with map_or_else is required for try_from conversion
    #[allow(clippy::manual_unwrap_or)]
    let output_tokens = u["output_tokens"].as_u64().map_or_else(
        || 0,
        |v| match u32::try_from(v) {
            Ok(v) => v,
            Err(_) => u32::MAX,
        },
    );

    // unwrap_or is disallowed; manual implementation with map_or_else is required for try_from conversion
    #[allow(clippy::manual_unwrap_or)]
    let cached = u["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .map_or_else(
            || 0,
            |v| match u32::try_from(v) {
                Ok(v) => v,
                Err(_) => u32::MAX,
            },
        );
    // unwrap_or is disallowed; manual implementation with map_or_else is required for try_from conversion
    #[allow(clippy::manual_unwrap_or)]
    let cache_write = u["input_tokens_details"]["cache_write_tokens"]
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
    use futures_lite::io::{AsyncReadExt, AsyncWriteExt, Cursor};
    use serde_json::json;

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

        let input = convert_input(&messages);
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
        let model = crate::model::Model::from_spec("openai/gpt-5.6").unwrap();
        let body = build_body(
            &model,
            &[],
            "system",
            &json!([]),
            Some("resp_1"),
            Some("session_1"),
            true,
        );
        assert_eq!(body["previous_response_id"], "resp_1");
        assert_eq!(body["prompt_cache_key"], "session_1");
        assert_eq!(body["store"], true);
        assert_eq!(body["reasoning"], json!({"summary":"auto"}));
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
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
        let input = convert_input(&messages);
        assert_eq!(input[0], reasoning_one);
        assert_eq!(input[1]["call_id"], "c1");
        assert_eq!(input[2], reasoning_two);
        assert_eq!(input[3]["call_id"], "c2");
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
    fn post_response_api_error_is_not_retried() {
        let error = AgentError::Api {
            status: 500,
            message: "provider rejected request".into(),
        };

        assert!(matches!(
            suppress_retry_after_response(error),
            AgentError::RequestSent { .. }
        ));
    }

    #[test]
    fn post_response_eof_is_non_retryable() {
        let error = IoError::new(
            ErrorKind::UnexpectedEof,
            "Responses API stream ended without a terminal event",
        )
        .into();

        assert!(matches!(
            suppress_retry_after_response(error),
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
            let model = crate::model::Model::from_spec("openai/gpt-5.6").unwrap();
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
}
