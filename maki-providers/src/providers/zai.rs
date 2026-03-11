use std::env;

use flume::Sender;
use futures_lite::StreamExt;
use futures_lite::io::{AsyncBufReadExt, BufReader};
use isahc::{AsyncReadResponseExt, HttpClient, Request};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::model::Model;
use crate::model::{ModelEntry, ModelFamily, ModelPricing, ModelTier};
use crate::provider::{BoxFuture, Provider};
use crate::{
    AgentError, ContentBlock, Message, ProviderEvent, Role, StopReason, StreamResponse, TokenUsage,
};

pub(crate) fn models() -> &'static [ModelEntry] {
    &[
        ModelEntry {
            prefixes: &["glm-5-code"],
            tier: ModelTier::Strong,
            family: ModelFamily::Glm,
            default: true,
            pricing: ModelPricing {
                input: 1.20,
                output: 5.00,
                cache_write: 0.00,
                cache_read: 0.30,
            },
            max_output_tokens: 131072,
            context_window: 200_000,
        },
        ModelEntry {
            prefixes: &["glm-5"],
            tier: ModelTier::Strong,
            family: ModelFamily::Glm,
            default: false,
            pricing: ModelPricing {
                input: 1.00,
                output: 3.20,
                cache_write: 0.00,
                cache_read: 0.20,
            },
            max_output_tokens: 131072,
            context_window: 200_000,
        },
        ModelEntry {
            prefixes: &["glm-4.7-flash"],
            tier: ModelTier::Weak,
            family: ModelFamily::Glm,
            default: true,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
            },
            max_output_tokens: 131072,
            context_window: 200_000,
        },
        ModelEntry {
            prefixes: &["glm-4.7", "glm-4.6"],
            tier: ModelTier::Medium,
            family: ModelFamily::Glm,
            default: true,
            pricing: ModelPricing {
                input: 0.60,
                output: 2.20,
                cache_write: 0.00,
                cache_read: 0.11,
            },
            max_output_tokens: 131072,
            context_window: 200_000,
        },
        ModelEntry {
            prefixes: &["glm-4.5-flash"],
            tier: ModelTier::Weak,
            family: ModelFamily::Glm,
            default: false,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
            },
            max_output_tokens: 98304,
            context_window: 131_072,
        },
        ModelEntry {
            prefixes: &["glm-4.5-air"],
            tier: ModelTier::Weak,
            family: ModelFamily::Glm,
            default: false,
            pricing: ModelPricing {
                input: 0.20,
                output: 1.10,
                cache_write: 0.00,
                cache_read: 0.03,
            },
            max_output_tokens: 98304,
            context_window: 131_072,
        },
        ModelEntry {
            prefixes: &["glm-4.5"],
            tier: ModelTier::Medium,
            family: ModelFamily::Glm,
            default: false,
            pricing: ModelPricing {
                input: 0.60,
                output: 2.20,
                cache_write: 0.00,
                cache_read: 0.11,
            },
            max_output_tokens: 98304,
            context_window: 131_072,
        },
    ]
}

const API_KEY_ENV: &str = "ZHIPU_API_KEY";
const BASE_STANDARD: &str = "https://api.z.ai/api/paas/v4";
const BASE_CODING: &str = "https://api.z.ai/api/coding/paas/v4";
const STREAM_DONE: &str = "[DONE]";

#[derive(Debug, Clone, Copy)]
pub enum ZaiPlan {
    Standard,
    Coding,
}

pub struct Zai {
    client: HttpClient,
    api_key: String,
    completions_url: String,
    models_url: String,
}

impl Zai {
    pub fn new(plan: ZaiPlan) -> Result<Self, AgentError> {
        let api_key = env::var(API_KEY_ENV).map_err(|_| AgentError::Config {
            message: format!("{API_KEY_ENV} not set"),
        })?;
        let base = match plan {
            ZaiPlan::Standard => BASE_STANDARD,
            ZaiPlan::Coding => BASE_CODING,
        };
        Ok(Self {
            client: super::http_client(),
            api_key,
            completions_url: format!("{base}/chat/completions"),
            models_url: format!("{base}/models"),
        })
    }
}

