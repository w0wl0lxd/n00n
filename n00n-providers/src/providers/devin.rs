use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_lock::{Mutex as AsyncMutex, OnceCell};
use async_process::{Command, Stdio};
use flume::Sender;
use futures_lite::StreamExt;
use futures_lite::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use n00n_storage::id::SessionRef;
use serde_json::Value;
use tracing::{debug, error, warn};

use agent_client_protocol_schema::{
    AgentCapabilities, ClientCapabilities, EmbeddedResourceResource, ImageContent,
    InitializeRequest, InitializeResponse,
};
use agent_client_protocol_schema::{
    ContentBlock as AcpContentBlock, Error as AcpError, JsonRpcMessage, NewSessionRequest,
    NewSessionResponse, Notification, PermissionOption, PermissionOptionKind, PromptRequest,
    ProtocolVersion, Request as AcpRequest, RequestId, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, Response as AcpResponse,
    SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOption, SessionConfigSelectOptions, SessionConfigValueId, SessionId,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, StopReason as AcpStopReason,
    TextContent, ToolCallContent, UsageUpdate,
};
#[allow(unused_imports)]
use agent_client_protocol_schema::{ToolCall, ToolCallUpdate};

use crate::model::{ModelEntry, ModelFamily, ModelPricing, ModelTier};
use crate::provider::{BoxFuture, Provider};
use crate::types::{Role, System};
use crate::{
    AgentError, Effort, Message, ProviderEvent, RequestOptions, StopReason, StreamResponse,
    ThinkingConfig, TokenUsage,
};

use super::ResolvedAuth;

inventory::submit!(n00n_config::providers::BuiltInProvider {
    slug: "devin",
    display_name: "Devin",
    protocol: n00n_config::providers::Protocol::Devin,
    default_base_url: "",
    default_api_key_env: "DEVIN_API_KEY",
    default_model: "devin/swe-1-7-max",
    plans: None,
    login_url: None,
    needs_url: false,
});

const DEFAULT_COMMAND: &str = "devin";
const REQUEST_PERMISSION_METHOD: &str = "session/request_permission";

