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
};

// Non-codex models OpenAI offers for subscription usage via the Coding Plan.
// Codex models are matched by their `-codex` substring in
// `coding_plan_context_window`, so they never need listing here.
pub(crate) const PLAN_MODELS: &[&str] = &[
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
const SESSION_STATE_TTL: Duration = Duration::from_secs(3600); // 1 hour

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
        state.last_response_id = None;
        state.last_message_count = 0;
        state.tools_hash = Some(tools_hash.to_string());
        state.messages_hash = None;
    }

    if state.last_message_count > 0 {
        let current_hash = hash_messages(&messages[..state.last_message_count]);
        if state.messages_hash != Some(current_hash) {
            state.last_response_id = None;
            state.last_message_count = 0;
            state.messages_hash = Some(current_hash);
        }
    }

    if let Some(prev_id) = state.last_response_id.clone() {
        if messages.len() > state.last_message_count + 1 {
            return (Some(prev_id), &messages[state.last_message_count + 1..]);
        }
        state.last_response_id = None;
        state.last_message_count = 0;
    }

    (None, &messages[state.last_message_count..])
}

fn record_in_state(
    state: &mut OpenAiSessionState,
    response_id: Option<String>,
    tools_hash: &str,
    messages: &[Message],
) {
    if let Some(rid) = response_id {
        state.last_response_id = Some(rid);
        state.last_message_count = messages.len();
        state.tools_hash = Some(tools_hash.to_string());
        state.messages_hash = Some(hash_messages(messages));
    }
}

fn is_codex_model(model_id: &str) -> bool {
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
    Some(if model_id.starts_with("gpt-5.6-") {
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
        let compat = OpenAiCompatProvider::new(&CONFIG, timeouts);
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
    ) -> Self {
        Self {
            compat: OpenAiCompatProvider::new(&CONFIG, timeouts),
            auth,
            storage: None,
            system_prefix: None,
            session_state: Arc::new(Mutex::new(HashMap::new())),
        }
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
        session_id: Option<&'a SessionRef>,
        tools_hash: &str,
        messages: &'a [Message],
    ) -> (Option<String>, &'a [Message]) {
        let mut state = self.session_state.lock().unwrap();

        // Opportunistically evict stale sessions
        let now = Instant::now();
        state.retain(|_, s| now.duration_since(s.last_used) < SESSION_STATE_TTL);

        if let Some(sid) = session_id {
            let session_state = state.entry(sid.clone()).or_default();
            session_state.last_used = now;
            incremental_for_state(session_state, tools_hash, messages)
        } else {
            (None, messages)
        }
    }

    fn record_response(
        &self,
        session_id: Option<&SessionRef>,
        response_id: Option<String>,
        tools_hash: &str,
        messages: &[Message],
    ) {
        if let Some(sid) = session_id {
            let mut state = self.session_state.lock().unwrap();
            let session_state = state.entry(sid.clone()).or_default();
            session_state.last_used = Instant::now();
            record_in_state(session_state, response_id, tools_hash, messages);
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
            let system = super::super::with_prefix(&self.system_prefix, system, &mut buf);

            let tools_hash = serde_json::to_string(tools).unwrap_or_default();
            let (previous_response_id, incremental_messages) =
                self.prepare_request(session_id, &tools_hash, messages);
            let prompt_cache_key = session_id.map(|s| s.to_string());

            if super::websocket::is_websocket_model(&model.id) {
                let stream_timeout = self.compat.stream_timeout();
                return self
                    .with_oauth_retry(|| async {
                        let auth = if is_codex_model(&model.id) {
                            self.codex_auth()?
                        } else {
                            self.current_auth()
                        };
                        let body = super::responses::build_body(
                            model,
                            incremental_messages,
                            system,
                            tools,
                            previous_response_id.as_deref(),
                            prompt_cache_key.as_deref(),
                        );
                        let (response_id, resp) =
                            super::websocket::stream_message(body, event_tx, &auth, stream_timeout)
                                .await?;
                        self.record_response(session_id, response_id, &tools_hash, messages);
                        Ok(resp)
                    })
                    .await;
            }

            if is_codex_model(&model.id) {
                let stream_timeout = self.compat.stream_timeout();
                return self
                    .with_oauth_retry(|| async {
                        let codex_auth = self.codex_auth()?;

                        let body = super::responses::build_body(
                            model,
                            incremental_messages,
                            system,
                            tools,
                            previous_response_id.as_deref(),
                            prompt_cache_key.as_deref(),
                        );

                        let (response_id, resp) = super::responses::do_stream(
                            self.compat.client(),
                            model,
                            &body,
                            event_tx,
                            &codex_auth,
                            stream_timeout,
                        )
                        .await?;

                        self.record_response(session_id, response_id, &tools_hash, messages);
                        Ok(resp)
                    })
                    .await;
            }

            let mut body = self.compat.build_body(model, messages, system, tools);
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

    fn tool_result(id: &str, output: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.into(),
                content: output.into(),
                is_error: false,
            }],
            ..Default::default()
        }
    }

    #[test_case("gpt-5.6-luna")]
    #[test_case("gpt-5.6-terra")]
    #[test_case("gpt-5.6-sol")]
    fn gpt_5_6_models_use_coding_plan_and_websocket(model_id: &str) {
        assert!(is_codex_model(model_id));
        assert!(crate::providers::openai::websocket::is_websocket_model(
            model_id
        ));
    }

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
    #[test_case("gpt-5.6-codex", Some(272_000) ; "codex_models_use_http")]
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