fn convert_messages(messages: &[Message], system: &str) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": system})];

    for msg in messages {
        match msg.role {
            Role::User => {
                let mut tool_results = Vec::new();
                let mut text_parts = Vec::new();

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => text_parts.push(text.clone()),
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            tool_results.push(json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": content,
                            }));
                        }
                        ContentBlock::ToolUse { .. } => {}
                    }
                }

                if !text_parts.is_empty() {
                    out.push(json!({"role": "user", "content": text_parts.join("\n")}));
                }
                out.extend(tool_results);
            }
            Role::Assistant => {
                let mut text = String::new();
                let mut tool_calls = Vec::new();

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text: t } => text.push_str(t),
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
                        ContentBlock::ToolResult { .. } => {}
                    }
                }

                let mut msg_obj = json!({"role": "assistant"});
                if !text.is_empty() {
                    msg_obj["content"] = Value::String(text);
                }
                if !tool_calls.is_empty() {
                    msg_obj["tool_calls"] = Value::Array(tool_calls);
                }
                out.push(msg_obj);
            }
        }
    }

    out
}

fn convert_tools(anthropic_tools: &Value) -> Value {
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

impl Provider for Zai {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            let wire_messages = convert_messages(messages, system);
            let wire_tools = convert_tools(tools);

            let mut body = json!({
                "model": model.id,
                "messages": wire_messages,
                "stream": true,
                "max_tokens": model.max_output_tokens,
            });
            if wire_tools.as_array().is_some_and(|a| !a.is_empty()) {
                body["tools"] = wire_tools;
            }

            debug!(model = %model.id, num_messages = messages.len(), "sending Z.AI API request");

            let json_body = serde_json::to_vec(&body)?;
            let request = Request::builder()
                .method("POST")
                .uri(&self.completions_url)
                .header("content-type", "application/json")
                .header("authorization", &format!("Bearer {}", self.api_key))
                .body(json_body)?;
            let mut response = self.client.send_async(request).await?;
            let status = response.status().as_u16();

            if status == 429 || status >= 500 {
                let error_body = response.text().await.unwrap_or_default();
                if error_body.contains("1113") || error_body.contains("nsufficien") {
                    warn!(status, "insufficient funds, bailing out");
                    return Err(AgentError::Api {
                        status: 402,
                        message: error_body,
                    });
                }
                return Err(AgentError::Api {
                    status,
                    message: error_body,
                });
            }

            if status == 200 {
                parse_sse(response, event_tx).await
            } else {
                Err(AgentError::from_response(response).await)
            }
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<String>, AgentError>> {
        Box::pin(async move {
            let request = Request::builder()
                .method("GET")
                .uri(&self.models_url)
                .header("authorization", &format!("Bearer {}", self.api_key))
                .body(())?;
            let mut response = self.client.send_async(request).await?;
            if response.status().as_u16() != 200 {
                return Err(AgentError::from_response(response).await);
            }

            let body: Value = serde_json::from_str(&response.text().await?)?;
            let mut models: Vec<String> = body["data"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m["id"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            models.sort();
            Ok(models)
        })
    }
}

#[derive(Deserialize)]
struct ToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: Option<ChunkDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
}

#[derive(Deserialize)]
struct ChunkUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Deserialize)]
struct SseChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<ChunkUsage>,
}

struct ToolAccumulator {
    id: String,
    name: String,
    arguments: String,
}