pub(crate) const fn models() -> &'static [ModelEntry] {
    &[
        ModelEntry {
            prefixes: &["swe-1-7"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: false,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 262_144,
        },
        ModelEntry {
            prefixes: &["swe-1-7-max"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: true,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 262_144,
        },
        ModelEntry {
            prefixes: &["swe-1-7-lightning"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: false,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 262_144,
        },
        ModelEntry {
            prefixes: &["claude-sonnet-4-6"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: false,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 200_000,
        },
        ModelEntry {
            prefixes: &["gpt-5-4-none"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: false,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 200_000,
        },
        ModelEntry {
            prefixes: &["gemini-3-1-pro-low"],
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            vision: true,
            default: false,
            pricing: ModelPricing {
                input: 0.00,
                output: 0.00,
                cache_write: 0.00,
                cache_read: 0.00,
                fast: None,
            },
            max_output_tokens: 128_000,
            context_window: 1_000_000,
        },
    ]
}

type PendingResponse = flume::Sender<Result<Value, AgentError>>;

struct DevinInner {
    _child: async_process::Child,
    stdin: Arc<AsyncMutex<async_process::ChildStdin>>,
    pending: Arc<AsyncMutex<HashMap<RequestId, PendingResponse>>>,
    sessions: Arc<AsyncMutex<HashMap<SessionRef, SessionId>>>,
    config_options: Arc<AsyncMutex<Vec<SessionConfigOption>>>,
    next_id: Arc<AsyncMutex<i64>>,
    event_tx: Arc<AsyncMutex<Option<Sender<ProviderEvent>>>>,
    text: Arc<AsyncMutex<String>>,
    thinking: Arc<AsyncMutex<String>>,
    usage: Arc<AsyncMutex<TokenUsage>>,
    agent_capabilities: Arc<AsyncMutex<Option<AgentCapabilities>>>,
}

impl DevinInner {
    async fn spawn(command: &str, api_key: Option<&str>) -> Result<Self, AgentError> {
        let mut cmd = Command::new(command);
        cmd.arg("acp");
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // The Devin CLI may be a system binary; do not inherit a bundled glibc
        // search path that n00n's wrapper sets for the n00n binary itself.
        cmd.env_remove("LD_LIBRARY_PATH");

        if let Some(key) = api_key {
            cmd.env("DEVIN_API_KEY", key);
        }

        let mut child = cmd.spawn().map_err(|e| AgentError::Config {
            message: format!("failed to spawn devin acp: {e}"),
        })?;

        let stdin = child.stdin.take().ok_or_else(|| AgentError::Config {
            message: "failed to capture stdin".to_string(),
        })?;

        let stdout = child.stdout.take().ok_or_else(|| AgentError::Config {
            message: "failed to capture stdout".to_string(),
        })?;

        let stderr = child.stderr.take().ok_or_else(|| AgentError::Config {
            message: "failed to capture stderr".to_string(),
        })?;

        let pending = Arc::new(AsyncMutex::new(HashMap::new()));
        let sessions = Arc::new(AsyncMutex::new(HashMap::new()));
        let config_options = Arc::new(AsyncMutex::new(Vec::new()));
        let next_id = Arc::new(AsyncMutex::new(0));
        let stdin_arc = Arc::new(AsyncMutex::new(stdin));
        let event_tx = Arc::new(AsyncMutex::new(None));
        let text = Arc::new(AsyncMutex::new(String::new()));
        let thinking = Arc::new(AsyncMutex::new(String::new()));
        let usage = Arc::new(AsyncMutex::new(TokenUsage::default()));
        let agent_capabilities = Arc::new(AsyncMutex::new(None));

        let pending_clone = Arc::clone(&pending);
        let stdin_clone = Arc::clone(&stdin_arc);
        let event_tx_clone = Arc::clone(&event_tx);
        let text_clone = Arc::clone(&text);
        let thinking_clone = Arc::clone(&thinking);
        let usage_clone = Arc::clone(&usage);

        smol::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Some(result) = lines.next().await {
                if let Ok(line) = result
                    && let Err(e) = Self::handle_line(
                        &line,
                        &pending_clone,
                        &stdin_clone,
                        &event_tx_clone,
                        &text_clone,
                        &thinking_clone,
                        &usage_clone,
                    )
                    .await
                {
                    debug!(error = %e, "failed to handle ACP line");
                }
            }
        })
        .detach();

        smol::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();

            while let Some(Ok(line)) = lines.next().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed.contains(" ERROR ") {
                    error!("devin acp stderr: {trimmed}");
                } else if trimmed.contains(" WARN ")
                    && trimmed.contains("config_importers")
                    && trimmed.contains("Failed to parse JSONC")
                    && (trimmed.contains("PreCompact") || trimmed.contains("PostCompact"))
                {
                    // n00n-agent implements these hooks directly; devin's config importer
                    // is outdated and logs a warning for valid Claude Code hook names.
                    debug!("devin acp stderr: {trimmed}");
                } else if trimmed.contains(" WARN ") {
                    warn!("devin acp stderr: {trimmed}");
                } else {
                    debug!("devin acp stderr: {trimmed}");
                }
            }
        })
        .detach();

        let inner = Self {
            _child: child,
            stdin: stdin_arc,
            pending,
            sessions,
            config_options,
            next_id,
            event_tx,
            text,
            thinking,
            usage,
            agent_capabilities,
        };

        inner.initialize().await?;
        Ok(inner)
    }

    async fn initialize(&self) -> Result<(), AgentError> {
        let req = InitializeRequest::new(ProtocolVersion::V1)
            .client_capabilities(ClientCapabilities::default());

        let response: InitializeResponse =
            self.send_request("initialize", req)
                .await
                .map_err(|e| AgentError::Config {
                    message: format!("initialize failed: {e}"),
                })?;

        if response.protocol_version != ProtocolVersion::V1 {
            return Err(AgentError::Config {
                message: format!(
                    "unsupported protocol version: {:?}",
                    response.protocol_version
                ),
            });
        }

        *self.agent_capabilities.lock().await = Some(response.agent_capabilities);

        Ok(())
    }

    async fn send_request<Params, Resp>(
        &self,
        method: &str,
        params: Params,
    ) -> Result<Resp, AgentError>
    where
        Params: serde::Serialize,
        Resp: for<'de> serde::Deserialize<'de>,
    {
        let id = {
            let mut next = self.next_id.lock().await;
            let id = *next;
            *next += 1;
            RequestId::Number(id)
        };

        let (tx, rx) = flume::bounded(1);

        {
            let mut pending = self.pending.lock().await;
            pending.insert(id.clone(), tx);
        }

        let request = AcpRequest {
            id: id.clone(),
            method: method.into(),
            params: Some(params),
        };

        let message = JsonRpcMessage::wrap(request);
        let json = serde_json::to_string(&message).map_err(|e| AgentError::Config {
            message: format!("failed to serialize request: {e}"),
        })?;

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(json.as_bytes())
                .await
                .map_err(|e| AgentError::Config {
                    message: format!("failed to write to stdin: {e}"),
                })?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|e| AgentError::Config {
                    message: format!("failed to write newline: {e}"),
                })?;
            stdin.flush().await.map_err(|e| AgentError::Config {
                message: format!("failed to flush stdin: {e}"),
            })?;
        }

        let result = rx.recv_async().await.map_err(|e| AgentError::Config {
            message: format!("failed to receive response: {e}"),
        })?;

        let result = result?;

        let result: Resp = serde_json::from_value(result).map_err(|e| AgentError::Config {
            message: format!("failed to deserialize response: {e}"),
        })?;

        Ok(result)
    }

    async fn get_or_create_session(
        &self,
        session_ref: &SessionRef,
    ) -> Result<SessionId, AgentError> {
        {
            let sessions = self.sessions.lock().await;
            if let Some(session_id) = sessions.get(session_ref) {
                return Ok(session_id.clone());
            }
        }

        let cwd = match std::env::current_dir() {
            Ok(p) => p,
            Err(_) => PathBuf::from("."),
        };
        let req = NewSessionRequest::new(cwd);
        let response: NewSessionResponse =
            self.send_request("session/new", req)
                .await
                .map_err(|e| AgentError::Config {
                    message: format!("session/new failed: {e}"),
                })?;

        let session_id = response.session_id;

        if let Some(opts) = response.config_options {
            let mut config_options = self.config_options.lock().await;
            *config_options = opts;
        }

        let mut sessions = self.sessions.lock().await;
        sessions.insert(session_ref.clone(), session_id.clone());

        Ok(session_id)
    }

    async fn apply_model_config(
        &self,
        session_id: &SessionId,
        model: &crate::model::Model,
        opts: &RequestOptions,
    ) -> Result<(), AgentError> {
        let model_value = model
            .id
            .split('/')
            .next_back()
            .unwrap_or_else(|| model.id.as_str());
        let (config_id, current_value, parsed) = {
            let guard = self.config_options.lock().await;
            let Some(option) = guard
                .iter()
                .find(|o| o.category == Some(SessionConfigOptionCategory::Model))
            else {
                return Ok(());
            };
            let SessionConfigKind::Select(select) = &option.kind else {
                return Ok(());
            };
            let options = flatten_options(&select.options);
            if options.is_empty() {
                return Ok(());
            }
            let parsed: Vec<ParsedModelValue> = options.iter().map(|o| parse_option(o)).collect();
            (option.id.clone(), select.current_value.clone(), parsed)
        };

        let desired = select_model_value(
            &parsed,
            model_value,
            opts.thinking,
            model.max_thinking_budget(),
        );
        if desired == current_value.0.as_ref() {
            return Ok(());
        }

        let req = SetSessionConfigOptionRequest::new(
            session_id.clone(),
            config_id,
            SessionConfigValueId::new(desired),
        );
        if let Err(e) = self
            .send_request::<SetSessionConfigOptionRequest, Value>("session/set_config_option", req)
            .await
        {
            debug!(error = %e, "failed to set devin model option");
        }
        Ok(())
    }

    async fn handle_line(
        line: &str,
        pending: &Arc<AsyncMutex<HashMap<RequestId, PendingResponse>>>,
        stdin: &Arc<AsyncMutex<async_process::ChildStdin>>,
        event_tx: &Arc<AsyncMutex<Option<Sender<ProviderEvent>>>>,
        text: &Arc<AsyncMutex<String>>,
        thinking: &Arc<AsyncMutex<String>>,
        usage: &Arc<AsyncMutex<TokenUsage>>,
    ) -> Result<(), AgentError> {
        let value: Value = serde_json::from_str(line).map_err(|e| AgentError::Config {
            message: format!("failed to parse JSON-RPC line: {e}"),
        })?;

        let message: JsonRpcMessage<Value> =
            serde_json::from_value(value).map_err(|e| AgentError::Config {
                message: format!("failed to parse JSON-RPC message: {e}"),
            })?;

        let Value::Object(obj) = message.inner() else {
            return Ok(());
        };

        if obj.contains_key("result") || obj.contains_key("error") {
            if let Ok(response) =
                serde_json::from_value::<AcpResponse<Value>>(Value::Object(obj.clone()))
            {
                let id = match &response {
                    AcpResponse::Result { id, .. } | AcpResponse::Error { id, .. } => id.clone(),
                };

                let result = match response {
                    AcpResponse::Result { result, .. } => Ok(result),
                    AcpResponse::Error { error, .. } => Err(AgentError::Api {
                        status: 500,
                        message: error.message,
                    }),
                };

                let mut pending = pending.lock().await;
                if let Some(tx) = pending.remove(&id)
                    && let Err(e) = tx.send(result)
                {
                    debug!(error = %e, "response receiver dropped");
                }
            }
        } else if obj.contains_key("method") {
            if obj.contains_key("id") {
                if let Ok(request) =
                    serde_json::from_value::<AcpRequest<Value>>(Value::Object(obj.clone()))
                {
                    Self::handle_incoming_request(&request, stdin).await?;
                }
            } else if let Ok(notification) =
                serde_json::from_value::<Notification<Value>>(Value::Object(obj.clone()))
            {
                if notification.method.as_ref() == "session/update" {
                    if let Some(params) = notification.params {
                        match serde_json::from_value::<SessionNotification>(params) {
                            Ok(sn) => {
                                Self::handle_session_update(
                                    sn.update, event_tx, text, thinking, usage,
                                )
                                .await?;
                            }
                            Err(e) => {
                                debug!(error = %e, "failed to parse session/update");
                            }
                        }
                    }
                } else {
                    debug!(method = %notification.method, "received ACP notification");
                }
            }
        }

        Ok(())
    }

    async fn handle_incoming_request(
        request: &AcpRequest<Value>,
        stdin: &Arc<AsyncMutex<async_process::ChildStdin>>,
    ) -> Result<(), AgentError> {
        let response: AcpResponse<Value> = match request.method.as_ref() {
            REQUEST_PERMISSION_METHOD | "requestPermission" => {
                let permission = match request.params.as_ref() {
                    Some(p) => {
                        match serde_json::from_value::<RequestPermissionRequest>(p.clone()) {
                            Ok(r) => Some(r),
                            Err(e) => {
                                debug!(error = %e, "failed to parse requestPermission request");
                                None
                            }
                        }
                    }
                    None => None,
                };

                let allowed = |o: &&PermissionOption| {
                    matches!(
                        o.kind,
                        PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
                    )
                };
                let option_id = permission
                    .as_ref()
                    .and_then(|p| {
                        // Prefer the most permissive option to avoid repeated prompts
                        // in non-interactive benchmark runs.
                        [
                            "switch_bypass",
                            "allow_always",
                            "allow_session",
                            "allow_once",
                        ]
                        .iter()
                        .find_map(|id| {
                            p.options
                                .iter()
                                .find(|o| allowed(o) && o.option_id.to_string().as_str() == *id)
                        })
                        .or_else(|| p.options.iter().find(|o| allowed(o)))
                        .or_else(|| p.options.first())
                    })
                    .map_or_else(|| "approve".to_string(), |o| o.option_id.to_string());

                let outcome =
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id));
                let result = serde_json::to_value(RequestPermissionResponse::new(outcome))
                    .map_err(|e| AgentError::Config {
                        message: format!("failed to serialize permission response: {e}"),
                    })?;

                AcpResponse::Result {
                    id: request.id.clone(),
                    result,
                }
            }
            _ => AcpResponse::Error {
                id: request.id.clone(),
                error: AcpError::new(-32601, "method_not_found"),
            },
        };

        let message = JsonRpcMessage::wrap(response);
        let json = serde_json::to_string(&message).map_err(|e| AgentError::Config {
            message: format!("failed to serialize response: {e}"),
        })?;

        let mut stdin = stdin.lock().await;
        stdin
            .write_all(json.as_bytes())
            .await
            .map_err(|e| AgentError::Config {
                message: format!("failed to write response: {e}"),
            })?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| AgentError::Config {
                message: format!("failed to write newline: {e}"),
            })?;
        stdin.flush().await.map_err(|e| AgentError::Config {
            message: format!("failed to flush response: {e}"),
        })?;

        Ok(())
    }

    fn acp_content_to_text(block: &AcpContentBlock) -> Option<String> {
        match block {
            AcpContentBlock::Text(t) => Some(t.text.clone()),
            AcpContentBlock::Image(_) => Some("[image]".to_string()),
            AcpContentBlock::Audio(_) => Some("[audio]".to_string()),
            AcpContentBlock::ResourceLink(r) => Some(format!("[resource: {}]", r.uri)),
            AcpContentBlock::Resource(r) => match &r.resource {
                EmbeddedResourceResource::TextResourceContents(t) => Some(t.text.clone()),
                EmbeddedResourceResource::BlobResourceContents(b) => {
                    Some(format!("[binary resource: {}]", b.uri))
                }
                _ => None,
            },
            _ => None,
        }
    }

    async fn handle_session_update(
        update: SessionUpdate,
        event_tx: &Arc<AsyncMutex<Option<Sender<ProviderEvent>>>>,
        text: &Arc<AsyncMutex<String>>,
        thinking: &Arc<AsyncMutex<String>>,
        usage: &Arc<AsyncMutex<TokenUsage>>,
    ) -> Result<(), AgentError> {
        match update {
            SessionUpdate::UserMessageChunk(_) => {
                // Ignore: echo of user message
            }
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let Some(chunk_text) = Self::acp_content_to_text(&chunk.content) {
                    {
                        let mut text = text.lock().await;
                        text.push_str(&chunk_text);
                    }
                    let tx = event_tx.lock().await.as_ref().cloned();
                    if let Some(tx) = tx
                        && let Err(e) = tx
                            .send_async(ProviderEvent::TextDelta { text: chunk_text })
                            .await
                    {
                        debug!(error = %e, "failed to send text delta");
                    }
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                if let AcpContentBlock::Text(t) = chunk.content {
                    let delta_text = t.text;
                    if !delta_text.is_empty() {
                        let mut thinking_guard = thinking.lock().await;
                        thinking_guard.push_str(&delta_text);
                        drop(thinking_guard);
                        let tx = event_tx.lock().await.as_ref().cloned();
                        if let Some(tx) = tx
                            && let Err(e) = tx
                                .send_async(ProviderEvent::ThinkingDelta { text: delta_text })
                                .await
                        {
                            debug!(error = %e, "failed to send thinking delta");
                        }
                    }
                } else {
                    debug!(
                        content = ?chunk.content,
                        "ignoring non-text agent thought chunk"
                    );
                }
            }
            SessionUpdate::ToolCall(call) => {
                let tx = event_tx.lock().await.as_ref().cloned();
                if let Some(tx) = tx
                    && let Err(e) = tx
                        .send_async(ProviderEvent::ToolUseStart {
                            id: call.tool_call_id.to_string(),
                            name: call.title,
                        })
                        .await
                {
                    debug!(error = %e, "failed to send tool use start");
                }
            }
            SessionUpdate::ToolCallUpdate(update) => {
                if let Some(content) = update.fields.content {
                    let tx = event_tx.lock().await.as_ref().cloned();
                    if let Some(tx) = tx {
                        for item in content {
                            let text = match item {
                                ToolCallContent::Content(c) => {
                                    Self::acp_content_to_text(&c.content)
                                }
                                ToolCallContent::Diff(d) => {
                                    let mut s =
                                        format!("[diff: {}]\n{}", d.path.display(), d.new_text);
                                    if let Some(old) = &d.old_text {
                                        use std::fmt::Write;
                                        let _ = write!(s, "\n(old: {old})");
                                    }
                                    Some(s)
                                }
                                ToolCallContent::Terminal(_) => {
                                    Some("[terminal output]".to_string())
                                }
                                _ => None,
                            };
                            if let Some(text) = text
                                && let Err(e) =
                                    tx.send_async(ProviderEvent::TextDelta { text }).await
                            {
                                debug!(error = %e, "failed to send tool call content");
                            }
                        }
                    }
                }
                if update.fields.title.is_some()
                    || update.fields.status.is_some()
                    || update.fields.kind.is_some()
                    || update.fields.locations.is_some()
                    || update.fields.raw_input.is_some()
                    || update.fields.raw_output.is_some()
                {
                    debug!(
                        tool_call_id = %update.tool_call_id,
                        "received tool call update with non-content fields"
                    );
                }
            }
            SessionUpdate::Plan(_) => {
                debug!(method = "session/update", "received ACP plan update");
            }
            SessionUpdate::AvailableCommandsUpdate(_) => {
                debug!(
                    method = "session/update",
                    "received ACP available commands update"
                );
            }
            SessionUpdate::CurrentModeUpdate(_) => {
                debug!(
                    method = "session/update",
                    "received ACP current mode update"
                );
            }
            SessionUpdate::ConfigOptionUpdate(_) => {
                debug!(
                    method = "session/update",
                    "received ACP config option update"
                );
            }
            SessionUpdate::SessionInfoUpdate(_) => {
                debug!(
                    method = "session/update",
                    "received ACP session info update"
                );
            }
            SessionUpdate::UsageUpdate(UsageUpdate {
                meta: Some(meta), ..
            }) => {
                *usage.lock().await = TokenUsage {
                    input: meta_get_u32(&meta, "cognition.ai/inputTokens"),
                    output: meta_get_u32(&meta, "cognition.ai/outputTokens"),
                    cache_read: meta_get_u32(&meta, "cognition.ai/cachedReadTokens"),
                    cache_creation: meta_get_u32(&meta, "cognition.ai/cachedWriteTokens"),
                };
            }
            _ => {
                debug!(
                    method = "session/update",
                    "received unhandled ACP session update"
                );
            }
        }

        Ok(())
    }
}

