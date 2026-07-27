use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_process::Command;
use flume::Sender;
use futures_lite::StreamExt;
use futures_lite::io::{AsyncBufReadExt, BufReader};
use n00n_storage::id::SessionRef;
use serde_json::Value;
use tracing::{debug, warn};

use crate::model::{Model, ModelEntry, ModelInfo, ModelTier, TokenUsage, lookup_entry};
use crate::provider::{BoxFuture, Provider};
use crate::{
    AgentError, ContentBlock, Effort, Message, ProviderEvent, RequestOptions, Role, StopReason,
    StreamResponse, System, ThinkingConfig,
};

use super::Timeouts;

const DEFAULT_COMMAND: &str = "cursor-agent";
const COMMAND_ENV: &str = "CURSOR_AGENT_PATH";
const API_KEY_ENV: &str = "CURSOR_API_KEY";
const MODE_ENV: &str = "CURSOR_AGENT_MODE";
const WORKSPACE_ENV: &str = "CURSOR_AGENT_WORKSPACE";
const TRUST_ENV: &str = "CURSOR_AGENT_TRUST";
const YOLO_ENV: &str = "CURSOR_AGENT_YOLO";
const APPROVE_MCPS_ENV: &str = "CURSOR_AGENT_APPROVE_MCPS";
const MODEL_PARAMS_ENV: &str = "CURSOR_MODEL_PARAMS";

const RESULT_MISSING: &str = "cursor-agent finished without a result event";
const STDOUT_MISSING: &str = "cursor-agent stdout not available";
const STDERR_MISSING: &str = "cursor-agent stderr not available";
const NO_MESSAGES: &str = "no messages to send to cursor-agent";
const SPAWN_FAILED: &str = "failed to spawn cursor-agent";

inventory::submit!(n00n_config::providers::BuiltInProvider {
    slug: "cursor",
    display_name: "Cursor",
    protocol: n00n_config::providers::Protocol::Cursor,
    default_base_url: "",
    default_api_key_env: API_KEY_ENV,
    default_model: "cursor/composer-2.5",
    plans: None,
    login_url: Some("https://cursor.com/dashboard/api"),
    needs_url: false,
});

include!("cursor_models.rs");

pub(crate) const fn models() -> &'static [ModelEntry] {
    MODELS
}

struct CursorSession {
    cursor_session_id: String,
    last_message_count: usize,
}

pub(crate) struct Cursor {
    command: PathBuf,
    timeouts: Timeouts,
    mode: Option<String>,
    workspace: Option<PathBuf>,
    trust: bool,
    yolo: bool,
    approve_mcps: bool,
    api_key: Option<String>,
    sessions: Mutex<HashMap<String, CursorSession>>,
}