async fn parse_sse(
    response: isahc::Response<isahc::AsyncBody>,
    event_tx: &Sender<ProviderEvent>,
) -> Result<StreamResponse, AgentError> {
    let reader = BufReader::new(response.into_body());
    let mut lines = reader.lines();

    let mut text = String::new();
    let mut tool_accumulators: Vec<ToolAccumulator> = Vec::new();
    let mut usage = TokenUsage::default();
    let mut stop_reason: Option<StopReason> = None;

    while let Some(line) = lines.next().await {
        let line = line?;
        let data = match line.strip_prefix("data: ") {
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
            return Err(ev.into_agent_error());
        }

        let chunk: SseChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "failed to parse SSE chunk");
                continue;
            }
        };

        if let Some(u) = chunk.usage {
            let cached = u
                .prompt_tokens_details
                .map(|d| d.cached_tokens)
                .unwrap_or(0);
            usage = TokenUsage {
                input: u.prompt_tokens.saturating_sub(cached),
                output: u.completion_tokens,
                cache_read: cached,
                cache_creation: 0,
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
            text.push_str(&reasoning);
            event_tx
                .send_async(ProviderEvent::ThinkingDelta { text: reasoning })
                .await?;
        }

        if let Some(content) = delta.content
            && !content.is_empty()
        {
            text.push_str(&content);
            event_tx
                .send_async(ProviderEvent::TextDelta { text: content })
                .await?;
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

    if !text.is_empty() {
        content_blocks.push(ContentBlock::Text { text });
    }

    for acc in tool_accumulators {
        let input: Value = match serde_json::from_str(&acc.arguments) {
            Ok(v) => {
                debug!(tool = %acc.name, json = %acc.arguments, "tool input JSON");
                v
            }
            Err(e) => {
                warn!(error = %e, tool = %acc.name, json = %acc.arguments, "malformed tool JSON, falling back to {{}}");
                Value::Object(Default::default())
            }
        };
        content_blocks.push(ContentBlock::ToolUse {
            id: acc.id,
            name: acc.name,
            input,
        });
    }

    Ok(StreamResponse {
        message: Message {
            role: Role::Assistant,
            content: content_blocks,
        },
        usage,
        stop_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_to_owned)]
    fn mock_response(data: &[u8]) -> isahc::Response<isahc::AsyncBody> {
        let body = isahc::AsyncBody::from_bytes_static(data.to_vec());
        isahc::Response::builder().status(200).body(body).unwrap()
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
            let resp = parse_sse(mock_response(sse.as_bytes()), &tx).await.unwrap();

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
        })
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
            let resp = parse_sse(mock_response(sse.as_bytes()), &tx).await.unwrap();

            assert!(
                matches!(&resp.message.content[0], ContentBlock::Text { text } if text == "Let me think...Hello")
            );

            let mut thinking = Vec::new();
            let mut text_deltas = Vec::new();
            while let Ok(e) = rx.try_recv() {
                match e {
                    ProviderEvent::ThinkingDelta { text } => thinking.push(text),
                    ProviderEvent::TextDelta { text } => text_deltas.push(text),
                    ProviderEvent::ToolUseStart { .. } => {}
                }
            }
            assert_eq!(thinking, vec!["Let me think", "..."]);
            assert_eq!(text_deltas, vec!["Hello"]);
        })
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
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tc_1".to_string(),
                    content: "file.txt".to_string(),
                    is_error: false,
                }],
            },
        ];

        let wire = convert_messages(&messages, "be helpful");

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
            let resp = parse_sse(mock_response(sse.as_bytes()), &tx).await.unwrap();

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
        })
    }

    #[test]
    fn parse_sse_error_payload_returns_err() {
        smol::block_on(async {
            let sse = "\
data: {\"error\":{\"message\":\"Server overloaded\",\"type\":\"overloaded_error\"}}\n";

            let (tx, _rx) = flume::unbounded();
            let err = parse_sse(mock_response(sse.as_bytes()), &tx)
                .await
                .unwrap_err();

            match err {
                AgentError::Api { status, message } => {
                    assert_eq!(status, 529);
                    assert_eq!(message, "Server overloaded");
                }
                other => panic!("expected Api error, got: {other:?}"),
            }
        })
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
            let resp = parse_sse(mock_response(sse.as_bytes()), &tx).await.unwrap();

            let tools: Vec<_> = resp.message.tool_uses().collect();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].1, "bash");
            assert_eq!(*tools[0].2, Value::Object(Default::default()));
        })
    }
}