fn clamped_u32(value: u64) -> u32 {
    let clamped = value.clamp(0, u64::from(u32::MAX));
    #[allow(clippy::cast_possible_truncation)]
    let n = clamped as u32;
    n
}

fn meta_get_u32(meta: &serde_json::Map<String, Value>, key: &str) -> u32 {
    meta.get(key).and_then(Value::as_u64).map_or(0, clamped_u32)
}

fn json_field_u32(value: &Value, key: &str) -> u32 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .map_or(0, clamped_u32)
}

fn map_stop_reason(reason: AcpStopReason) -> StopReason {
    match reason {
        AcpStopReason::MaxTokens => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    }
}

fn parse_prompt_response(value: &Value) -> (Option<StopReason>, Option<TokenUsage>) {
    let stop_reason = value.get("stopReason").and_then(|v| {
        match serde_json::from_value::<AcpStopReason>(v.clone()) {
            Ok(r) => Some(map_stop_reason(r)),
            Err(e) => {
                debug!(error = %e, "failed to parse stop reason");
                None
            }
        }
    });

    let usage = value.get("usage").map(|u| TokenUsage {
        input: json_field_u32(u, "inputTokens"),
        output: json_field_u32(u, "outputTokens"),
        cache_read: json_field_u32(u, "cachedReadTokens"),
        cache_creation: json_field_u32(u, "cachedWriteTokens"),
    });

    (stop_reason, usage)
}