impl Cursor {
    pub(crate) fn new(timeouts: Timeouts) -> Self {
        let command = match std::env::var(COMMAND_ENV) {
            Ok(v) if !v.is_empty() => PathBuf::from(v),
            _ => PathBuf::from(DEFAULT_COMMAND),
        };

        let api_key = match super::KeyPool::resolve("cursor", API_KEY_ENV) {
            Ok(pool) => Some(pool.current().to_string()),
            Err(e) => {
                debug!(error = %e, "no Cursor API key configured; cursor-agent will use stored credentials");
                None
            }
        };

        Self {
            command,
            timeouts,
            mode: env_optional(MODE_ENV),
            workspace: env_optional(WORKSPACE_ENV).map(PathBuf::from),
            trust: env_flag(TRUST_ENV, true),
            yolo: env_flag(YOLO_ENV, true),
            approve_mcps: env_flag(APPROVE_MCPS_ENV, false),
            api_key,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    async fn do_stream(
        &self,
        model: &Model,
        messages: &[Message],
        system: &System,
        _tools: &Value,
        event_tx: &Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&SessionRef>,
    ) -> Result<StreamResponse, AgentError> {
        let prompt = self.build_prompt(messages, system, session_id)?;
        let mut child = self
            .build_command(model, opts, prompt, session_id)?
            .spawn()
            .map_err(|e| AgentError::Config {
                message: format!("{SPAWN_FAILED}: {e}"),
            })?;

        let stdout = child.stdout.take().ok_or_else(|| AgentError::Config {
            message: STDOUT_MISSING.into(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| AgentError::Config {
            message: STDERR_MISSING.into(),
        })?;

        let stderr_buffer = Arc::new(Mutex::new(String::new()));
        let stderr_task = smol::spawn(collect_stderr(stderr, Arc::clone(&stderr_buffer)));

        let parse = parse_stream(
            BufReader::new(stdout).lines(),
            event_tx,
            self.timeouts.stream,
        );

        let result = futures_lite::future::or(async { Some(parse.await) }, async {
            smol::Timer::after(self.timeouts.stream).await;
            None
        })
        .await;

        let Some(result) = result else {
            if let Err(e) = child.kill() {
                warn!(error = %e, "failed to kill cursor-agent after timeout");
            }
            let () = stderr_task.await;
            return Err(AgentError::Timeout {
                secs: self.timeouts.stream.as_secs(),
            });
        };
        let result = result?;

        let status = child.status().await?;
        let () = stderr_task.await;

        if !status.success() && result.result.is_none() {
            let stderr_text = stderr_buffer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            return Err(map_stderr_error(&stderr_text));
        }

        let text = if result.text.is_empty() {
            result.result.map_or(String::new(), |t| t)
        } else {
            result.text
        };

        if result.is_error {
            warn!("cursor-agent reported an error during the turn");
        }

        if text.is_empty() && result.cursor_session_id.is_none() {
            let stderr_text = stderr_buffer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            return Err(map_stderr_error(&stderr_text));
        }

        if let Some(sid) = session_id {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sessions.insert(
                sid.as_str().to_string(),
                CursorSession {
                    cursor_session_id: result.cursor_session_id.map_or(String::new(), |id| id),
                    last_message_count: messages.len(),
                },
            );
        }

        let mut content = Vec::new();
        if !result.thinking.is_empty() {
            content.push(ContentBlock::Thinking {
                thinking: result.thinking,
                signature: None,
            });
        }
        content.push(ContentBlock::Text { text });

        Ok(StreamResponse {
            message: Message {
                role: Role::Assistant,
                content,
                ..Default::default()
            },
            usage: TokenUsage::default(),
            stop_reason: Some(StopReason::EndTurn),
        })
    }

    async fn do_list_models(&self) -> Result<Vec<ModelInfo>, AgentError> {
        let mut cmd = Command::new(&self.command);
        cmd.arg("--list-models");
        if let Some(api_key) = &self.api_key {
            cmd.env(API_KEY_ENV, api_key);
        }

        let output = cmd.output().await?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(AgentError::Api {
                status: 500,
                message: format!("cursor-agent --list-models failed: {message}"),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut models = Vec::new();
        for line in stdout.lines() {
            let Some((id, display)) = line.split_once(" - ") else {
                continue;
            };
            let id = id.trim();
            let display = display.trim();
            if id.is_empty() || id == "Available" {
                continue;
            }

            let mut info = ModelInfo::id_only(id.to_string());
            info.name = Some(display.to_string());
            info.supports_thinking = Some(supports_thinking(id));
            info.supports_vision = Some(false);

            if let Ok(entry) = lookup_entry(MODELS, id) {
                info.context_window = Some(entry.context_window);
                info.max_output_tokens = Some(entry.max_output_tokens);
                info.pricing = Some(entry.pricing.clone());
                info.tier = Some(entry.tier);
            } else {
                info.context_window = Some(128_000);
                info.max_output_tokens = Some(32_768);
                info.tier = Some(ModelTier::Strong);
            }
            models.push(info);
        }
        Ok(models)
    }

    fn build_prompt(
        &self,
        messages: &[Message],
        system: &System,
        session_id: Option<&SessionRef>,
    ) -> Result<String, AgentError> {
        let session_key = session_id.map(|s| s.as_str().to_string());
        let resume = {
            let sessions = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            session_key
                .as_ref()
                .and_then(|k| sessions.get(k))
                .map(|s| s.last_message_count)
        };

        let new_messages = match resume {
            Some(last) if messages.len() > last => &messages[last..],
            _ => messages,
        };

        if new_messages.is_empty() {
            return Err(AgentError::Config {
                message: NO_MESSAGES.into(),
            });
        }

        let mut prompt = String::new();
        let system_text = system.to_string();
        if !system_text.is_empty() && resume.is_none() {
            prompt.push_str("System:\n");
            prompt.push_str(&system_text);
            prompt.push_str("\n\n");
        }

        for (i, message) in new_messages.iter().enumerate() {
            if i > 0 {
                prompt.push_str("\n\n");
            }
            prompt.push_str(role_label(&message.role));
            prompt.push_str(":\n");
            for block in &message.content {
                append_content_block(&mut prompt, block);
            }
        }

        Ok(prompt)
    }

    fn build_command(
        &self,
        model: &Model,
        opts: RequestOptions,
        prompt: String,
        session_id: Option<&SessionRef>,
    ) -> Result<Command, AgentError> {
        let mut cmd = Command::new(&self.command);
        cmd.arg("--print")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--stream-partial-output");

        if self.trust {
            cmd.arg("--trust");
        }
        if self.yolo {
            cmd.arg("--yolo");
        }
        if self.approve_mcps {
            cmd.arg("--approve-mcps");
        }
        if let Some(mode) = &self.mode {
            cmd.arg("--mode").arg(mode);
        }

        if let Some(workspace) = &self.workspace {
            cmd.arg("--workspace").arg(workspace);
        } else {
            let current = std::env::current_dir().map_err(|e| AgentError::Config {
                message: format!("cannot determine current directory: {e}"),
            })?;
            cmd.arg("--workspace").arg(current);
        }

        if let Some(api_key) = &self.api_key {
            cmd.env(API_KEY_ENV, api_key);
        }

        cmd.arg("--model").arg(format_model_id(model, opts));

        if let Some(sid) = self.cursor_session_id(session_id) {
            cmd.arg("--resume").arg(sid);
        }

        cmd.arg(prompt);
        Ok(cmd)
    }

    fn cursor_session_id(&self, session_id: Option<&SessionRef>) -> Option<String> {
        let session_key = session_id.map(|s| s.as_str().to_string())?;
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions
            .get(&session_key)
            .map(|s| s.cursor_session_id.clone())
    }
}

impl Provider for Cursor {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a System,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            self.do_stream(model, messages, system, tools, event_tx, opts, session_id)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>> {
        Box::pin(async move { self.do_list_models().await })
    }

    fn fetch_usage(&self) -> BoxFuture<'_, Result<Option<crate::ProviderUsage>, AgentError>> {
        Box::pin(async { Ok(None) })
    }

    fn refresh_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }

    fn reload_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async { Ok(false) })
    }

    fn adjust_model(&self, model: &mut Model) {
        // The CLI provider cannot accept image blocks, so ensure n00n adapts
        // them to text notes before they reach the prompt builder.
        if model.supports_vision_override.is_none() {
            model.supports_vision_override = Some(false);
        }
    }
}

struct CursorResult {
    text: String,
    thinking: String,
    cursor_session_id: Option<String>,
    result: Option<String>,
    is_error: bool,
}

async fn parse_stream(
    mut lines: futures_lite::io::Lines<BufReader<async_process::ChildStdout>>,
    event_tx: &Sender<ProviderEvent>,
    _stream_timeout: Duration,
) -> Result<CursorResult, AgentError> {
    let mut text = String::new();
    let mut text_last = String::new();
    let mut thinking = String::new();
    let mut thinking_last = String::new();
    let mut cursor_session_id = None;
    let mut result = None;
    let mut is_error = false;

    while let Some(line) = lines.next().await {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                debug!(line, error = %e, "cursor-agent emitted non-JSON line");
                continue;
            }
        };

        let Some(kind) = value.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };

        match kind {
            "system" => {
                if let Some(id) = value.get("session_id").and_then(serde_json::Value::as_str) {
                    cursor_session_id = Some(id.to_string());
                }
            }
            "assistant" => {
                handle_assistant_event(&value, &mut text, &mut text_last, event_tx).await?;
            }
            "thinking" => {
                handle_thinking_event(&value, &mut thinking, &mut thinking_last, event_tx).await?;
            }
            "tool_call" => {
                handle_tool_call_event(&value, event_tx).await?;
            }
            "result" => {
                result = value
                    .get("result")
                    .and_then(serde_json::Value::as_str)
                    .map(String::from);
                is_error = value
                    .get("is_error")
                    .and_then(serde_json::Value::as_bool)
                    .map_or(false, |v| v);
                if let Some(id) = value.get("session_id").and_then(serde_json::Value::as_str) {
                    cursor_session_id = Some(id.to_string());
                }
            }
            _ => {}
        }
    }

    Ok(CursorResult {
        text,
        thinking,
        cursor_session_id,
        result,
        is_error,
    })
}

async fn handle_assistant_event(
    value: &Value,
    text: &mut String,
    text_last: &mut String,
    event_tx: &Sender<ProviderEvent>,
) -> Result<(), AgentError> {
    let full = extract_text_content(value);
    if full.is_empty() {
        return Ok(());
    }

    let delta = if full.starts_with(text_last.as_str()) && full.len() >= text_last.len() {
        &full[text_last.len()..]
    } else {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
            event_tx
                .send_async(ProviderEvent::TextDelta {
                    text: "\n".to_string(),
                })
                .await?;
        }
        text_last.clear();
        full.as_str()
    };

    if !delta.is_empty() {
        event_tx
            .send_async(ProviderEvent::TextDelta {
                text: delta.to_string(),
            })
            .await?;
        text.push_str(delta);
    }
    *text_last = full;
    Ok(())
}

