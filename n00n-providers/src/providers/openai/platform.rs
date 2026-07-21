use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flume::Sender;
use n00n_storage::StateDir;
use n00n_storage::id::SessionRef;
use serde_json::Value;
use tracing::{debug, warn};

use crate::model::Model;
use crate::provider::{BoxFuture, Provider};
use crate::{AgentError, Message, ProviderEvent, RequestOptions, StreamResponse, dialect};

use super::auth;
use crate::providers::ResolvedAuth;
use crate::providers::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};

static CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
    api_key_env: "OPENAI_API_KEY",
    base_url: "https://api.openai.com/v1",
    max_tokens_field: "max_completion_tokens",
    include_stream_usage: true,
    provider_name: "OpenAI",
    supports_prompt_cache_key: true,
    supports_prompt_cache_breakpoint: true,
};

// Non-codex models OpenAI offers for subscription usage via the Coding Plan.
// Codex models are matched by their `-codex` substring in
// `coding_plan_context_window`, so they never need listing here.
pub(crate) const PLAN_MODELS: &[&str] = &[
    "gpt-5.6",
    "gpt-5.6-luna",
    "gpt-5.6-terra",
    "gpt-5.6-sol",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.2",
];

const CODEX_PLAN_CONTEXT_WINDOW: u32 = 272_000;
const GPT_5_6_PLAN_CONTEXT_WINDOW: u32 = 372_000;
const SESSION_STATE_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug)]
struct OpenAiSessionState {
    last_response_id: Option<String>,
    last_message_count: usize,
    tools_hash: Option<String>,
    messages_hash: Option<u64>,
    last_used: Instant,
}

impl Default for OpenAiSessionState {
    fn default() -> Self {
        Self {
            last_response_id: None,
            last_message_count: 0,
            tools_hash: None,
            messages_hash: None,
            last_used: Instant::now(),
        }
    }
}

fn hash_messages(messages: &[Message]) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(
        serde_json::to_string(messages)
            .unwrap_or_default()
            .as_bytes(),
    );
    hasher.finish()
}

fn incremental_for_state<'a>(
    state: &mut OpenAiSessionState,
    tools_hash: &str,
    messages: &'a [Message],
) -> (Option<String>, &'a [Message]) {
    if state.tools_hash.as_deref() != Some(tools_hash) || messages.len() < state.last_message_count
    {
        *state = OpenAiSessionState {
            tools_hash: Some(tools_hash.to_owned()),
            ..Default::default()
        };
    }

    if state.last_message_count > 0 {
        let current_hash = hash_messages(&messages[..state.last_message_count]);
        if state.messages_hash != Some(current_hash) {
            *state = OpenAiSessionState {
                tools_hash: Some(tools_hash.to_owned()),
                messages_hash: Some(current_hash),
                ..Default::default()
            };
        }
    }

    if let Some(previous_response_id) = state.last_response_id.clone() {
        if messages.len() > state.last_message_count + 1 {
            return (
                Some(previous_response_id),
                &messages[state.last_message_count + 1..],
            );
        }
        *state = OpenAiSessionState {
            tools_hash: Some(tools_hash.to_owned()),
            ..Default::default()
        };
    }

    (None, &messages[state.last_message_count..])
}

fn record_in_state(
    state: &mut OpenAiSessionState,
    response_id: Option<String>,
    tools_hash: &str,
    messages: &[Message],
) {
    if let Some(response_id) = response_id {
        *state = OpenAiSessionState {
            last_response_id: Some(response_id),
            last_message_count: messages.len(),
            tools_hash: Some(tools_hash.to_owned()),
            messages_hash: Some(hash_messages(messages)),
            last_used: Instant::now(),
        };
    }
}

pub(crate) fn is_codex_model(model_id: &str) -> bool {
    coding_plan_context_window(model_id).is_some()
}

// Codex models match by substring so future releases route without a registry
// edit; the named non-codex plans match exactly to avoid catching near-misses
// like `gpt-5.6-terra-preview`.
fn coding_plan_context_window(model_id: &str) -> Option<u32> {
    if model_id.contains("-codex") {
        return Some(CODEX_PLAN_CONTEXT_WINDOW);
    }
    if !PLAN_MODELS.contains(&model_id) {
        return None;
    }
    Some(if model_id.starts_with("gpt-5.6") {
        GPT_5_6_PLAN_CONTEXT_WINDOW
    } else {
        CODEX_PLAN_CONTEXT_WINDOW
    })
}

pub struct OpenAi {
    compat: OpenAiCompatProvider,
    auth: Arc<Mutex<ResolvedAuth>>,
    storage: Option<StateDir>,
    system_prefix: Option<String>,
    session_state: Arc<Mutex<HashMap<SessionRef, OpenAiSessionState>>>,
}