#[derive(Debug, Clone)]
struct ParsedModelValue {
    value: String,
    base: Vec<String>,
    rank: Option<u32>,
    fast: bool,
}

fn is_thinking_token(token: &str) -> bool {
    matches!(
        token,
        "none"
            | "no-thinking"
            | "minimal"
            | "low"
            | "medium"
            | "high"
            | "xhigh"
            | "max"
            | "adaptive"
            | "lightning"
            | "thinking"
    )
}

fn token_rank(token: &str) -> Option<u32> {
    match token {
        "none" | "no-thinking" | "lightning" => Some(1),
        "minimal" => Some(2),
        "low" => Some(3),
        "medium" => Some(4),
        "high" => Some(5),
        "xhigh" => Some(6),
        "max" | "thinking" => Some(7),
        _ => None,
    }
}

fn name_rank(name: &str) -> Option<u32> {
    let lower = name.to_lowercase();
    if lower.contains("no thinking") || lower.contains("lightning") {
        return Some(1);
    }
    if lower.contains("minimal") {
        return Some(2);
    }
    if lower.contains("xhigh") || lower.contains("x-high") {
        return Some(6);
    }
    if lower.contains("high") {
        return Some(5);
    }
    if lower.contains("max") {
        return Some(7);
    }
    if lower.contains("thinking") {
        return Some(7);
    }
    if lower.contains("low") {
        return Some(3);
    }
    if lower.contains("medium") {
        return Some(4);
    }
    None
}

fn split_model_tokens(value: &str) -> (Vec<String>, Option<String>, bool) {
    let tokens: Vec<&str> = value.split(['-', '_']).collect();
    let mut idx = tokens.len();
    let mut fast = false;
    while idx > 0 {
        match tokens[idx - 1].to_lowercase().as_str() {
            "fast" | "priority" => {
                fast = true;
                idx -= 1;
            }
            "1m" => {
                idx -= 1;
            }
            _ => break,
        }
    }
    let thinking = if idx > 1 {
        let last = tokens[idx - 1].to_lowercase();
        if is_thinking_token(&last) {
            idx -= 1;
            Some(last)
        } else {
            None
        }
    } else {
        None
    };
    let base = tokens[..idx].iter().map(|s| s.to_lowercase()).collect();
    (base, thinking, fast)
}

fn parse_model_id(value: &str) -> ParsedModelValue {
    let (base, thinking, fast) = split_model_tokens(value);
    let rank = thinking.as_deref().and_then(token_rank);
    ParsedModelValue {
        value: value.to_string(),
        base,
        rank,
        fast,
    }
}

fn parse_option(option: &SessionConfigSelectOption) -> ParsedModelValue {
    let value = option.value.to_string();
    let (base, thinking, fast) = split_model_tokens(&value);
    let rank = if let Some(t) = thinking.as_deref() {
        token_rank(t)
    } else {
        name_rank(&option.name).or(Some(2))
    };
    ParsedModelValue {
        value,
        base,
        rank,
        fast,
    }
}

fn flatten_options(options: &SessionConfigSelectOptions) -> Vec<&SessionConfigSelectOption> {
    match options {
        SessionConfigSelectOptions::Ungrouped(opts) => opts.iter().collect(),
        SessionConfigSelectOptions::Grouped(groups) => {
            groups.iter().flat_map(|g| g.options.iter()).collect()
        }
        _ => Vec::new(),
    }
}

fn effort_rank(effort: Effort) -> u32 {
    match effort {
        Effort::Minimal => 2,
        Effort::Low => 3,
        Effort::Medium => 4,
        Effort::High => 5,
        Effort::XHigh => 6,
        Effort::Max => 7,
    }
}