async fn handle_thinking_event(
    value: &Value,
    thinking: &mut String,
    thinking_last: &mut String,
    event_tx: &Sender<ProviderEvent>,
) -> Result<(), AgentError> {
    let full = value
        .get("text")
        .and_then(serde_json::Value::as_str)
        .map_or("", |v| v)
        .to_string();
    if full.is_empty() {
        return Ok(());
    }

    let delta = if full.starts_with(thinking_last.as_str()) && full.len() >= thinking_last.len() {
        &full[thinking_last.len()..]
    } else {
        if !thinking.is_empty() && !thinking.ends_with('\n') {
            thinking.push('\n');
            event_tx
                .send_async(ProviderEvent::ThinkingDelta {
                    text: "\n".to_string(),
                })
                .await?;
        }
        thinking_last.clear();
        full.as_str()
    };

    if !delta.is_empty() {
        event_tx
            .send_async(ProviderEvent::ThinkingDelta {
                text: delta.to_string(),
            })
            .await?;
        thinking.push_str(delta);
    }
    *thinking_last = full;
    Ok(())
}

async fn handle_tool_call_event(
    value: &Value,
    event_tx: &Sender<ProviderEvent>,
) -> Result<(), AgentError> {
    if !matches!(
        value.get("subtype").and_then(serde_json::Value::as_str),
        Some("started")
    ) {
        return Ok(());
    }
    let Some(call_id) = value.get("call_id").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let Some(tool_call) = value.get("tool_call") else {
        return Ok(());
    };
    let name = tool_name(tool_call);
    event_tx
        .send_async(ProviderEvent::ToolUseStart {
            id: call_id.to_string(),
            name: name.to_string(),
        })
        .await?;
    Ok(())
}

fn tool_name(tool_call: &Value) -> &str {
    if tool_call.get("readToolCall").is_some() {
        return "read";
    }
    if tool_call.get("writeToolCall").is_some() {
        return "write";
    }
    if let Some(function) = tool_call.get("function")
        && let Some(name) = function.get("name").and_then(serde_json::Value::as_str)
    {
        return name;
    }
    "tool"
}