impl OpenAi {
    pub fn new(timeouts: crate::providers::Timeouts) -> Result<Self, AgentError> {
        let storage = StateDir::resolve()?;
        let resolved = auth::resolve(&storage)?;
        let compat = OpenAiCompatProvider::new(&CONFIG, timeouts)?;
        Ok(Self {
            compat,
            auth: Arc::new(Mutex::new(resolved)),
            storage: Some(storage),
            system_prefix: None,
            session_state: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub(crate) fn with_auth(
        auth: Arc<Mutex<ResolvedAuth>>,
        timeouts: crate::providers::Timeouts,
    ) -> Result<Self, AgentError> {
        Ok(Self {
            compat: OpenAiCompatProvider::new(&CONFIG, timeouts)?,
            auth,
            storage: None,
            system_prefix: None,
            session_state: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub(crate) fn with_system_prefix(mut self, prefix: Option<String>) -> Self {
        self.system_prefix = prefix;
        self
    }

    fn current_auth(&self) -> ResolvedAuth {
        self.auth.lock().unwrap().clone()
    }

    fn is_oauth(&self) -> bool {
        self.storage.as_ref().is_some_and(auth::is_oauth)
    }

    async fn refresh_oauth(&self) -> Result<(), AgentError> {
        let storage = self.storage.clone().ok_or_else(|| AgentError::Config {
            message: "OAuth refresh not available for externally-managed auth".into(),
        })?;
        let resolved = smol::unblock(move || {
            let tokens =
                n00n_storage::auth::load_tokens(&storage, auth::PROVIDER).ok_or_else(|| {
                    AgentError::Api {
                        status: 401,
                        message: "OpenAI OAuth tokens not found on disk".into(),
                    }
                })?;
            match auth::refresh_tokens(&tokens) {
                Ok(fresh) => {
                    n00n_storage::auth::save_tokens(&storage, auth::PROVIDER, &fresh)?;
                    Ok(auth::build_oauth_resolved(&fresh))
                }
                Err(e) => {
                    warn!(error = %e, "OpenAI OAuth refresh failed, clearing stale tokens");
                    let _ = n00n_storage::auth::delete_tokens(&storage, auth::PROVIDER);
                    Err(e)
                }
            }
        })
        .await?;
        *self.auth.lock().unwrap() = resolved;
        debug!("refreshed OpenAI OAuth token");
        Ok(())
    }

    async fn with_oauth_retry<T, F, Fut>(&self, f: F) -> Result<T, AgentError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, AgentError>>,
    {
        let result = f().await;
        if self.is_oauth()
            && matches!(&result, Err(e) if e.is_auth_error())
            && self.refresh_oauth().await.is_ok()
        {
            return f().await;
        }
        result
    }

    fn codex_auth(&self) -> Result<ResolvedAuth, AgentError> {
        // Prefer OAuth tokens for the ChatGPT Coding Plan backend.
        if let Some(storage) = self.storage.as_ref()
            && let Some(tokens) = n00n_storage::auth::load_tokens(storage, auth::PROVIDER)
        {
            return Ok(auth::build_coding_plan_resolved(&tokens));
        }
        // Fall back to standard API key via the Responses API.
        let mut auth = self.current_auth();
        if auth.base_url.is_none() {
            auth.base_url = Some(CONFIG.base_url.into());
        }
        Ok(auth)
    }

    fn prepare_request<'a>(
        &self,
        session_id: Option<&SessionRef>,
        tools_hash: &str,
        messages: &'a [Message],
    ) -> (Option<String>, &'a [Message]) {
        let Some(session_id) = session_id else {
            return (None, messages);
        };
        let mut states = self.session_state.lock().unwrap();
        let now = Instant::now();
        states.retain(|_, state| now.duration_since(state.last_used) < SESSION_STATE_TTL);
        let state = states.entry(session_id.clone()).or_default();
        state.last_used = now;
        incremental_for_state(state, tools_hash, messages)
    }

    fn record_response(
        &self,
        session_id: Option<&SessionRef>,
        response_id: Option<String>,
        tools_hash: &str,
        messages: &[Message],
    ) {
        if let Some(session_id) = session_id {
            let mut states = self.session_state.lock().unwrap();
            let state = states.entry(session_id.clone()).or_default();
            state.last_used = Instant::now();
            record_in_state(state, response_id, tools_hash, messages);
        }
    }
}

impl Provider for OpenAi {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            let mut buf = String::new();
            let system = super::super::with_prefix(self.system_prefix.as_deref(), system, &mut buf);

            if is_codex_model(&model.id) {
                let tools_hash = serde_json::to_string(tools).unwrap_or_default();
                let (previous_response_id, incremental_messages) =
                    self.prepare_request(session_id, &tools_hash, messages);
                let prompt_cache_key = session_id.map(ToString::to_string);
                let stream_timeout = self.compat.stream_timeout();
                let body = super::websocket::build_request_body(
                    model,
                    incremental_messages,
                    system,
                    tools,
                    opts,
                    previous_response_id.as_deref(),
                    prompt_cache_key.as_deref(),
                );
                return self
                    .with_oauth_retry(|| async {
                        let auth = self.codex_auth()?;
                        let (response_id, response) = super::websocket::stream_message(
                            &body,
                            event_tx,
                            &auth,
                            stream_timeout,
                        )
                        .await?;
                        self.record_response(session_id, response_id, &tools_hash, messages);
                        Ok(response)
                    })
                    .await;
            }

            let mut body = self.compat.build_body_with_session(
                model,
                messages,
                system,
                tools,
                session_id.map(|s| s.as_str()),
            );
            opts.thinking
                .apply_reasoning_effort(&mut body, &dialect::STANDARD, model);
            self.with_oauth_retry(|| async {
                let auth = self.current_auth();
                self.compat
                    .do_stream(model, &[], &body, event_tx, &auth)
                    .await
            })
            .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        Box::pin(async {
            if self.is_oauth() {
                let models = super::models()
                    .iter()
                    .flat_map(|e| e.prefixes.iter())
                    .filter(|id| is_codex_model(id))
                    .map(|&s| crate::model::ModelInfo::id_only(s.to_string()))
                    .collect();
                return Ok(models);
            }
            self.with_oauth_retry(|| async {
                let auth = self.current_auth();
                self.compat.do_list_models(&auth).await
            })
            .await
        })
    }

    fn refresh_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async {
            if self.is_oauth() {
                self.refresh_oauth().await
            } else {
                Ok(())
            }
        })
    }

    fn reload_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async {
            let Some(storage) = self.storage.clone() else {
                return Ok(());
            };
            let resolved = smol::unblock(move || auth::resolve(&storage)).await?;
            *self.auth.lock().unwrap() = resolved;
            debug!("reloaded OpenAI auth from storage");
            Ok(())
        })
    }