fn desired_rank(thinking: ThinkingConfig, max_budget: Option<u32>) -> Option<u32> {
    match thinking {
        ThinkingConfig::Off => None,
        ThinkingConfig::Adaptive => Some(4),
        ThinkingConfig::Effort(e) => Some(effort_rank(e)),
        ThinkingConfig::Budget(n) => max_budget.map_or(Some(7), |max| {
            Some(effort_rank(Effort::from_budget(n, max)))
        }),
    }
}

fn first_available(parsed: &[ParsedModelValue], default: &str) -> String {
    parsed
        .first()
        .map_or_else(|| default.to_string(), |p| p.value.clone())
}

fn select_by_rank<'a>(
    family: &'a [&'a ParsedModelValue],
    desired: u32,
    prefer_fast: bool,
) -> Option<&'a ParsedModelValue> {
    family
        .iter()
        .max_by_key(|p| {
            let rank = p.rank.unwrap_or_else(|| 7);
            i64::from(if p.fast == prefer_fast { 100 } else { 0 })
                - i64::from(rank.abs_diff(desired))
        })
        .copied()
}

fn select_current_value(
    family: &[&ParsedModelValue],
    current: &ParsedModelValue,
    parsed: &[ParsedModelValue],
) -> String {
    if family.is_empty() {
        return first_available(parsed, &current.value);
    }
    let desired = current.rank.unwrap_or_else(|| 7);
    select_by_rank(family, desired, current.fast).map_or_else(
        || first_available(parsed, &current.value),
        |p| p.value.clone(),
    )
}

fn select_model_value(
    parsed: &[ParsedModelValue],
    model_value: &str,
    thinking: ThinkingConfig,
    max_budget: Option<u32>,
) -> String {
    let current = parse_model_id(model_value);
    let family: Vec<&ParsedModelValue> = parsed.iter().filter(|p| p.base == current.base).collect();

    if matches!(thinking, ThinkingConfig::Off) {
        if let Some(p) = parsed.iter().find(|p| p.value == model_value) {
            return p.value.clone();
        }
        return select_current_value(&family, &current, parsed);
    }

    if matches!(thinking, ThinkingConfig::Adaptive)
        && let Some(p) = family.iter().find(|p| p.rank.is_none())
    {
        return p.value.clone();
    }

    let desired = desired_rank(thinking, max_budget).unwrap_or_else(|| 7);
    select_by_rank(&family, desired, current.fast)
        .map_or_else(|| first_available(parsed, model_value), |p| p.value.clone())
}