fn extract_text_content(value: &Value) -> String {
    let mut out = String::new();
    let Some(content) = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return out;
    };
    for item in content {
        if let Some(text) = item.get("text").and_then(serde_json::Value::as_str) {
            out.push_str(text);
        }
    }
    out
}

async fn collect_stderr(reader: async_process::ChildStderr, buffer: Arc<Mutex<String>>) {
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next().await {
        match line {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                {
                    let mut buf = buffer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    buf.push_str(line);
                    buf.push('\n');
                }
                if line.to_lowercase().contains("usage limit")
                    || line.to_lowercase().contains("rate limit")
                {
                    warn!(stderr = %line, "cursor-agent usage/rate limit");
                } else {
                    debug!(stderr = %line, "cursor-agent stderr");
                }
            }
            Err(e) => {
                warn!(error = %e, "cursor-agent stderr read error");
            }
        }
    }
}

fn map_stderr_error(stderr: &str) -> AgentError {
    let lower = stderr.to_lowercase();
    if lower.contains("usage limit") || lower.contains("insufficient quota") {
        return AgentError::Api {
            status: 402,
            message: "Cursor usage limit reached".into(),
        };
    }
    if lower.contains("rate limit") || lower.contains("too many requests") {
        return AgentError::Api {
            status: 429,
            message: "Cursor rate limit hit".into(),
        };
    }
    if lower.contains("not logged in") || lower.contains("invalid api key") {
        return AgentError::Api {
            status: 401,
            message: "Cursor authentication failed; run `cursor login`".into(),
        };
    }
    if stderr.is_empty() {
        return AgentError::Api {
            status: 500,
            message: RESULT_MISSING.into(),
        };
    }
    AgentError::Api {
        status: 500,
        message: stderr.to_string(),
    }
}

fn format_model_id(model: &Model, opts: RequestOptions) -> String {
    let id = &model.id;
    if id.contains('[') {
        return id.clone();
    }

    if let Some(params) = env_optional(MODEL_PARAMS_ENV)
        && !params.is_empty()
    {
        return format!("{id}[{params}]");
    }

    let effort = match opts.thinking {
        ThinkingConfig::Off => None,
        ThinkingConfig::Adaptive | ThinkingConfig::Effort(Effort::Medium) => Some("medium"),
        ThinkingConfig::Effort(Effort::Minimal | Effort::Low) => Some("low"),
        ThinkingConfig::Effort(Effort::High | Effort::XHigh | Effort::Max)
        | ThinkingConfig::Budget(_) => Some("high"),
    };

    match effort {
        Some(e) => format!("{id}[effort={e}]"),
        None => id.clone(),
    }
}

fn append_content_block(prompt: &mut String, block: &ContentBlock) {
    match block {
        ContentBlock::Text { text } => prompt.push_str(text),
        ContentBlock::Thinking { thinking, .. } => {
            prompt.push_str("\n[thinking: ");
            prompt.push_str(thinking);
            prompt.push(']');
        }
        ContentBlock::RedactedThinking { data } => {
            prompt.push_str("\n[redacted thinking: ");
            prompt.push_str(data);
            prompt.push(']');
        }
        ContentBlock::ToolUse { name, input, .. } => {
            prompt.push_str("\n[tool use: ");
            prompt.push_str(name);
            prompt.push(' ');
            prompt.push_str(&input.to_string());
            prompt.push(']');
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            prompt.push_str("\n[tool result for ");
            prompt.push_str(tool_use_id);
            if *is_error {
                prompt.push_str(" (error)");
            }
            prompt.push_str(": ");
            prompt.push_str(content);
            prompt.push(']');
        }
        ContentBlock::Image { source } => {
            prompt.push_str("\n[image: ");
            prompt.push_str(&source.to_data_url());
            prompt.push(']');
        }
    }
}

fn role_label(role: &Role) -> &'static str {
    match role {
        Role::User => "User",
        Role::Assistant => "Assistant",
    }
}

fn supports_thinking(model_id: &str) -> bool {
    !model_id.contains("flash-minimal") && !model_id.contains("flash-low")
}