    fn adjust_model(&self, model: &mut Model) {
        if self.is_oauth()
            && let Some(context_window) = coding_plan_context_window(&model.id)
        {
            model.context_window = model.context_window.min(context_window);
        }
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;
    use crate::{ContentBlock, Role};

    const TOOLS_HASH: &str = "[]";

    fn assistant(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
            ..Default::default()
        }
    }

    fn tool_result(tool_use_id: &str, content: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: content.into(),
                is_error: false,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn incremental_request_uses_previous_response_and_new_messages() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(&mut state, Some("resp_1".into()), "[]", &first);
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (previous_response_id, incremental_messages) =
            incremental_for_state(&mut state, "[]", &second);

        assert_eq!(previous_response_id.as_deref(), Some("resp_1"));
        assert_eq!(incremental_messages.len(), 1);
        assert!(matches!(
            &incremental_messages[0].content[0],
            ContentBlock::Text { text } if text == "again"
        ));
    }

    #[test]
    fn incremental_request_keeps_only_tool_results_after_tool_calls() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("run".into())];
        record_in_state(&mut state, Some("resp_1".into()), "[]", &first);
        let second = vec![
            Message::user("run".into()),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "one"}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "result".into(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];

        let (previous_response_id, incremental_messages) =
            incremental_for_state(&mut state, "[]", &second);

        assert_eq!(previous_response_id.as_deref(), Some("resp_1"));
        assert_eq!(incremental_messages.len(), 1);
        assert!(matches!(
            &incremental_messages[0].content[0],
            ContentBlock::ToolResult { tool_use_id, content, .. }
                if tool_use_id == "call_1" && content == "result"
        ));
    }
    #[test]
    fn incremental_request_resets_when_tools_change() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(&mut state, Some("resp_1".into()), "[]", &first);
        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];

        let (previous_response_id, incremental_messages) =
            incremental_for_state(&mut state, "[\"new\"]", &second);

        assert!(previous_response_id.is_none());
        assert_eq!(incremental_messages.len(), second.len());
    }

    #[test_case("gpt-5.6")]
    #[test_case("gpt-5.6-luna")]
    #[test_case("gpt-5.6-terra")]
    #[test_case("gpt-5.6-sol")]
    #[test_case("gpt-5.5")]
    #[test_case("gpt-5.3-codex")]
    fn coding_plan_models_use_websocket(model_id: &str) {
        assert!(is_codex_model(model_id));
    }

    #[test_case("gpt-5.6", Some(372_000))]
    #[test_case("gpt-5.6-luna", Some(372_000))]
    #[test_case("gpt-5.6-terra", Some(372_000))]
    #[test_case("gpt-5.6-sol", Some(372_000))]
    #[test_case("gpt-5.5", Some(272_000))]
    #[test_case("gpt-5.4", Some(272_000))]
    #[test_case("gpt-5.2", Some(272_000))]
    #[test_case("gpt-5.3-codex", Some(272_000))]
    #[test_case("gpt-5.7-codex", Some(272_000) ; "unlisted codex model still routes")]
    #[test_case("gpt-5.5-preview", None ; "non_plan_5_5_preview_rejected")]
    #[test_case("gpt-5.6-terra-preview", None ; "non_plan_5_6_preview_rejected")]
    #[test_case("gpt-5.6-codex", Some(272_000) ; "codex_model")]
    #[test_case("gpt-5.4-nano", None ; "non_plan_5_4_nano_rejected")]
    fn coding_plan_context_window_resolves_plan_models(model_id: &str, expected: Option<u32>) {
        assert_eq!(coding_plan_context_window(model_id), expected);
    }

    #[test]
    fn incremental_first_turn_sends_full_messages() {
        let mut state = OpenAiSessionState::default();
        let messages = vec![Message::user("hello".into())];
        let (prev, inc) = incremental_for_state(&mut state, TOOLS_HASH, &messages);

        assert!(prev.is_none());
        assert_eq!(inc.len(), 1);
        assert!(matches!(inc[0].role, Role::User));
    }

    #[test]
    fn incremental_second_turn_skips_assistant_message() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        let (prev, _inc) = incremental_for_state(&mut state, TOOLS_HASH, &first);
        assert!(prev.is_none());

        record_in_state(&mut state, Some("resp_1".into()), TOOLS_HASH, &first);

        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];
        let (prev, inc) = incremental_for_state(&mut state, TOOLS_HASH, &second);

        assert_eq!(prev.as_deref(), Some("resp_1"));
        assert_eq!(inc.len(), 1);
        assert!(matches!(inc[0].role, Role::User));
        assert!(matches!(
            &inc[0].content[0],
            ContentBlock::Text { text } if text == "again"
        ));
    }

    #[test]
    fn incremental_tool_loop_skips_assistant_tool_calls() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("run".into())];
        let (prev, _inc) = incremental_for_state(&mut state, TOOLS_HASH, &first);
        assert!(prev.is_none());
        record_in_state(&mut state, Some("resp_1".into()), TOOLS_HASH, &first);

        let second = vec![
            Message::user("run".into()),
            assistant("ok"),
            tool_result("call_1", "result"),
            Message::user("next".into()),
        ];
        let (prev, inc) = incremental_for_state(&mut state, TOOLS_HASH, &second);

        assert_eq!(prev.as_deref(), Some("resp_1"));
        assert_eq!(inc.len(), 2);
        assert!(matches!(inc[0].role, Role::User));
        assert!(matches!(inc[1].role, Role::User));
        assert!(matches!(
            &inc[1].content[0],
            ContentBlock::Text { text } if text == "next"
        ));
    }

    #[test]
    fn incremental_tools_change_resets_state() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(&mut state, Some("resp_1".into()), TOOLS_HASH, &first);

        let second = vec![
            Message::user("hello".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];
        let (prev, inc) = incremental_for_state(&mut state, "[\"new\"]", &second);

        assert!(prev.is_none());
        assert_eq!(inc.len(), 3);
        assert_eq!(state.tools_hash, Some("[\"new\"]".to_string()));
    }

    #[test]
    fn incremental_messages_shrink_resets_state() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("a".into()), Message::user("b".into())];
        record_in_state(&mut state, Some("resp_1".into()), TOOLS_HASH, &first);

        let second = vec![Message::user("a".into())];
        let (prev, inc) = incremental_for_state(&mut state, TOOLS_HASH, &second);

        assert!(prev.is_none());
        assert_eq!(inc.len(), 1);
    }

    #[test]
    fn incremental_prefix_change_resets_state() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(&mut state, Some("resp_1".into()), TOOLS_HASH, &first);

        let second = vec![
            Message::user("different".into()),
            assistant("hi"),
            Message::user("again".into()),
        ];
        let (prev, inc) = incremental_for_state(&mut state, TOOLS_HASH, &second);

        assert!(prev.is_none());
        assert_eq!(inc.len(), 3);
    }

    #[test]
    fn record_response_without_id_leaves_state_unchanged() {
        let mut state = OpenAiSessionState::default();
        let first = vec![Message::user("hello".into())];
        record_in_state(&mut state, Some("resp_1".into()), TOOLS_HASH, &first);

        let second = vec![Message::user("again".into())];
        record_in_state(&mut state, None, TOOLS_HASH, &second);

        assert_eq!(state.last_response_id.as_deref(), Some("resp_1"));
        assert_eq!(state.last_message_count, 1);
    }
}