fn fallback_models() -> Vec<crate::model::ModelInfo> {
    models()
        .iter()
        .map(|e| crate::model::ModelInfo {
            id: e.prefixes[0].to_string(),
            name: None,
            context_window: Some(e.context_window),
            max_output_tokens: Some(e.max_output_tokens),
            pricing: Some(e.pricing.clone()),
            supports_thinking: None,
            supports_vision: Some(e.vision),
            tier: Some(e.tier),
            is_free: infer_free_status(e.prefixes[0]),
            is_promo: None,
            provider_info: None,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct DevinModelMeta {
    id: &'static str,
    context_window: Option<u32>,
    max_output_tokens: Option<u32>,
    pricing: Option<ModelPricing>,
}

const DEVIN_PRIVATE_MODELS: &[DevinModelMeta] = &[
    // Claude 4.5 family (Cognition private preview)
    DevinModelMeta {
        id: "MODEL_PRIVATE_2",
        context_window: Some(200_000),
        max_output_tokens: Some(64_000),
        pricing: Some(ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_write: 3.75,
            cache_read: 0.30,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_3",
        context_window: Some(200_000),
        max_output_tokens: Some(64_000),
        pricing: Some(ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_write: 3.75,
            cache_read: 0.30,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_11",
        context_window: Some(200_000),
        max_output_tokens: Some(64_000),
        pricing: Some(ModelPricing {
            input: 1.0,
            output: 5.0,
            cache_write: 1.25,
            cache_read: 0.10,
            fast: None,
        }),
    },
    // GPT-5.1 family
    DevinModelMeta {
        id: "MODEL_PRIVATE_12",
        context_window: Some(400_000),
        max_output_tokens: Some(128_000),
        pricing: Some(ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_write: 0.0,
            cache_read: 0.125,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_13",
        context_window: Some(400_000),
        max_output_tokens: Some(128_000),
        pricing: Some(ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_write: 0.0,
            cache_read: 0.125,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_14",
        context_window: Some(400_000),
        max_output_tokens: Some(128_000),
        pricing: Some(ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_write: 0.0,
            cache_read: 0.125,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_15",
        context_window: Some(400_000),
        max_output_tokens: Some(128_000),
        pricing: Some(ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_write: 0.0,
            cache_read: 0.125,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_19",
        context_window: Some(400_000),
        max_output_tokens: Some(128_000),
        pricing: Some(ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_write: 0.0,
            cache_read: 0.125,
            fast: None,
        }),
    },
    // GPT-5.1 Fast family (2x standard GPT-5.1 pricing)
    DevinModelMeta {
        id: "MODEL_PRIVATE_20",
        context_window: Some(400_000),
        max_output_tokens: Some(128_000),
        pricing: Some(ModelPricing {
            input: 2.5,
            output: 20.0,
            cache_write: 0.0,
            cache_read: 0.25,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_21",
        context_window: Some(400_000),
        max_output_tokens: Some(128_000),
        pricing: Some(ModelPricing {
            input: 2.5,
            output: 20.0,
            cache_write: 0.0,
            cache_read: 0.25,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_22",
        context_window: Some(400_000),
        max_output_tokens: Some(128_000),
        pricing: Some(ModelPricing {
            input: 2.5,
            output: 20.0,
            cache_write: 0.0,
            cache_read: 0.25,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_23",
        context_window: Some(400_000),
        max_output_tokens: Some(128_000),
        pricing: Some(ModelPricing {
            input: 2.5,
            output: 20.0,
            cache_write: 0.0,
            cache_read: 0.25,
            fast: None,
        }),
    },
    // xAI Grok
    DevinModelMeta {
        id: "MODEL_PRIVATE_4",
        context_window: None,
        max_output_tokens: None,
        pricing: Some(ModelPricing {
            input: 0.2,
            output: 1.5,
            cache_write: 0.0,
            cache_read: 0.02,
            fast: None,
        }),
    },
    // GPT-5 family (context/output sizes not yet documented)
    DevinModelMeta {
        id: "MODEL_PRIVATE_5",
        context_window: None,
        max_output_tokens: None,
        pricing: Some(ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_write: 0.0,
            cache_read: 0.125,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_6",
        context_window: None,
        max_output_tokens: None,
        pricing: Some(ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_write: 0.0,
            cache_read: 0.125,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_7",
        context_window: None,
        max_output_tokens: None,
        pricing: Some(ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_write: 0.0,
            cache_read: 0.125,
            fast: None,
        }),
    },
    DevinModelMeta {
        id: "MODEL_PRIVATE_8",
        context_window: None,
        max_output_tokens: None,
        pricing: Some(ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_write: 0.0,
            cache_read: 0.125,
            fast: None,
        }),
    },
    // GPT-5.1-Codex Medium
    DevinModelMeta {
        id: "MODEL_PRIVATE_9",
        context_window: Some(400_000),
        max_output_tokens: Some(128_000),
        pricing: Some(ModelPricing {
            input: 0.25,
            output: 2.0,
            cache_write: 0.0,
            cache_read: 0.025,
            fast: None,
        }),
    },
];

fn is_gpt_5_1(lower: &str) -> bool {
    lower.contains("gpt") && (lower.contains("5_1") || lower.contains("5.1"))
}

fn is_gpt_5(lower: &str) -> bool {
    lower.contains("gpt") && lower.contains('5') && !is_gpt_5_1(lower)
}

fn is_claude_4(lower: &str) -> bool {
    lower.contains("claude")
        && (lower.contains("_4") || lower.contains("-4") || lower.contains("4.5"))
}

fn is_claude_4_5(lower: &str) -> bool {
    lower.contains("4.5") || lower.contains("4_5") || lower.contains("4-5")
}

fn infer_context_window(model_id: &str) -> Option<u32> {
    if let Some(meta) = DEVIN_PRIVATE_MODELS.iter().find(|m| m.id == model_id) {
        return meta.context_window;
    }

    let lower = model_id.to_lowercase();
    if lower.contains("swe-1-7") {
        Some(262_144)
    } else if lower.contains("-1m") {
        Some(1_000_000)
    } else if is_claude_4(&lower) {
        Some(200_000)
    } else if is_gpt_5_1(&lower) {
        Some(400_000)
    } else {
        None
    }
}

fn infer_max_output_tokens(model_id: &str) -> Option<u32> {
    if let Some(meta) = DEVIN_PRIVATE_MODELS.iter().find(|m| m.id == model_id) {
        return meta.max_output_tokens;
    }

    let lower = model_id.to_lowercase();
    if is_claude_4(&lower) {
        Some(64_000)
    } else if is_gpt_5_1(&lower) || lower.contains("swe-1-7") {
        Some(128_000)
    } else {
        None
    }
}

fn parse_pricing(value: &serde_json::Value) -> Option<crate::model::ModelPricing> {
    let input = value.get("input_cost_per_million_usd")?.as_f64()?;
    let output = value.get("output_cost_per_million_usd")?.as_f64()?;
    let cache_write = value
        .get("cache_write_cost_per_million_usd")
        .and_then(Value::as_f64)?;
    let cache_read = value
        .get("cache_read_cost_per_million_usd")
        .and_then(Value::as_f64)?;
    Some(crate::model::ModelPricing {
        input,
        output,
        cache_write,
        cache_read,
        fast: None,
    })
}

fn infer_pricing(model_id: &str) -> Option<ModelPricing> {
    if let Some(meta) = DEVIN_PRIVATE_MODELS.iter().find(|m| m.id == model_id) {
        return meta.pricing.clone();
    }

    // For non-private Devin models, only apply family fallbacks we can do accurately.
    // Unknown MODEL_PRIVATE_* entries should not silently get a generic price.
    if model_id.starts_with("MODEL_PRIVATE_") {
        return None;
    }

    let lower = model_id.to_lowercase();
    if is_gpt_5_1(&lower) {
        if lower.contains("codex") && lower.contains("medium") {
            Some(ModelPricing {
                input: 0.25,
                output: 2.0,
                cache_write: 0.0,
                cache_read: 0.025,
                fast: None,
            })
        } else if lower.contains("fast") {
            Some(ModelPricing {
                input: 2.5,
                output: 20.0,
                cache_write: 0.0,
                cache_read: 0.25,
                fast: None,
            })
        } else {
            Some(ModelPricing {
                input: 1.25,
                output: 10.0,
                cache_write: 0.0,
                cache_read: 0.125,
                fast: None,
            })
        }
    } else if is_gpt_5(&lower) {
        Some(ModelPricing {
            input: 1.25,
            output: 10.0,
            cache_write: 0.0,
            cache_read: 0.125,
            fast: None,
        })
    } else if lower.contains("claude") {
        if lower.contains("opus") {
            if is_claude_4_5(&lower) {
                Some(ModelPricing {
                    input: 5.0,
                    output: 25.0,
                    cache_write: 6.25,
                    cache_read: 0.5,
                    fast: None,
                })
            } else {
                Some(ModelPricing {
                    input: 15.0,
                    output: 75.0,
                    cache_write: 18.75,
                    cache_read: 1.5,
                    fast: None,
                })
            }
        } else if lower.contains("haiku") {
            Some(ModelPricing {
                input: 1.0,
                output: 5.0,
                cache_write: 1.25,
                cache_read: 0.1,
                fast: None,
            })
        } else if lower.contains("sonnet") {
            Some(ModelPricing {
                input: 3.0,
                output: 15.0,
                cache_write: 3.75,
                cache_read: 0.3,
                fast: None,
            })
        } else {
            None
        }
    } else if lower.contains("grok") {
        Some(ModelPricing {
            input: 0.2,
            output: 1.5,
            cache_write: 0.0,
            cache_read: 0.02,
            fast: None,
        })
    } else {
        None
    }
}

fn infer_is_free(model_id: &str) -> Option<bool> {
    if model_id.starts_with("MODEL_PRIVATE_") {
        Some(false)
    } else {
        infer_free_status(model_id)
    }
}

fn infer_is_promo(model_id: &str) -> Option<bool> {
    if model_id.starts_with("MODEL_PRIVATE_") {
        Some(true)
    } else {
        None
    }
}

fn infer_free_status(model_id: &str) -> Option<bool> {
    // Standard SWE-1.7 is in a free preview for paid Devin users until 2026-08-08.
    // The Lightning variant is a paid, faster tier and is not part of the preview.
    let lower = model_id.to_lowercase();
    if lower.starts_with("swe-1-7") && !lower.contains("lightning") {
        Some(true)
    } else {
        None
    }
}

pub struct Devin {
    inner: Arc<OnceCell<DevinInner>>,
    api_key: Option<String>,
    command: String,
}

impl Devin {
    fn api_key_from_auth(auth: &ResolvedAuth) -> Option<String> {
        auth.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .and_then(|(_, v)| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
    }

    fn is_safe_command_name(s: &str) -> bool {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    }

    fn command_from_auth(auth: &ResolvedAuth) -> String {
        let candidate = auth
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_COMMAND);

        if Self::is_safe_command_name(candidate) {
            candidate.to_string()
        } else {
            warn!(command = %candidate, "ignoring unsafe devin command override");
            DEFAULT_COMMAND.to_string()
        }
    }

    fn with_api_key(api_key: Option<String>, command: String) -> Self {
        Self {
            inner: Arc::new(OnceCell::new()),
            api_key,
            command,
        }
    }

    pub fn new(_timeouts: super::Timeouts) -> Self {
        let auth = match super::KeyPool::resolve("devin", "DEVIN_API_KEY") {
            Ok(pool) => ResolvedAuth::bearer(pool.current()),
            Err(e) => {
                debug!(
                    error = %e,
                    "no devin API key configured; devin acp will use its own credentials"
                );
                ResolvedAuth {
                    base_url: None,
                    headers: Vec::new(),
                }
            }
        };

        Self::with_api_key(
            Self::api_key_from_auth(&auth),
            Self::command_from_auth(&auth),
        )
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn with_auth(
        auth: &Arc<Mutex<ResolvedAuth>>,
        _timeouts: super::Timeouts,
    ) -> Result<Self, AgentError> {
        let resolved = match auth.lock() {
            Ok(guard) => guard,
            Err(e) => e.into_inner(),
        };

        Ok(Self::with_api_key(
            Self::api_key_from_auth(&resolved),
            Self::command_from_auth(&resolved),
        ))
    }

    async fn get_inner(&self) -> Result<&DevinInner, AgentError> {
        self.inner
            .get_or_try_init(|| async {
                let command = self.command.clone();
                let api_key = self.api_key.clone();
                DevinInner::spawn(&command, api_key.as_deref()).await
            })
            .await
    }

    async fn convert_content_block(&self, block: &crate::types::ContentBlock) -> AcpContentBlock {
        let inner = self.get_inner().await;
        let capabilities = if let Ok(i) = inner {
            i.agent_capabilities.lock().await.clone()
        } else {
            None
        };

        let supports_image = capabilities.is_some_and(|c| c.prompt_capabilities.image);

        match block {
            crate::types::ContentBlock::Text { text } => {
                AcpContentBlock::Text(TextContent::new(text.clone()))
            }
            crate::types::ContentBlock::Thinking { thinking, .. } => {
                AcpContentBlock::Text(TextContent::new(thinking.clone()))
            }
            crate::types::ContentBlock::RedactedThinking { .. } => {
                AcpContentBlock::Text(TextContent::new("[redacted thinking]".to_string()))
            }
            crate::types::ContentBlock::ToolUse { id, name, .. } => {
                AcpContentBlock::Text(TextContent::new(format!("[tool use: {name} id={id}]")))
            }
            crate::types::ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                let label = if *is_error { "error" } else { "result" };
                AcpContentBlock::Text(TextContent::new(format!("[tool {label}: {content}]")))
            }
            crate::types::ContentBlock::Image { source } => {
                if supports_image {
                    AcpContentBlock::Image(ImageContent::new(
                        source.data.to_string(),
                        source.media_type.mime().to_string(),
                    ))
                } else {
                    AcpContentBlock::Text(TextContent::new(
                        "[image not supported by this Devin session]".to_string(),
                    ))
                }
            }
        }
    }
}

impl Provider for Devin {
    fn stream_message<'a>(
        &'a self,
        model: &'a crate::model::Model,
        messages: &'a [Message],
        _system: &'a System,
        _tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            let inner = self.get_inner().await?;

            let session_ref = session_id.ok_or_else(|| AgentError::Config {
                message: "session_id is required for Devin provider".to_string(),
            })?;

            let session_id = inner.get_or_create_session(session_ref).await?;
            inner.apply_model_config(&session_id, model, &opts).await?;

            let last_message = messages.last().ok_or_else(|| AgentError::Config {
                message: "no messages provided".to_string(),
            })?;

            let content: Vec<AcpContentBlock> = {
                let mut blocks = Vec::new();
                for block in &last_message.content {
                    blocks.push(self.convert_content_block(block).await);
                }
                blocks
            };

            let req = PromptRequest::new(session_id, content);

            {
                let mut text = inner.text.lock().await;
                text.clear();
            }
            {
                let mut thinking = inner.thinking.lock().await;
                thinking.clear();
            }
            {
                *inner.usage.lock().await = TokenUsage::default();
            }
            {
                *inner.event_tx.lock().await = Some(event_tx.clone());
            }

            let response: Value = inner
                .send_request::<PromptRequest, Value>("session/prompt", req)
                .await
                .map_err(|e| AgentError::Config {
                    message: format!("session/prompt failed: {e}"),
                })?;

            let (stop_reason, response_usage) = parse_prompt_response(&response);
            let usage = if let Some(u) = response_usage {
                u
            } else {
                *inner.usage.lock().await
            };

            let final_text = inner.text.lock().await.clone();
            let thinking = inner.thinking.lock().await.clone();
            *inner.event_tx.lock().await = None;

            let mut content_blocks = Vec::new();
            if !thinking.is_empty() {
                content_blocks.push(crate::types::ContentBlock::Thinking {
                    thinking,
                    signature: None,
                });
            }
            if !final_text.is_empty() {
                content_blocks.push(crate::types::ContentBlock::Text { text: final_text });
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
                stop_reason,
            })
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        Box::pin(async move {
            let inner = self.get_inner().await?;

            let cwd = match std::env::current_dir() {
                Ok(p) => p,
                Err(_) => PathBuf::from("."),
            };
            let req = NewSessionRequest::new(cwd);
            let response: NewSessionResponse = inner
                .send_request("session/new", req)
                .await
                .map_err(|e| AgentError::Config {
                    message: format!("session/new failed: {e}"),
                })?;

            let models = response
                .config_options
                .map_or_else(fallback_models, |opts| {
                    let mut models = Vec::new();
                    for opt in opts {
                        if opt.category != Some(SessionConfigOptionCategory::Model) {
                            continue;
                        }
                        let SessionConfigKind::Select(select) = opt.kind else {
                            continue;
                        };
                        let options: Vec<_> = match select.options {
                            SessionConfigSelectOptions::Ungrouped(opts) => opts,
                            SessionConfigSelectOptions::Grouped(groups) => {
                                groups.into_iter().flat_map(|g| g.options).collect()
                            }
                            _ => Vec::new(),
                        };
                        for option in options {
                            let value_str = option.value.to_string();
                            let mut info = crate::model::ModelInfo::id_only(value_str.clone());
                            info.name = Some(option.name.clone()).filter(|n| !n.trim().is_empty());
                            info.supports_vision = option
                                .meta
                                .as_ref()
                                .and_then(|m| m.get("cognition.ai/supportsImages"))
                                .and_then(Value::as_bool);
                            if let Some(meta) = &option.meta {
                                info.context_window = meta
                                    .get("cognition.ai/contextWindow")
                                    .and_then(Value::as_u64)
                                    .map(clamped_u32)
                                    .or_else(|| infer_context_window(&value_str));
                                info.max_output_tokens = meta
                                    .get("cognition.ai/maxOutputTokens")
                                    .and_then(Value::as_u64)
                                    .map(clamped_u32)
                                    .or_else(|| infer_max_output_tokens(&value_str));
                                info.pricing = meta
                                    .get("cognition.ai/pricing")
                                    .and_then(parse_pricing)
                                    .or_else(|| infer_pricing(&value_str));
                                info.is_free = meta
                                    .get("cognition.ai/free")
                                    .and_then(Value::as_bool)
                                    .or_else(|| infer_is_free(&value_str));
                                info.is_promo = meta
                                    .get("cognition.ai/promo")
                                    .and_then(Value::as_bool)
                                    .or_else(|| infer_is_promo(&value_str));
                            } else {
                                info.context_window = infer_context_window(&value_str);
                                info.max_output_tokens = infer_max_output_tokens(&value_str);
                                info.pricing = infer_pricing(&value_str);
                                info.is_free = infer_is_free(&value_str);
                                info.is_promo = infer_is_promo(&value_str);
                            }
                            models.push(info);
                        }
                    }
                    if models.is_empty() {
                        fallback_models()
                    } else {
                        models
                    }
                });

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

    fn opt(value: &str, name: &str) -> SessionConfigSelectOption {
        SessionConfigSelectOption::new(value.to_string(), name)
    }

    #[test]
    fn swe_max_off_normalizes_to_swe_base() {
        let parsed = vec![
            parse_option(&opt("swe-1-7", "SWE-1.7 Max")),
            parse_option(&opt("swe-1-7-medium", "SWE-1.7 Medium")),
            parse_option(&opt("swe-1-7-lightning", "SWE-1.7 Lightning")),
        ];
        assert_eq!(
            select_model_value(&parsed, "swe-1-7-max", ThinkingConfig::Off, None),
            "swe-1-7"
        );
    }

    #[test]
    fn swe_off_exact_honors_selected_variant() {
        let parsed = vec![
            parse_option(&opt("swe-1-7", "SWE-1.7 Max")),
            parse_option(&opt("swe-1-7-lightning", "SWE-1.7 Lightning")),
        ];
        assert_eq!(
            select_model_value(&parsed, "swe-1-7-lightning", ThinkingConfig::Off, None),
            "swe-1-7-lightning"
        );
    }

    #[test]
    fn swe_low_picks_medium_when_no_low_variant() {
        let parsed = vec![
            parse_option(&opt("swe-1-7", "SWE-1.7 Max")),
            parse_option(&opt("swe-1-7-medium", "SWE-1.7 Medium")),
            parse_option(&opt("swe-1-7-lightning", "SWE-1.7 Lightning")),
        ];
        assert_eq!(
            select_model_value(
                &parsed,
                "swe-1-7",
                ThinkingConfig::Effort(Effort::Low),
                None
            ),
            "swe-1-7-medium"
        );
    }

    #[test]
    fn swe_max_picks_max_variant() {
        let parsed = vec![
            parse_option(&opt("swe-1-7", "SWE-1.7 Max")),
            parse_option(&opt("swe-1-7-medium", "SWE-1.7 Medium")),
            parse_option(&opt("swe-1-7-lightning", "SWE-1.7 Lightning")),
        ];
        assert_eq!(
            select_model_value(
                &parsed,
                "swe-1-7-medium",
                ThinkingConfig::Effort(Effort::Max),
                None
            ),
            "swe-1-7"
        );
    }

    #[test]
    fn binary_thinking_family_max_selects_thinking() {
        let parsed = vec![
            parse_option(&opt("claude-opus-4-6", "Claude Opus 4.6")),
            parse_option(&opt("claude-opus-4-6-thinking", "Claude Opus 4.6 Thinking")),
        ];
        assert_eq!(
            select_model_value(
                &parsed,
                "claude-opus-4-6",
                ThinkingConfig::Effort(Effort::Max),
                None
            ),
            "claude-opus-4-6-thinking"
        );
    }

    #[test]
    fn binary_thinking_family_low_keeps_non_thinking() {
        let parsed = vec![
            parse_option(&opt("claude-opus-4-6", "Claude Opus 4.6")),
            parse_option(&opt("claude-opus-4-6-thinking", "Claude Opus 4.6 Thinking")),
        ];
        assert_eq!(
            select_model_value(
                &parsed,
                "claude-opus-4-6",
                ThinkingConfig::Effort(Effort::Low),
                None
            ),
            "claude-opus-4-6"
        );
    }

    #[test]
    fn adaptive_value_selects_itself() {
        let parsed = vec![parse_option(&opt("adaptive", "Adaptive"))];
        assert_eq!(
            select_model_value(&parsed, "adaptive", ThinkingConfig::Adaptive, None),
            "adaptive"
        );
    }

    #[test]
    fn adaptive_without_adaptive_option_falls_back_to_medium() {
        let parsed = vec![
            parse_option(&opt("swe-1-7", "SWE-1.7 Max")),
            parse_option(&opt("swe-1-7-medium", "SWE-1.7 Medium")),
            parse_option(&opt("swe-1-7-lightning", "SWE-1.7 Lightning")),
        ];
        assert_eq!(
            select_model_value(&parsed, "swe-1-7", ThinkingConfig::Adaptive, None),
            "swe-1-7-medium"
        );
    }

    #[test]
    fn thinking_fast_suffix_is_preserved() {
        let parsed = vec![
            parse_option(&opt("claude-opus-5-medium", "Claude Opus 5 Medium")),
            parse_option(&opt(
                "claude-opus-5-medium-fast",
                "Claude Opus 5 Medium Fast",
            )),
            parse_option(&opt("claude-opus-5-low", "Claude Opus 5 Low")),
            parse_option(&opt("claude-opus-5-low-fast", "Claude Opus 5 Low Fast")),
        ];
        assert_eq!(
            select_model_value(
                &parsed,
                "claude-opus-5-low-fast",
                ThinkingConfig::Effort(Effort::Medium),
                None
            ),
            "claude-opus-5-medium-fast"
        );
    }

    #[test]
    fn is_safe_command_name_accepts_simple_names() {
        assert!(Devin::is_safe_command_name("devin"));
        assert!(Devin::is_safe_command_name("devin2"));
        assert!(Devin::is_safe_command_name("my-devin.cli_2"));
    }

    #[test]
    fn is_safe_command_name_rejects_unsafe_inputs() {
        assert!(!Devin::is_safe_command_name(""));
        assert!(!Devin::is_safe_command_name("devin acp"));
        assert!(!Devin::is_safe_command_name("/tmp/devin"));
        assert!(!Devin::is_safe_command_name("https://example.com"));
        assert!(!Devin::is_safe_command_name("dévïn"));
    }

    #[test]
    fn command_from_auth_defaults_to_devin() {
        let auth = ResolvedAuth {
            base_url: None,
            headers: Vec::new(),
        };
        assert_eq!(Devin::command_from_auth(&auth), "devin");
    }

    #[test]
    fn command_from_auth_uses_safe_base_url() {
        let auth = ResolvedAuth {
            base_url: Some("devin2".to_string()),
            headers: Vec::new(),
        };
        assert_eq!(Devin::command_from_auth(&auth), "devin2");
    }

    #[test]
    fn command_from_auth_falls_back_for_unsafe_base_url() {
        let auth = ResolvedAuth {
            base_url: Some("/tmp/evil".to_string()),
            headers: Vec::new(),
        };
        assert_eq!(Devin::command_from_auth(&auth), "devin");
    }
}