fn env_optional(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(v.as_str(), "1" | "true" | "yes"),
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelPricing;
    use crate::types::SystemBlock;

    fn test_cursor() -> Cursor {
        Cursor {
            command: PathBuf::from("/bin/true"),
            timeouts: Timeouts::default(),
            mode: None,
            workspace: None,
            trust: true,
            yolo: true,
            approve_mcps: false,
            api_key: None,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn test_model(id: &str) -> Model {
        Model {
            id: id.into(),
            provider: std::sync::Arc::from("cursor"),
            tier: ModelTier::Strong,
            family: crate::model::ModelFamily::Generic,
            supports_tool_examples_override: None,
            supports_thinking_override: None,
            supports_vision_override: None,
            pricing: ModelPricing::ZERO,
            max_output_tokens: Some(32_768),
            context_window: 128_000,
        }
    }

    fn user_message(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
            ..Default::default()
        }
    }

    #[test]
    fn models_list_is_not_empty() {
        assert!(!MODELS.is_empty());
        assert!(lookup_entry(MODELS, "composer-2.5").is_ok());
        assert!(lookup_entry(MODELS, "claude-opus-5-thinking-low-fast").is_ok());
    }

    #[test]
    fn format_model_id_passthrough_with_brackets() {
        let model = test_model("claude-opus-4-8[context=1m,effort=high]");
        assert_eq!(
            format_model_id(&model, RequestOptions::default()),
            "claude-opus-4-8[context=1m,effort=high]"
        );
    }

    #[test]
    fn format_model_id_appends_effort() {
        let model = test_model("composer-2.5");
        let opts = RequestOptions {
            thinking: ThinkingConfig::Effort(Effort::High),
            ..Default::default()
        };
        assert_eq!(format_model_id(&model, opts), "composer-2.5[effort=high]");
    }

    #[test]
    fn format_model_id_off_keeps_plain_id() {
        let model = test_model("composer-2.5");
        assert_eq!(
            format_model_id(&model, RequestOptions::default()),
            "composer-2.5"
        );
    }

    #[test]
    fn build_prompt_includes_system_on_first_call() {
        let cursor = test_cursor();
        let mut system = System::new();
        system.push(SystemBlock::new("be helpful", crate::CacheControl::None));
        let messages = vec![user_message("hi")];
        let prompt = cursor.build_prompt(&messages, &system, None).unwrap();
        assert!(prompt.contains("System:"));
        assert!(prompt.contains("be helpful"));
        assert!(prompt.contains("User:\nhi"));
    }

    #[test]
    fn build_prompt_skips_system_on_resume() {
        let cursor = test_cursor();
        let mut system = System::new();
        system.push(SystemBlock::new("be helpful", crate::CacheControl::None));
        let session_ref = SessionRef::generate();
        let key = session_ref.as_str().to_string();
        {
            let mut sessions = cursor.sessions.lock().unwrap();
            sessions.insert(
                key,
                CursorSession {
                    cursor_session_id: "c1".into(),
                    last_message_count: 1,
                },
            );
        }
        let messages = vec![user_message("first"), user_message("second")];
        let prompt = cursor
            .build_prompt(&messages, &system, Some(&session_ref))
            .unwrap();
        assert!(!prompt.contains("System:"));
        assert!(!prompt.contains("first"));
        assert!(prompt.contains("second"));
    }

    #[test]
    fn extract_text_content_joins_blocks() {
        let value = serde_json::json!({
            "message": {
                "content": [
                    { "type": "text", "text": "hello " },
                    { "type": "text", "text": "world" }
                ]
            }
        });
        assert_eq!(extract_text_content(&value), "hello world");
    }

    #[test]
    fn map_stderr_error_detects_usage_limit() {
        let err = map_stderr_error("Error: usage limit exceeded");
        assert!(matches!(err, AgentError::Api { status: 402, .. }));
    }

    #[test]
    fn map_stderr_error_detects_auth_failure() {
        let err = map_stderr_error("Error: not logged in");
        assert!(matches!(err, AgentError::Api { status: 401, .. }));
    }
}
